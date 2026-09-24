//! The mailbox guard: a WebSocket proxy on loopback between magic-wormhole
//! and the mailbox server.
//!
//! The library talks to the mailbox itself and gives us no hook into what
//! it reads, and what it reads can hang or crash it (the upstream findings
//! in the module docs): a `welcome` demanding hashcash of any difficulty is
//! minted in a synchronous loop that never yields, a peer message with a
//! non-numeric phase reaches a `todo!()`, a body shorter than a nonce
//! reaches an unchecked `split_at`, and nothing bounds message sizes (64 MiB
//! each, tungstenite's default) or how many are queued. The default mailbox
//! is plain `ws://`, so "the server" includes anyone on the path.
//!
//! So the library is pointed at `ws://127.0.0.1:<port>/<token>` instead,
//! and this guard, which owns the real connection, forwards only messages
//! that are within [`MAX_WS_MESSAGE_BYTES`], within the connection's
//! budget, and shaped so that none of those paths is reachable. Anything
//! else closes both sides; the library then fails with an I/O error and the
//! session reports the guard's verdict instead.
//!
//! The guard also carries `wss://` (F-MW4) over rustls and tokio-rustls,
//! the stack LocalSend already uses, so the library needs no TLS of its
//! own.
//!
//! The listening socket is bound to 127.0.0.1 only, accepts at most
//! [`MAX_LOCAL_ATTEMPTS`] connections, and upgrades only the one whose
//! request path is the random token: another local process can at worst
//! make the transfer fail, never ride on it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use async_tungstenite::tungstenite::http::StatusCode;
use async_tungstenite::tungstenite::protocol::WebSocketConfig;
use async_tungstenite::tungstenite::{Error as WsError, Message};
use futures_core::Stream;
use serde_json::Value;
use sukkula_core::hex;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::client::TlsStream;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};
use tokio_util::either::Either;
use tokio_util::sync::{CancellationToken, DropGuard};
use url::Url;

use super::Tuning;
use crate::api::{ErrorCode, ErrorInfo};

/// Largest WebSocket message either way. A 64 KiB text, JSON-escaped at
/// worst (six bytes a character), encrypted and hex-encoded, still fits.
pub(crate) const MAX_WS_MESSAGE_BYTES: usize = 1024 * 1024;

/// Most messages the server may send over one connection. A whole
/// transfer takes about thirty, counting acks and the echoes of our own.
pub(crate) const MAX_SERVER_MESSAGES: usize = 128;

/// Most bytes the server may send over one connection.
pub(crate) const MAX_SERVER_BYTES: usize = 4 * 1024 * 1024;

/// Hardest hashcash a server may ask for. The library mints it in a
/// synchronous loop; 20 bits is about a million SHA-1s, well under a
/// second on the phone. No public server asks for any.
pub(crate) const MAX_HASHCASH_BITS: u64 = 20;

/// Longest hashcash resource string accepted.
const MAX_HASHCASH_RESOURCE_BYTES: usize = 256;

/// Longest `side` accepted in a server message. The reference clients use
/// ten hex digits.
const MAX_SIDE_BYTES: usize = 64;

/// Longest numeric phase: any `u64`.
const MAX_PHASE_DIGITS: usize = 20;

/// Shortest encrypted body, in hex: a 24-byte nonce and a 16-byte tag.
const MIN_SEALED_BODY_HEX: usize = 2 * (24 + 16);

/// Longest PAKE body, in hex. The SPAKE2 message is 33 bytes, wrapped in a
/// small JSON object.
const MAX_PAKE_BODY_HEX: usize = 1024;

/// Local connections tried before the guard gives up on the library.
const MAX_LOCAL_ATTEMPTS: usize = 4;

/// Bound on one local WebSocket upgrade.
const LOCAL_UPGRADE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on sending a close frame.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// The library's side.
type Local = WebSocketStream<Compat<TcpStream>>;

/// The server's side, plain or TLS.
type Upstream = WebSocketStream<Compat<Either<TcpStream, TlsStream<TcpStream>>>>;

/// A running guard. Dropping it stops it.
pub(crate) struct MailboxGuard {
    /// What the library connects to.
    pub url: String,
    verdict: Arc<OnceLock<ErrorInfo>>,
    _stop: DropGuard,
}

impl MailboxGuard {
    /// Why the guard closed the connection, if it did so on purpose.
    pub(crate) fn verdict(&self) -> Option<ErrorInfo> {
        self.verdict.get().cloned()
    }
}

