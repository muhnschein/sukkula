//! Our HTTPS client: one TLS connection to one peer, HTTP/1.1 over it.
//!
//! Everything a peer sends back is read the bounded way ([`wire`]): response
//! heads within `max_buf_size`, JSON bodies at most 64 KiB, every read with a
//! timeout. Upstream's client reads response bodies whole
//! (`reqwest::Response::json`/`text`), so a hostile receiver -- or anyone
//! who announces itself on the LAN, since discovery answers announcements --
//! could have it buffer without limit; see `super` for the list.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hyper::body::Incoming;
use hyper::client::conn::http1::{self, SendRequest};
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use super::Shared;
use super::identity::Identity;
use super::tls::{self, PinnedServer};
use super::wire::{self, JsonError, OutBody, ReadError};
use crate::api::{ErrorCode, ErrorInfo};

/// Largest response head accepted, and the read buffer size.
const MAX_BUF_BYTES: usize = 64 * 1024;

/// Most response headers accepted.
const MAX_HEADERS: usize = 32;

/// An open connection.
pub(super) struct Conn {
    sender: SendRequest<OutBody>,
    host: String,
    /// How long the connection may take to be ready for a request.
    wait: Duration,
    /// The fingerprint the server proved, uppercase.
    fingerprint: String,
    /// Ends the connection's driver task when this is dropped.
    _close: tokio_util::sync::DropGuard,
}

/// Why a request failed, before it is mapped to what the UI shows.
#[derive(Debug)]
pub(super) enum Failure {
    /// The certificate was not the pinned one (F-LS3). The fingerprint of
    /// the one the server showed instead: what discovery learns about that
    /// address (`discovery::Claims`). Shown, not proven -- the handshake
    /// stopped before the server signed anything.
    Mismatch(Option<String>),
    /// The address is not one we talk to (S7).
    NotPermitted,
    /// Timed out, refused, reset, or garbled.
    Network(&'static str),
    /// The peer answered with this status.
    Status(StatusCode),
}

impl Failure {
    /// What the UI is told.
    pub(super) fn into_error(self) -> ErrorInfo {
        match self {
            Failure::Mismatch(_) => ErrorInfo::new(
                ErrorCode::PeerMismatch,
                "the peer's certificate does not match the one it announced",
            ),
            Failure::NotPermitted => {
                ErrorInfo::new(ErrorCode::Refused, "the peer's address is not on the LAN")
            }
            Failure::Network(what) => ErrorInfo::new(ErrorCode::Network, what),
            Failure::Status(s) => status_error(s),
        }
    }
}

/// What a status from a LocalSend receiver means for a send.
fn status_error(status: StatusCode) -> ErrorInfo {
    match status.as_u16() {
        401 => ErrorInfo::new(ErrorCode::Refused, "the receiver requires a PIN"),
        403 => ErrorInfo::new(ErrorCode::Refused, "the receiver declined"),
        409 => ErrorInfo::new(ErrorCode::Refused, "the receiver is busy"),
        429 => ErrorInfo::new(ErrorCode::Refused, "the receiver is refusing requests"),
        413 => ErrorInfo::new(
            ErrorCode::TooLarge,
            "the receiver refused the request as too large",
        ),
        _ => ErrorInfo::new(ErrorCode::Network, "the receiver reported an error"),
    }
}

impl Conn {
    /// Connects to `addr` and completes TLS, pinned to `expected` when set.
    pub(super) async fn open(
        shared: &Shared,
        identity: &Identity,
        addr: SocketAddr,
        expected: Option<&str>,
    ) -> Result<Conn, Failure> {
        if !shared.ctx.permits(addr.ip()) {
            return Err(Failure::NotPermitted);
        }
        let handshake = shared.opts.handshake_timeout();
        let tcp = tokio::time::timeout(handshake, TcpStream::connect(addr))
            .await
            .map_err(|_| Failure::Network("connecting timed out"))?
            .map_err(|_| Failure::Network("could not connect"))?;
        let _ = tcp.set_nodelay(true);

        let verifier = PinnedServer::new(expected);
        let config = tls::client_config(identity, verifier.clone())
            .map_err(|_| Failure::Network("TLS configuration failed"))?;
        let name = ServerName::IpAddress(addr.ip().into());
        let tls = tokio::time::timeout(handshake, TlsConnector::from(config).connect(name, tcp))
            .await
            .map_err(|_| Failure::Network("the TLS handshake timed out"))?;
        let tls = match tls {
            Ok(tls) => tls,
            Err(_) if verifier.mismatched() => return Err(Failure::Mismatch(verifier.seen())),
            Err(_) => return Err(Failure::Network("the TLS handshake failed")),
        };
        let fingerprint = verifier
            .seen()
            .ok_or(Failure::Network("the peer presented no certificate"))?;

        let (sender, conn) = http1::Builder::new()
            .max_buf_size(MAX_BUF_BYTES)
            .max_headers(MAX_HEADERS)
            .handshake::<_, OutBody>(TokioIo::new(tls))
            .await
            .map_err(|_| Failure::Network("HTTP handshake failed"))?;
        let close = CancellationToken::new();
        let stop = close.clone();
        let shutdown = shared.ctx.shutdown_token().clone();
        shared.tasks.spawn(async move {
            tokio::select! {
                _ = conn => {}
                () = stop.cancelled() => {}
                () = shutdown.cancelled() => {}
            }
        });
        Ok(Conn {
            sender,
            host: addr.to_string(),
            wait: handshake,
            fingerprint,
            _close: close.drop_guard(),
        })
    }

