//! What the send and receive paths share: panic containment, timeouts, the
//! mapping from library errors to what the UI shows, the servers from the
//! settings, and one open wormhole with its message budget.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use magic_wormhole::transit::{self, DirectHint, RelayHint};
use magic_wormhole::{Key, KeyPurpose, Wormhole, WormholeError};
use sukkula_core::config::{MAX_URL_BYTES, WormholeSettings};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::mailbox::MailboxGuard;
use super::wire::{self, PeerMsg};
use super::{CLEANUP_WAIT, MAX_PEER_MESSAGE_BYTES, MAX_PEER_MESSAGES};
use crate::api::{ErrorCode, ErrorInfo};
use crate::ctx::cancelled;

// ------------------------------------------------------------- panics

/// A library future panicked. The v1 path still has `todo!`, `expect` and
/// slice indexing reachable from the mailbox server, the STUN server and the
/// transit peer (see the upstream findings in the module docs); the engine
/// builds with `panic = "unwind"`, so the panic is caught here and the
/// transfer fails instead of the task dying half-way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Panicked;

/// Polls a future, turning a panic inside any poll into [`Panicked`].
///
/// Once it has panicked the future is dropped and never polled again: its
/// state is whatever the unwinding left, and nothing of it is trusted.
pub(crate) struct CatchUnwind<'a, T> {
    inner: Option<Pin<Box<dyn Future<Output = T> + Send + 'a>>>,
}

impl<'a, T> CatchUnwind<'a, T> {
    /// Wraps `fut`.
    pub(crate) fn new(fut: impl Future<Output = T> + Send + 'a) -> Self {
        CatchUnwind {
            inner: Some(Box::pin(fut)),
        }
    }
}

impl<T> Future for CatchUnwind<'_, T> {
    type Output = Result<T, Panicked>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(Err(Panicked));
        };
        // AssertUnwindSafe: after a panic the future is dropped unpolled,
        // and the state it shares with us (a transfer handle, an inbox
        // file) is built to stay consistent when a holder unwinds.
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(v)) => {
                self.inner = None;
                Poll::Ready(Ok(v))
            }
            Err(_) => {
                self.inner = None;
                Poll::Ready(Err(Panicked))
            }
        }
    }
}

/// The error a caught panic becomes.
pub(crate) fn panicked() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Internal, "the wormhole library failed")
}

/// Runs a library future with a deadline, catching its panics.
pub(crate) async fn lib<T>(
    limit: Duration,
    what: &'static str,
    fut: impl Future<Output = T> + Send,
) -> Result<T, ErrorInfo> {
    match tokio::time::timeout(limit, CatchUnwind::new(fut)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(Panicked)) => Err(panicked()),
        Err(_) => Err(timed_out(what)),
    }
}

/// As [`lib`], with an absolute deadline.
pub(crate) async fn lib_until<T>(
    deadline: Instant,
    what: &'static str,
    fut: impl Future<Output = T> + Send,
) -> Result<T, ErrorInfo> {
    match tokio::time::timeout_at(deadline, CatchUnwind::new(fut)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(Panicked)) => Err(panicked()),
        Err(_) => Err(timed_out(what)),
    }
}

/// Runs `fut` until it ends or `token` is cancelled.
pub(crate) async fn cancellable<T>(
    token: &CancellationToken,
    fut: impl Future<Output = Result<T, ErrorInfo>>,
) -> Result<T, ErrorInfo> {
    tokio::select! {
        r = fut => r,
        () = token.cancelled() => Err(cancelled()),
    }
}

/// The error for a wait that ran out.
pub(crate) fn timed_out(what: &'static str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Network, format!("{what} timed out"))
}

/// The peer broke the v1 protocol.
pub(crate) fn protocol(what: &'static str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Network, what)
}

/// Maps a library error to what the UI shows. The library's own messages
/// can carry what the server or the peer sent, so none of them is passed
/// on: the message is always one of ours (api.rs: "never containing
/// peer-supplied text").
pub(crate) fn wormhole_error(e: &WormholeError) -> ErrorInfo {
    let (code, message) = match e {
        WormholeError::PakeFailed => (ErrorCode::BadCode, "the code does not match the other side"),
        WormholeError::UnclaimedNameplate(_) => {
            (ErrorCode::BadCode, "nobody is waiting with this code")
        }
        WormholeError::CodeInvalid(_) => (ErrorCode::BadCode, "the code is not usable"),
        WormholeError::Crypto => (
            ErrorCode::Network,
            "a message from the peer did not decrypt",
        ),
        WormholeError::ServerError(_) => (ErrorCode::Network, "the mailbox server failed"),
        _ => (ErrorCode::Network, "the wormhole connection failed"),
    };
    tracing::debug!(kind = message, "wormhole error");
    ErrorInfo::new(code, message)
}

// ------------------------------------------------------------ servers

/// One transit relay endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// A host name or an IP literal, without brackets.
    pub host: String,
    /// Its port.
    pub port: u16,
}