/// Starts a guard in front of `upstream`. It stops when `parent` is
/// cancelled or the guard is dropped.
///
/// # Errors
///
/// No loopback port or no randomness.
pub(crate) async fn open(
    upstream: &Url,
    tuning: &Tuning,
    parent: &CancellationToken,
) -> Result<MailboxGuard, ErrorInfo> {
    let no_port = || ErrorInfo::new(ErrorCode::Network, "no loopback port for the mailbox");
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .map_err(|_| no_port())?;
    let port = listener.local_addr().map_err(|_| no_port())?.port();
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "no randomness available"))?;
    let token = hex::encode(&random);
    let stop = parent.child_token();
    let verdict = Arc::new(OnceLock::new());
    let guard = Guard {
        upstream: upstream.clone(),
        token: token.clone(),
        tuning: *tuning,
        verdict: verdict.clone(),
    };
    let running = stop.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = running.cancelled() => {}
            () = guard.run(listener) => {}
        }
    });
    Ok(MailboxGuard {
        url: format!("ws://127.0.0.1:{port}/{token}"),
        verdict,
        _stop: stop.drop_guard(),
    })
}

struct Guard {
    upstream: Url,
    token: String,
    tuning: Tuning,
    verdict: Arc<OnceLock<ErrorInfo>>,
}

impl Guard {
    /// Runs to the end, recording why it ended if that was a refusal.
    async fn run(&self, listener: TcpListener) {
        match self.accept_library(&listener).await {
            Ok(local) => {
                drop(listener);
                self.serve(local).await;
            }
            Err(e) => {
                self.refuse(e);
            }
        }
    }

    /// Records why the guard is closing. Called before the library's side
    /// is closed, so the verdict is there when the library's error comes
    /// back.
    fn refuse(&self, e: ErrorInfo) {
        tracing::debug!(reason = %e.message, "mailbox guard closed");
        let _ = self.verdict.set(e);
    }

    /// Accepts the library's connection: loopback only, the token as the
    /// path, a bounded number of tries.
    async fn accept_library(&self, listener: &TcpListener) -> Result<Local, ErrorInfo> {
        let deadline = tokio::time::Instant::now()
            .checked_add(self.tuning.handshake)
            .unwrap_or_else(tokio::time::Instant::now);
        for _ in 0..MAX_LOCAL_ATTEMPTS {
            let accepted = tokio::time::timeout_at(deadline, listener.accept()).await;
            let Ok(Ok((stream, peer))) = accepted else {
                return Err(net("the wormhole library did not connect"));
            };
            if !peer.ip().is_loopback() {
                continue;
            }
            let expected = format!("/{}", self.token);
            // The error type is tungstenite's `Callback` signature.
            #[allow(clippy::result_large_err)]
            let check = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                if req.uri().path() == expected {
                    Ok(resp)
                } else {
                    let mut refusal = ErrorResponse::new(None);
                    *refusal.status_mut() = StatusCode::NOT_FOUND;
                    Err(refusal)
                }
            };
            let upgrade = async_tungstenite::accept_hdr_async_with_config(
                stream.compat(),
                check,
                Some(ws_config()),
            );
            if let Ok(Ok(ws)) = tokio::time::timeout(LOCAL_UPGRADE_TIMEOUT, upgrade).await {
                return Ok(ws);
            }
        }
        Err(net("the wormhole library did not connect"))
    }

    async fn serve(&self, mut local: Local) {
        let stream = match tokio::time::timeout(self.tuning.handshake, self.connect()).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                self.refuse(e);
                close(&mut local).await;
                return;
            }
            Err(_) => {
                self.refuse(net("the mailbox server is unreachable"));
                close(&mut local).await;
                return;
            }
        };
        let handshake = async_tungstenite::client_async_with_config(
            self.upstream.as_str(),
            stream.compat(),
            Some(ws_config()),
        );
        let mut upstream: Upstream =
            match tokio::time::timeout(self.tuning.handshake, handshake).await {
                Ok(Ok((ws, _response))) => ws,
                _ => {
                    self.refuse(net("the mailbox server refused the WebSocket"));
                    close(&mut local).await;
                    return;
                }
            };
        if let Err(e) = self.pump(&mut local, &mut upstream).await {
            self.refuse(e);
        }
        close(&mut local).await;
        close(&mut upstream).await;
    }

    /// TCP, then TLS for `wss://`.
    async fn connect(&self) -> Result<Either<TcpStream, TlsStream<TcpStream>>, ErrorInfo> {
        let host = super::session::host_string(&self.upstream)
            .ok_or_else(|| unusable("the mailbox URL has no host"))?;
        let port = self
            .upstream
            .port_or_known_default()
            .ok_or_else(|| unusable("the mailbox URL has no port"))?;
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|_| net("the mailbox server is unreachable"))?;
        let _ = tcp.set_nodelay(true);
        if self.upstream.scheme() == "wss" {
            tls(&host, tcp).await.map(Either::Right)
        } else {
            Ok(Either::Left(tcp))
        }
    }

    /// Forwards until either side closes. The library's messages go out
    /// as they are; the server's are checked first.
    async fn pump(&self, local: &mut Local, upstream: &mut Upstream) -> Result<(), ErrorInfo> {
        let mut budget = Budget::default();
        loop {
            tokio::select! {
                m = next(local) => match m {
                    Some(Ok(Message::Text(t))) => {
                        send(upstream, Message::Text(t), self.tuning.idle).await?;
                    }
                    // The library never sends binary.
                    Some(Ok(Message::Binary(_))) => {
                        return Err(unusable("the wormhole library sent binary data"));
                    }
                    // Pings and pongs are answered by tungstenite itself.
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return Ok(()),
                },
                m = next(upstream) => match m {
                    Some(Ok(Message::Text(t))) => {
                        check_server_message(t.as_str(), &mut budget)?;
                        send(local, Message::Text(t), self.tuning.idle).await?;
                    }
                    Some(Ok(Message::Binary(_))) => {
                        return Err(bad_server("the mailbox server sent binary data"));
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(e)) => return Err(ws_read_error(&e)),
                },
            }
        }
    }
}

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_WS_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_WS_MESSAGE_BYTES))
}