    /// The fingerprint the server proved.
    pub(super) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Whether the peer closed the connection, so the next request needs a
    /// new one.
    pub(super) fn closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// Starts a request; the response future is the caller's to bound.
    pub(super) async fn start(
        &mut self,
        method: Method,
        path_and_query: &str,
        body: OutBody,
        json: bool,
    ) -> Result<impl Future<Output = hyper::Result<Response<Incoming>>> + use<>, Failure> {
        tokio::time::timeout(self.wait, self.sender.ready())
            .await
            .map_err(|_| Failure::Network("the connection is stuck"))?
            .map_err(|_| Failure::Network("the connection closed"))?;
        let mut req = Request::builder()
            .method(method)
            .uri(path_and_query)
            .header(HOST, self.host.as_str());
        if json {
            req = req.header(CONTENT_TYPE, "application/json");
        }
        let req = req
            .body(body)
            .map_err(|_| Failure::Network("could not build the request"))?;
        Ok(self.sender.send_request(req))
    }

    /// Sends a request with a small body and waits `wait` for the head.
    pub(super) async fn call(
        &mut self,
        method: Method,
        path_and_query: &str,
        body: OutBody,
        wait: Duration,
    ) -> Result<Response<Incoming>, Failure> {
        let json = matches!(body, OutBody::Full(_));
        let pending = self.start(method, path_and_query, body, json).await?;
        tokio::time::timeout(wait, pending)
            .await
            .map_err(|_| Failure::Network("the peer did not answer in time"))?
            .map_err(|_| Failure::Network("the request failed"))
    }
}

/// Reads a JSON response of at most 64 KiB.
pub(super) async fn json<T: DeserializeOwned>(
    shared: &Shared,
    response: Response<Incoming>,
) -> Result<T, Failure> {
    let idle = shared.opts.idle_timeout();
    wire::read_json(response.into_body(), idle, shared.opts.handshake_timeout())
        .await
        .map_err(|e| match e {
            JsonError::Read(ReadError::TooLarge) => {
                Failure::Network("the peer's answer is too large")
            }
            JsonError::Read(_) => Failure::Network("reading the peer's answer failed"),
            JsonError::Malformed => Failure::Network("the peer's answer is malformed"),
        })
}

/// Reads and discards a response body, at most 64 KiB of it, so the
/// connection can be reused. Errors are ignored: the status was the answer.
pub(super) async fn drain(shared: &Shared, response: Response<Incoming>) {
    let _ = wire::read_capped(
        response.into_body(),
        sukkula_core::limits::MAX_MESSAGE_BYTES,
        shared.opts.idle_timeout(),
        shared.opts.handshake_timeout(),
    )
    .await;
}

/// Asks the peer at `addr` who it is: `POST /register` with our details when
/// we are receiving (so it lists us), `GET /info` otherwise (so it does not
/// list a phone that would refuse its offers). Returns the proven
/// fingerprint and what the peer said about itself.
pub(super) async fn introduce(
    shared: &Arc<Shared>,
    identity: &Identity,
    addr: SocketAddr,
    expected: Option<&str>,
) -> Result<(String, PeerSays), Failure> {
    let mut conn = Conn::open(shared, identity, addr, expected).await?;
    let wait = shared.opts.handshake_timeout();
    let says = if shared.receiving() {
        let dto = shared.register_dto(identity);
        let path = format!("{}/register", wire::API_V2);
        let response = conn
            .call(Method::POST, &path, OutBody::json(&dto), wait)
            .await?;
        if response.status() != StatusCode::OK {
            return Err(Failure::Status(response.status()));
        }
        let r: localsend::http::dto_v2::RegisterResponseDtoV2 = json(shared, response).await?;
        PeerSays {
            alias: r.alias,
            model: r.device_model,
            device_type: r.device_type,
        }
    } else {
        let path = format!("{}/info", wire::API_V2);
        let response = conn.call(Method::GET, &path, OutBody::Empty, wait).await?;
        if response.status() != StatusCode::OK {
            return Err(Failure::Status(response.status()));
        }
        let r: localsend::http::dto_v2::InfoResponseDtoV2 = json(shared, response).await?;
        PeerSays {
            alias: r.alias,
            model: r.device_model,
            device_type: r.device_type,
        }
    };
    Ok((conn.fingerprint().to_owned(), says))
}

/// A peer's description of itself. Nothing here has been checked.
pub(super) struct PeerSays {
    pub(super) alias: String,
    pub(super) model: Option<String>,
    pub(super) device_type: Option<localsend::model::discovery::DeviceType>,
}

/// Tells a peer, best effort, that we cancelled: `POST /cancel`.
pub(super) async fn cancel(
    shared: &Arc<Shared>,
    identity: &Identity,
    addr: SocketAddr,
    fingerprint: &str,
    session_id: Option<&str>,
) {
    let attempt = async {
        let mut conn = Conn::open(shared, identity, addr, Some(fingerprint)).await?;
        let path = match session_id {
            Some(id) => {
                wire::path_with_query(&format!("{}/cancel", wire::API_V2), &[("sessionId", id)])
            }
            None => format!("{}/cancel", wire::API_V2),
        };
        let response = conn
            .call(
                Method::POST,
                &path,
                OutBody::Empty,
                shared.opts.handshake_timeout(),
            )
            .await?;
        drain(shared, response).await;
        Ok::<(), Failure>(())
    };
    // Bounded as a whole: a cancel must never keep anything waiting.
    let _ = tokio::time::timeout(shared.opts.handshake_timeout(), attempt).await;
}

/// An empty-bodied chunk channel's capacity: two chunks in flight at most.
pub(super) const STREAM_DEPTH: usize = 2;

/// A body fed chunk by chunk, of exactly `len` bytes.
pub(super) fn stream_body(len: u64) -> (tokio::sync::mpsc::Sender<Bytes>, OutBody) {
    let (tx, rx) = tokio::sync::mpsc::channel(STREAM_DEPTH);
    (tx, OutBody::Stream { rx, len })
}