/// The servers a transfer uses (F-MW4): the library's defaults unless the
/// settings name others.
#[derive(Clone, Debug)]
pub(crate) struct Servers {
    /// The mailbox (rendezvous) server, `ws://` or `wss://`.
    pub mailbox: Url,
    /// Whether it is not the default, and so belongs in the QR code.
    pub custom_mailbox: bool,
    /// Our transit relay.
    pub relay: Endpoint,
}

impl Servers {
    /// The servers the settings ask for, checked again here: the settings
    /// were validated when saved, but a URL is only trusted where it is
    /// used.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::BadSettings`] for a URL this adapter cannot use.
    pub(crate) fn from_settings(s: &WormholeSettings) -> Result<Servers, ErrorInfo> {
        let custom = s
            .mailbox_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());
        let mailbox = check_mailbox_url(
            custom.unwrap_or(magic_wormhole::rendezvous::DEFAULT_RENDEZVOUS_SERVER),
        )?;
        let relay = s
            .relay_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());
        let relay = check_relay_url(relay.unwrap_or(transit::DEFAULT_RELAY_SERVER))?;
        Ok(Servers {
            mailbox,
            custom_mailbox: custom.is_some(),
            relay,
        })
    }

    /// Our relay as the hint the peer is sent.
    pub(crate) fn relay_hint(&self) -> RelayHint {
        RelayHint::new(
            Some(self.relay.host.clone()),
            [DirectHint::new(self.relay.host.clone(), self.relay.port)],
            [],
        )
    }
}

fn bad_url(what: &'static str) -> ErrorInfo {
    ErrorInfo::new(
        ErrorCode::BadSettings,
        format!("the {what} URL is not usable"),
    )
}

fn plain_url(raw: &str, what: &'static str) -> Result<Url, ErrorInfo> {
    if raw.is_empty() || raw.len() > MAX_URL_BYTES || !raw.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(bad_url(what));
    }
    let url = Url::parse(raw).map_err(|_| bad_url(what))?;
    let ok = url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.query().is_none()
        && host_string(&url).is_some_and(|h| !h.is_empty());
    if ok { Ok(url) } else { Err(bad_url(what)) }
}

/// A `ws://` or `wss://` URL with a host and nothing that could smuggle
/// credentials or a second target.
pub(crate) fn check_mailbox_url(raw: &str) -> Result<Url, ErrorInfo> {
    let url = plain_url(raw, "mailbox")?;
    let ok = matches!(url.scheme(), "ws" | "wss") && url.port_or_known_default().is_some();
    if ok { Ok(url) } else { Err(bad_url("mailbox")) }
}

/// A `tcp://host:port` URL with nothing else.
pub(crate) fn check_relay_url(raw: &str) -> Result<Endpoint, ErrorInfo> {
    let url = plain_url(raw, "relay")?;
    let path_ok = matches!(url.path(), "" | "/");
    match (url.scheme(), host_string(&url), url.port()) {
        ("tcp", Some(host), Some(port)) if path_ok && port != 0 => Ok(Endpoint { host, port }),
        _ => Err(bad_url("relay")),
    }
}

/// The host of `url` as a connectable string: IPv6 literals without their
/// brackets.
pub(crate) fn host_string(url: &Url) -> Option<String> {
    match url.host()? {
        url::Host::Domain(d) => Some(d.to_owned()),
        url::Host::Ipv4(a) => Some(a.to_string()),
        url::Host::Ipv6(a) => Some(a.to_string()),
    }
}

// ------------------------------------------------------------ session

/// Marker for the relay token key; the library's own marker is private.
#[derive(Debug)]
struct RelayToken;

impl KeyPurpose for RelayToken {}

/// An open wormhole: the library's connection, the guard it runs through,
/// and how many peer messages it may still read.
pub(crate) struct Session {
    wormhole: Wormhole,
    guard: MailboxGuard,
    read: usize,
}

impl Session {
    /// Wraps a connected wormhole.
    pub(crate) fn new(wormhole: Wormhole, guard: MailboxGuard) -> Self {
        Session {
            wormhole,
            guard,
            read: 0,
        }
    }

    /// The error to report for a failed library call: the guard's verdict
    /// when it closed the connection on purpose, else the library's.
    pub(crate) fn explain(&self, e: &WormholeError) -> ErrorInfo {
        self.guard.verdict().unwrap_or_else(|| wormhole_error(e))
    }

    /// Sends one v1 message.
    pub(crate) async fn send(&mut self, limit: Duration, msg: Vec<u8>) -> Result<(), ErrorInfo> {
        match lib(limit, "sending to the peer", self.wormhole.send(msg)).await? {
            Ok(()) => Ok(()),
            Err(e) => Err(self.explain(&e)),
        }
    }

    /// Reads the next peer message by `deadline`, within the message budget
    /// and the size cap, and parses it.
    pub(crate) async fn receive_until(&mut self, deadline: Instant) -> Result<PeerMsg, ErrorInfo> {
        self.read = self.read.saturating_add(1);
        if self.read > MAX_PEER_MESSAGES {
            return Err(protocol("the peer sent too many messages"));
        }
        let received = lib_until(deadline, "waiting for the peer", self.wormhole.receive()).await?;
        let bytes = received.map_err(|e| self.explain(&e))?;
        if bytes.len() > MAX_PEER_MESSAGE_BYTES {
            return Err(protocol("the peer sent an oversized message"));
        }
        wire::parse(&bytes)
    }