async fn next<T>(ws: &mut WebSocketStream<Compat<T>>) -> Option<Result<Message, WsError>>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    std::future::poll_fn(|cx| Pin::new(&mut *ws).poll_next(cx)).await
}

async fn send<T>(
    ws: &mut WebSocketStream<Compat<T>>,
    m: Message,
    limit: Duration,
) -> Result<(), ErrorInfo>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    match tokio::time::timeout(limit, ws.send(m)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(net("the mailbox connection failed")),
        Err(_) => Err(net("the mailbox connection stalled")),
    }
}

/// Closes with a close frame, so the library reads an orderly end rather
/// than a vanished stream; bounded, best effort.
async fn close<T>(ws: &mut WebSocketStream<Compat<T>>)
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close(None)).await;
}

fn ws_read_error(e: &WsError) -> ErrorInfo {
    match e {
        WsError::Capacity(_) => bad_server("the mailbox server sent an oversized message"),
        WsError::Utf8(_) => bad_server("the mailbox server sent malformed text"),
        WsError::Protocol(_) => bad_server("the mailbox server broke the WebSocket protocol"),
        _ => net("the mailbox connection failed"),
    }
}

/// TLS for `wss://`, with ring and the system's roots through the platform
/// verifier: the same rustls, provider and verifier LocalSend's stack uses.
async fn tls(host: &str, tcp: TcpStream) -> Result<TlsStream<TcpStream>, ErrorInfo> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    let failed = || net("the mailbox server's TLS handshake failed");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| failed())?
        .with_platform_verifier()
        .map_err(|_| failed())?
        .with_no_client_auth();
    let name = match host.parse::<IpAddr>() {
        Ok(ip) => rustls::pki_types::ServerName::IpAddress(ip.into()),
        Err(_) => rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|_| failed())?,
    };
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|_| failed())
}

fn net(message: &'static str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Network, message)
}

fn unusable(message: &'static str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Internal, message)
}

fn bad_server(message: &'static str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Network, message)
}

/// What the server has sent so far over one connection.
#[derive(Debug, Default)]
pub(crate) struct Budget {
    messages: usize,
    bytes: usize,
}

/// Checks one message from the server before the library sees it.
///
/// # Errors
///
/// Over the budget, not a JSON object with a `type`, a `welcome` that asks
/// for too much work, or a `message` whose phase or body the library would
/// panic on.
pub(crate) fn check_server_message(text: &str, budget: &mut Budget) -> Result<(), ErrorInfo> {
    budget.messages = budget.messages.saturating_add(1);
    budget.bytes = budget.bytes.saturating_add(text.len());
    if budget.messages > MAX_SERVER_MESSAGES || budget.bytes > MAX_SERVER_BYTES {
        return Err(bad_server("the mailbox server sent too much"));
    }
    let v: Value = serde_json::from_str(text)
        .map_err(|_| bad_server("the mailbox server sent malformed JSON"))?;
    match v.get("type").and_then(Value::as_str) {
        Some("welcome") => check_welcome(&v),
        Some("message") => check_peer_message(&v),
        Some(_) => Ok(()),
        None => Err(bad_server(
            "the mailbox server sent a message without a type",
        )),
    }
}