    /// As [`receive_until`](Self::receive_until), with no deadline of its
    /// own: for waits that something else bounds (the consent timeout).
    pub(crate) async fn receive_unbounded(&mut self) -> Result<PeerMsg, ErrorInfo> {
        self.read = self.read.saturating_add(1);
        if self.read > MAX_PEER_MESSAGES {
            return Err(protocol("the peer sent too many messages"));
        }
        let received = CatchUnwind::new(self.wormhole.receive())
            .await
            .map_err(|Panicked| panicked())?;
        let bytes = received.map_err(|e| self.explain(&e))?;
        if bytes.len() > MAX_PEER_MESSAGE_BYTES {
            return Err(protocol("the peer sent an oversized message"));
        }
        wire::parse(&bytes)
    }

    /// The transit key and the relay token derived from it. The token is
    /// what the library sends a relay first, and what the transit guards
    /// check to know a local connection is the library's.
    pub(crate) fn transit_key(&self) -> (Key<transit::TransitKey>, String) {
        let purpose = format!("{}/transit-key", self.wormhole.appid());
        let key: Key<transit::TransitKey> =
            self.wormhole.key().derive_subkey_from_purpose(&purpose);
        let token = key
            .derive_subkey_from_purpose::<RelayToken>("transit_relay_token")
            .to_hex();
        (key, token)
    }

    /// Ends the session: tells the peer why when `error` is given, then
    /// closes the mailbox. Bounded by [`CLEANUP_WAIT`] and best effort;
    /// skipped after a panic, whose state is not trusted.
    pub(crate) async fn goodbye(self, error: Option<&'static str>) {
        let Session {
            mut wormhole,
            guard,
            ..
        } = self;
        let bye = async move {
            if let Some(reason) = error {
                let _ = wormhole.send(wire::error(reason)).await;
            }
            let _ = wormhole.close().await;
        };
        let _ = tokio::time::timeout(CLEANUP_WAIT, CatchUnwind::new(bye)).await;
        drop(guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panicking_future_becomes_an_error() {
        let r = CatchUnwind::new(async {
            let v: Vec<u8> = Vec::new();
            #[allow(clippy::indexing_slicing)]
            v[3]
        })
        .await;
        assert_eq!(r, Err(Panicked));
        let r = CatchUnwind::new(async { 7 }).await;
        assert_eq!(r, Ok(7));
    }

    #[tokio::test]
    async fn a_panic_after_a_pending_poll_is_caught_too() {
        let r = CatchUnwind::new(async {
            tokio::task::yield_now().await;
            panic!("late");
        })
        .await;
        assert_eq!(r, Err::<(), _>(Panicked));
    }

    #[test]
    fn urls_are_checked_at_use() {
        assert!(check_mailbox_url("ws://relay.magic-wormhole.io:4000/v1").is_ok());
        assert!(check_mailbox_url("wss://mailbox.example/v1").is_ok());
        for bad in [
            "",
            "http://x/v1",
            "ws://user:pw@host/v1",
            "ws://host/v1#frag",
            "ws://host/v1?a=b",
            "ws://",
            "ws://:80/v1",
            "ws://ho st/v1",
            "tcp://host:1",
        ] {
            assert!(check_mailbox_url(bad).is_err(), "{bad}");
        }
        assert_eq!(
            check_relay_url("tcp://transit.magic-wormhole.io:4001").unwrap(),
            Endpoint {
                host: "transit.magic-wormhole.io".into(),
                port: 4001
            }
        );
        assert_eq!(check_relay_url("tcp://[::1]:9").unwrap().host, "::1");
        for bad in [
            "tcp://host",
            "tcp://host:0",
            "tcp://host:1/path",
            "ws://host:1",
            "tcp://u@host:1",
        ] {
            assert!(check_relay_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_servers_are_the_librarys() {
        let s = Servers::from_settings(&WormholeSettings::default()).unwrap();
        assert_eq!(s.mailbox.as_str(), "ws://relay.magic-wormhole.io:4000/v1");
        assert!(!s.custom_mailbox);
        assert_eq!(s.relay.host, "transit.magic-wormhole.io");
        assert_eq!(s.relay.port, 4001);
        let s = Servers::from_settings(&WormholeSettings {
            mailbox_url: Some("wss://m.example/v1".into()),
            relay_url: Some("tcp://r.example:9".into()),
        })
        .unwrap();
        assert!(s.custom_mailbox);
        assert_eq!(s.relay.port, 9);
        assert_eq!(
            Servers::from_settings(&WormholeSettings {
                mailbox_url: Some("wss://m.example/v1#x".into()),
                relay_url: None,
            })
            .unwrap_err()
            .code,
            ErrorCode::BadSettings
        );
    }
}