fn check_welcome(v: &Value) -> Result<(), ErrorInfo> {
    let Some(hashcash) = v
        .get("welcome")
        .and_then(|w| w.get("permission-required"))
        .and_then(|p| p.get("hashcash"))
    else {
        return Ok(());
    };
    if hashcash.is_null() {
        return Ok(());
    }
    let bits_ok = hashcash
        .get("bits")
        .and_then(Value::as_u64)
        .is_some_and(|b| b <= MAX_HASHCASH_BITS);
    let resource_ok = hashcash
        .get("resource")
        .and_then(Value::as_str)
        .is_some_and(|r| r.len() <= MAX_HASHCASH_RESOURCE_BYTES);
    if bits_ok && resource_ok {
        Ok(())
    } else {
        Err(bad_server("the mailbox server demands too much work"))
    }
}

/// A peer message as the server relays it: `side`, `phase` and a hex
/// `body`. The library would reach `todo!()` on a phase that is neither
/// `pake`, `version` nor a `u64`, and slice out of bounds on an encrypted
/// body shorter than a nonce.
fn check_peer_message(v: &Value) -> Result<(), ErrorInfo> {
    let bad = || bad_server("the mailbox server relayed a malformed message");
    let side = v.get("side").and_then(Value::as_str).ok_or_else(bad)?;
    if side.is_empty() || side.len() > MAX_SIDE_BYTES {
        return Err(bad());
    }
    let phase = v.get("phase").and_then(Value::as_str).ok_or_else(bad)?;
    let body = v.get("body").and_then(Value::as_str).ok_or_else(bad)?;
    let hex_ok = body.len().is_multiple_of(2) && body.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex_ok {
        return Err(bad());
    }
    let ok = match phase {
        "pake" => body.len() <= MAX_PAKE_BODY_HEX,
        "version" => body.len() >= MIN_SEALED_BODY_HEX,
        numeric => {
            numeric.len() <= MAX_PHASE_DIGITS
                && numeric.bytes().all(|b| b.is_ascii_digit())
                && numeric.parse::<u64>().is_ok()
                && body.len() >= MIN_SEALED_BODY_HEX
        }
    };
    if ok { Ok(()) } else { Err(bad()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that sends a welcome, then echoes.
    async fn upstream() -> Url {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let Ok(mut ws) = async_tungstenite::accept_async(s.compat()).await else {
                        return;
                    };
                    let _ = ws
                        .send(Message::text(r#"{"type":"welcome","welcome":{}}"#))
                        .await;
                    while let Some(Ok(Message::Text(t))) = next(&mut ws).await {
                        let _ = ws.send(Message::Text(t)).await;
                    }
                });
            }
        });
        format!("ws://{addr}/v1").parse().unwrap()
    }

    async fn client(url: &str) -> Result<Local, ()> {
        let u: Url = url.parse().unwrap();
        let tcp = TcpStream::connect((u.host_str().unwrap(), u.port().unwrap()))
            .await
            .map_err(|_| ())?;
        async_tungstenite::client_async(url, tcp.compat())
            .await
            .map(|(ws, _)| ws)
            .map_err(|_| ())
    }

    #[tokio::test]
    async fn only_the_token_opens_the_guard() {
        let up = upstream().await;
        let stop = CancellationToken::new();
        let guard = open(&up, &Tuning::default(), &stop).await.unwrap();
        let (base, token) = guard.url.rsplit_once('/').unwrap();
        assert_eq!(token.len(), 32);
        assert!(base.starts_with("ws://127.0.0.1:"));
        // A local stranger without the token is turned away...
        assert!(client(&format!("{base}/v1")).await.is_err());
        assert!(client(&format!("{base}/{}", "0".repeat(32))).await.is_err());
        // ...and the library still gets through.
        let mut ws = client(&guard.url).await.unwrap();
        match next(&mut ws).await {
            Some(Ok(Message::Text(t))) => assert!(t.as_str().contains("welcome")),
            other => panic!("{other:?}"),
        }
        ws.send(Message::text(r#"{"type":"ack"}"#)).await.unwrap();
        match next(&mut ws).await {
            Some(Ok(Message::Text(t))) => assert_eq!(t.as_str(), r#"{"type":"ack"}"#),
            other => panic!("{other:?}"),
        }
        assert!(guard.verdict().is_none());
        // Dropping the guard ends it.
        drop(guard);
        let end = tokio::time::timeout(Duration::from_secs(5), next(&mut ws)).await;
        assert!(
            matches!(end, Ok(None | Some(Ok(Message::Close(_)) | Err(_)))),
            "{end:?}"
        );
    }

    #[tokio::test]
    async fn the_guard_gives_up_on_a_library_that_never_comes() {
        let up = upstream().await;
        let stop = CancellationToken::new();
        let t = Tuning {
            handshake: Duration::from_millis(200),
            ..Tuning::default()
        };
        let guard = open(&up, &t, &stop).await.unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(guard.verdict().is_some());
        assert!(client(&guard.url).await.is_err(), "the port is closed");
    }

    fn check(s: &str) -> Result<(), ErrorInfo> {
        check_server_message(s, &mut Budget::default())
    }

    #[test]
    fn ordinary_server_messages_pass() {
        assert!(check(r#"{"type":"welcome","welcome":{"motd":"hi"}}"#).is_ok());
        assert!(
            check(r#"{"type":"welcome","welcome":{"permission-required":{"none":{}}}}"#).is_ok()
        );
        assert!(
            check(r#"{"type":"welcome","welcome":{"permission-required":{"hashcash":{"bits":6,"resource":"r"}}}}"#)
                .is_ok()
        );
        assert!(check(r#"{"type":"ack","id":null}"#).is_ok());
        assert!(check(r#"{"type":"nameplates","nameplates":[{"id":"7"}]}"#).is_ok());
        let body = "ab".repeat(40);
        assert!(
            check(&format!(
                r#"{{"type":"message","side":"0123456789","phase":"0","body":"{body}"}}"#
            ))
            .is_ok()
        );
        assert!(
            check(&format!(
                r#"{{"type":"message","side":"s","phase":"version","body":"{body}"}}"#
            ))
            .is_ok()
        );
        assert!(check(r#"{"type":"message","side":"s","phase":"pake","body":"7b7d"}"#).is_ok());
    }

    #[test]
    fn messages_the_library_would_hang_or_panic_on_are_stopped() {
        // Hashcash the library would mint forever.
        assert!(check(r#"{"type":"welcome","welcome":{"permission-required":{"hashcash":{"bits":200,"resource":"r"}}}}"#).is_err());
        assert!(check(r#"{"type":"welcome","welcome":{"permission-required":{"hashcash":{"bits":"6","resource":"r"}}}}"#).is_err());
        let body = "ab".repeat(40);
        // todo!() on a non-numeric phase.
        for phase in ["dilate-1", "99999999999999999999999", "-1", "", "1.0", " 1"] {
            assert!(
                check(&format!(
                    r#"{{"type":"message","side":"s","phase":"{phase}","body":"{body}"}}"#
                ))
                .is_err(),
                "{phase:?}"
            );
        }
        // split_at past the end of a short body.
        assert!(check(r#"{"type":"message","side":"s","phase":"3","body":"abcd"}"#).is_err());
        assert!(check(r#"{"type":"message","side":"s","phase":"version","body":""}"#).is_err());
        // Not hex, odd length, missing fields.
        assert!(
            check(&format!(
                r#"{{"type":"message","side":"s","phase":"1","body":"{body}zz"}}"#
            ))
            .is_err()
        );
        assert!(
            check(&format!(
                r#"{{"type":"message","side":"s","phase":"1","body":"{body}a"}}"#
            ))
            .is_err()
        );
        assert!(check(r#"{"type":"message","phase":"1"}"#).is_err());
        assert!(check(r#"{"no":"type"}"#).is_err());
        assert!(check("[]").is_err());
        assert!(check("not json").is_err());
    }

    #[test]
    fn the_budget_is_per_connection() {
        let mut b = Budget::default();
        for _ in 0..MAX_SERVER_MESSAGES {
            check_server_message(r#"{"type":"ack"}"#, &mut b).unwrap();
        }
        assert!(check_server_message(r#"{"type":"ack"}"#, &mut b).is_err());
        let mut b = Budget::default();
        let pad = "a".repeat(MAX_WS_MESSAGE_BYTES.checked_sub(32).unwrap());
        let big = format!(r#"{{"type":"x","pad":"{pad}"}}"#);
        for _ in 0..4 {
            check_server_message(&big, &mut b).unwrap();
        }
        assert!(check_server_message(&big, &mut b).is_err());
    }
}
