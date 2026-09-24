//! Our HTTPS server: LocalSend v2's five endpoints, and the receive path of
//! `crate::ctx` behind them.
//!
//! In the order a hostile byte meets them:
//!
//! 1. **Accept** (S7): the peer's address is checked with
//!    [`Ctx::permits`](crate::ctx::Ctx::permits) before the TLS handshake --
//!    before a byte of it is read. Connections are capped in all
//!    ([`MAX_CONNECTIONS`]), per address ([`MAX_CONNECTIONS_PER_IP`]) and in
//!    rate per address.
//! 2. **TLS** (F-LS2): only HTTPS is spoken; a plain-HTTP request fails the
//!    handshake. A client certificate is required. The handshake has a
//!    deadline.
//! 3. **HTTP/1.1 head**: at most [`MAX_BUF_BYTES`] and [`MAX_HEADERS`], read
//!    within the handshake timeout; an idle keep-alive connection ends on
//!    the same timer.
//! 4. **Routing**: the v2 endpoints only. Every other path is a 404 whose
//!    body is never read.
//! 5. **JSON bodies** (S4, S6): at most 64 KiB, with an idle timeout and an
//!    overall deadline, before parsing.
//! 6. **Offers** (S5): rate-limited per address, then the optional PIN
//!    (F-LS4), then [`Ctx::offer`](crate::ctx::Ctx::offer) -- validation and
//!    the user's consent. One session at a time, like LocalSend itself.
//! 7. **Uploads** (S3): accepted only for an active session, from its
//!    sender's address, with its per-file token. The body is streamed into
//!    the inbox chunk by chunk with an idle timeout on every read; the inbox
//!    caps it at the declared size and deletes it on any failure.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Incoming};
use hyper::header::CONTENT_TYPE;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use localsend::crypto::cert;
use localsend::http::dto::ErrorResponse;
use localsend::http::dto_v2::{
    PrepareUploadRequestDtoV2, PrepareUploadResponseDtoV2, RegisterDtoV2,
};
use localsend::model::discovery::ProtocolType;
use serde::Serialize;
use sukkula_core::Protocol;
use sukkula_core::consent::Refusal;
use sukkula_core::limits::RATE_LIMIT_ENTRIES;
use sukkula_core::offer::{OfferFile, RawFile, RawOffer};
use sukkula_core::reach::RateLimiter;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use super::identity::Identity;
use super::peers::Sighting;
use super::wire::{self, JsonError, ReadError};
use super::{Shared, canonical, client, tls};
use crate::api::{ErrorCode, ErrorInfo, Event, Outcome};
use crate::ctx::{Declined, TransferHandle, cancelled};

/// Connections open at once, in all.
pub(super) const MAX_CONNECTIONS: usize = 32;

/// Connections open at once from one address.
pub(super) const MAX_CONNECTIONS_PER_IP: usize = 8;

/// New connections per address per [`CONNECT_WINDOW`]. Each costs an RSA
/// signature; this keeps one peer from making the phone do nothing else.
const CONNECT_BURST: u32 = 64;

/// The window [`CONNECT_BURST`] refills over.
const CONNECT_WINDOW: Duration = Duration::from_secs(10);

/// Largest request head, and the connection's read buffer.
const MAX_BUF_BYTES: usize = 64 * 1024;

/// Most request headers.
const MAX_HEADERS: usize = 32;

/// Upload requests waiting for the session task at once: as many as the
/// sender can have connections, so a sender uploading in parallel is
/// queued, not refused. Files are still received one at a time.
const UPLOAD_QUEUE: usize = MAX_CONNECTIONS_PER_IP;

/// Wrong PINs from one address before it is locked out (F-LS4).
const PIN_FAILURES_PER_IP: u32 = 3;

/// Wrong PINs from everyone together per [`PIN_WINDOW`] before every
/// address is locked out: a LAN attacker has as many addresses as it wants.
const PIN_FAILURES_GLOBAL: u32 = 10;

/// How long a PIN lockout lasts, and the window the counts are kept for.
const PIN_WINDOW: Duration = Duration::from_secs(300);

/// A running server.
pub(super) struct Server {
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

struct Inner {
    shared: Arc<Shared>,
    identity: Arc<Identity>,
    acceptor: TlsAcceptor,
    /// Whether new offers are taken: the Receive switch. Off with a session
    /// still running means draining: only that session's sender is served.
    accepting: AtomicBool,
    /// Cancelled when receiving stops; connections other than the running
    /// session's then end.
    drain: Mutex<CancellationToken>,
    /// Ends the listener; connections finish the response they are writing
    /// and close. A child of `stop`.
    closing: CancellationToken,
    /// Ends the listener and every connection at once: the engine stopping.
    stop: CancellationToken,
    slot: Mutex<Slot>,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    connect_limiter: Mutex<RateLimiter>,
    permits: Arc<Semaphore>,
    pins: Mutex<PinFailures>,
    next_pending: AtomicU64,
}

/// The single session slot.
enum Slot {
    Free,
    /// An offer is waiting for the user.
    Pending {
        id: u64,
        sender: Party,
        cancel: CancellationToken,
    },
    /// An offer was accepted and its files are arriving.
    Active(Active),
}

/// Who made the offer: its address and the certificate it proved. Uploads
/// and cancels must come from both -- the address, as LocalSend binds them,
/// and the certificate, so that a host spoofing the sender's address on the
/// LAN is not the sender.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Party {
    ip: IpAddr,
    fingerprint: String,
}

impl Party {
    fn of(caller: &Caller) -> Party {
        Party {
            ip: caller.ip,
            fingerprint: caller.fingerprint.clone(),
        }
    }

    fn is(&self, caller: &Caller) -> bool {
        self.ip == caller.ip && self.fingerprint == caller.fingerprint
    }
}

struct Active {
    session_id: String,
    sender: Party,
    uploads: mpsc::Sender<Upload>,
    /// Cancelled by the sender's `POST /cancel`.
    cancel: CancellationToken,
}

/// One upload request, handed from its connection to the session task.
struct Upload {
    file_id: String,
    token: String,
    body: Incoming,
    reply: oneshot::Sender<StatusCode>,
}

/// Who is on the other end of a connection.
struct Caller {
    ip: IpAddr,
    /// The fingerprint of the client certificate it proved.
    fingerprint: String,
}

type Reply = Response<Full<Bytes>>;

impl Server {
    /// Binds `port` on IPv4 and starts accepting.
    ///
    /// # Errors
    ///
    /// The port could not be bound.
    pub(super) async fn start(
        shared: Arc<Shared>,
        identity: Arc<Identity>,
    ) -> Result<Server, ErrorInfo> {
        let config = tls::server_config(&identity)?;
        // IPv4 only: LocalSend's baseline. Every IPv6 peer would also be one
        // more reach to reason about (S7), for no peer that lacks IPv4.
        let listener = TcpListener::bind((shared.opts.bind, shared.opts.port))
            .await
            .map_err(|_| {
                ErrorInfo::new(ErrorCode::Network, "the LocalSend port is not available")
            })?;
        let port = listener
            .local_addr()
            .map_err(|_| ErrorInfo::new(ErrorCode::Network, "the LocalSend port is not available"))?
            .port();
        shared.set_port(port);
        let stop = shared.ctx.shutdown_token().child_token();
        let inner = Arc::new(Inner {
            shared: shared.clone(),
            identity,
            acceptor: TlsAcceptor::from(config),
            accepting: AtomicBool::new(true),
            drain: Mutex::new(CancellationToken::new()),
            closing: stop.child_token(),
            stop,
            slot: Mutex::new(Slot::Free),
            per_ip: Mutex::new(HashMap::new()),
            connect_limiter: Mutex::new(RateLimiter::new(CONNECT_BURST, CONNECT_WINDOW)),
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            pins: Mutex::new(PinFailures::default()),
            next_pending: AtomicU64::new(0),
        });
        let task = shared.tasks.spawn(accept_loop(inner.clone(), listener));
        Ok(Server { inner, task })
    }

    /// The bound port.
    pub(super) fn port(&self) -> u16 {
        self.inner.shared.port()
    }

    /// Whether it has stopped, or is stopping, for good.
    pub(super) fn stopped(&self) -> bool {
        self.inner.closing.is_cancelled()
    }

    /// Takes offers again after [`drain`](Self::drain).
    pub(super) fn resume(&self) {
        *lock(&self.inner.drain) = CancellationToken::new();
        self.inner.accepting.store(true, Ordering::Release);
    }

    /// Stops taking offers: a pending offer is declined, every connection
    /// but the running session's ends. Returns true when nothing is left
    /// running and the server has stopped; otherwise it stops by itself when
    /// the session ends.
    pub(super) fn drain(&self) -> bool {
        self.inner.accepting.store(false, Ordering::Release);
        lock(&self.inner.drain).cancel();
        let idle = {
            let slot = lock(&self.inner.slot);
            match &*slot {
                Slot::Free => true,
                Slot::Pending { cancel, .. } => {
                    cancel.cancel();
                    false
                }
                Slot::Active(_) => false,
            }
        };
        if idle {
            self.inner.closing.cancel();
        }
        idle
    }

    /// Waits for the listener to close.
    pub(super) async fn wait(self) {
        let _ = self.task.await;
    }
}

async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    let mut backoff = Duration::from_millis(50);
    loop {
        let accepted = tokio::select! {
            () = inner.closing.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (tcp, remote) = match accepted {
            Ok(a) => {
                backoff = Duration::from_millis(50);
                a
            }
            Err(_) => {
                // Out of descriptors, or a connection that died in the
                // backlog. Neither says the listener is broken; wait a
                // little so exhaustion does not spin.
                tokio::select! {
                    () = inner.closing.cancelled() => break,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = backoff.saturating_mul(2).min(Duration::from_secs(1));
                continue;
            }
        };
        let ip = canonical(remote.ip());
        // S7, before the handshake: not a byte of an unpermitted peer is read.
        if !inner.shared.ctx.permits(ip) {
            continue;
        }
        if !inner.accepting.load(Ordering::Acquire) && !inner.is_session_sender(ip) {
            continue;
        }
        if !lock(&inner.connect_limiter).allow(ip, Instant::now()) {
            continue;
        }
        let Ok(permit) = inner.permits.clone().try_acquire_owned() else {
            continue;
        };
        let Some(ip_guard) = IpGuard::claim(&inner, ip) else {
            continue;
        };
        let drain = lock(&inner.drain).clone();
        let task_inner = inner.clone();
        inner.shared.tasks.spawn(async move {
            serve(task_inner, tcp, ip, drain, permit, ip_guard).await;
        });
    }
}

async fn serve(
    inner: Arc<Inner>,
    tcp: TcpStream,
    ip: IpAddr,
    drain: CancellationToken,
    _permit: OwnedSemaphorePermit,
    _ip_guard: IpGuard,
) {
    let _ = tcp.set_nodelay(true);
    let handshake = inner.shared.opts.handshake_timeout();
    let tls = tokio::select! {
        () = inner.closing.cancelled() => return,
        tls = tokio::time::timeout(handshake, inner.acceptor.accept(tcp)) => tls,
    };
    let Ok(Ok(tls)) = tls else {
        return;
    };
    // Client authentication is mandatory, so a certificate is always there;
    // without one, there is nobody to talk to.
    let Some(fingerprint) = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| cert::fingerprint_from_cert_der(c.as_ref()))
    else {
        return;
    };
    let caller = Arc::new(Caller { ip, fingerprint });
    let svc_inner = inner.clone();
    let service = service_fn(move |req| {
        let inner = svc_inner.clone();
        let caller = caller.clone();
        async move { Ok::<_, Infallible>(route(&inner, &caller, req).await) }
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(handshake)
        .max_buf_size(MAX_BUF_BYTES)
        .max_headers(MAX_HEADERS)
        .keep_alive(true);
    let conn = builder.serve_connection(TokioIo::new(tls), service);
    tokio::pin!(conn);
    let mut spared = false;
    let mut closing = false;
    loop {
        tokio::select! {
            _ = &mut conn => break,
            () = inner.stop.cancelled() => break,
            () = inner.closing.cancelled(), if !closing => {
                // Let the response being written -- the last file's 200 --
                // go out, then close. Bounded below.
                closing = true;
                conn.as_mut().graceful_shutdown();
            }
            () = tokio::time::sleep(handshake), if closing => break,
            () = drain.cancelled(), if !spared && !closing => {
                if inner.is_session_sender(ip) {
                    spared = true;
                } else {
                    break;
                }
            }
        }
    }
}

/// The endpoints served. Everything else is a 404.
enum Endpoint {
    Info,
    Register,
    PrepareUpload,
    Upload,
    Cancel,
}

fn endpoint(method: &Method, path: &str) -> Option<Endpoint> {
    // Old clients (v1.17 and earlier) probe with the v1 info route.
    if method == Method::GET && path == "/api/localsend/v1/info" {
        return Some(Endpoint::Info);
    }
    let rest = path.strip_prefix(wire::API_V2)?;
    match (method, rest) {
        (&Method::GET, "/info") => Some(Endpoint::Info),
        (&Method::POST, "/register") => Some(Endpoint::Register),
        (&Method::POST, "/prepare-upload") => Some(Endpoint::PrepareUpload),
        (&Method::POST, "/upload") => Some(Endpoint::Upload),
        (&Method::POST, "/cancel") => Some(Endpoint::Cancel),
        _ => None,
    }
}

async fn route(inner: &Arc<Inner>, caller: &Caller, req: Request<Incoming>) -> Reply {
    match endpoint(req.method(), req.uri().path()) {
        Some(Endpoint::Info) => info(inner, caller),
        Some(Endpoint::Register) => register(inner, caller, req).await,
        Some(Endpoint::PrepareUpload) => prepare_upload(inner, caller, req).await,
        Some(Endpoint::Upload) => upload(inner, caller, req).await,
        Some(Endpoint::Cancel) => cancel(inner, caller, &req),
        // The body of an unknown request is never read.
        None => status(StatusCode::NOT_FOUND, "Not found"),
    }
}

fn info(inner: &Inner, caller: &Caller) -> Reply {
    if !inner.accepting.load(Ordering::Acquire) {
        return status(StatusCode::FORBIDDEN, "Not receiving");
    }
    if !inner.shared.ctx.allow_discovery(caller.ip) {
        return status(StatusCode::TOO_MANY_REQUESTS, "Too many requests");
    }
    json(StatusCode::OK, &inner.shared.info_dto(&inner.identity))
}

async fn register(inner: &Inner, caller: &Caller, req: Request<Incoming>) -> Reply {
    if !inner.accepting.load(Ordering::Acquire) {
        return status(StatusCode::FORBIDDEN, "Not receiving");
    }
    if !inner.shared.ctx.allow_discovery(caller.ip) {
        return status(StatusCode::TOO_MANY_REQUESTS, "Too many requests");
    }
    let dto: RegisterDtoV2 = match read_body(inner, req.into_body()).await {
        Ok(dto) => dto,
        Err(reply) => return reply,
    };
    // F-LS2: a peer that says it speaks plain HTTP is not one we talk to.
    if dto.protocol != ProtocolType::Https {
        return status(StatusCode::FORBIDDEN, "Plain HTTP peers are refused");
    }
    // The claimed fingerprint must be the certificate it just proved.
    if wire::fingerprint(&dto.fingerprint).as_deref() != Some(caller.fingerprint.as_str()) {
        return status(
            StatusCode::FORBIDDEN,
            "Fingerprint does not match the certificate",
        );
    }
    if dto.port != 0 && caller.fingerprint != inner.identity.fingerprint() {
        inner.shared.saw_peer(&Sighting {
            fingerprint: &caller.fingerprint,
            addr: SocketAddr::new(caller.ip, dto.port),
            alias: &dto.alias,
            model: dto.device_model.as_deref(),
            device_type: wire::device_type(dto.device_type.as_ref()),
        });
    }
    json(
        StatusCode::OK,
        &inner.shared.register_response(&inner.identity),
    )
}

async fn prepare_upload(inner: &Arc<Inner>, caller: &Caller, req: Request<Incoming>) -> Reply {
    let shared = &inner.shared;
    if !inner.accepting.load(Ordering::Acquire) {
        return status(StatusCode::FORBIDDEN, "Not receiving");
    }
    if !shared.ctx.allow_offer(caller.ip) {
        return status(StatusCode::TOO_MANY_REQUESTS, "Too many requests");
    }
    if let Some(pin) = shared.ctx.settings().localsend.pin {
        let given = wire::query_value(req.uri().query(), "pin");
        match lock(&inner.pins).check(caller.ip, given.as_deref(), &pin, Instant::now()) {
            PinCheck::Ok => {}
            PinCheck::Missing => return status(StatusCode::UNAUTHORIZED, "PIN required"),
            PinCheck::Wrong => return status(StatusCode::UNAUTHORIZED, "Invalid PIN"),
            PinCheck::Locked => return status(StatusCode::TOO_MANY_REQUESTS, "Too many requests"),
        }
    }
    let dto: PrepareUploadRequestDtoV2 = match read_body(inner, req.into_body()).await {
        Ok(dto) => dto,
        Err(reply) => return reply,
    };
    if dto.info.protocol != ProtocolType::Https {
        return status(StatusCode::FORBIDDEN, "Plain HTTP peers are refused");
    }
    if dto.files.is_empty() {
        return status(StatusCode::BAD_REQUEST, "No files provided");
    }
    let Some(mut pending) = PendingGuard::claim(inner, Party::of(caller)) else {
        return status(StatusCode::CONFLICT, "Blocked by another session");
    };

    let (raw, ids) = raw_offer(&dto);
    let sender_port = dto.info.port;
    drop(dto);
    let outcome = tokio::select! {
        outcome = shared.ctx.offer(raw) => outcome,
        () = pending.cancel.cancelled() => {
            return status(StatusCode::FORBIDDEN, "Cancelled");
        }
    };
    let accepted = match outcome {
        Ok(accepted) => accepted,
        Err(Declined::Invalid(_)) => return status(StatusCode::BAD_REQUEST, "Invalid offer"),
        Err(Declined::Refused(Refusal::Busy) | Declined::Busy) => {
            return status(StatusCode::CONFLICT, "Blocked by another session");
        }
        Err(Declined::Refused(_)) => return status(StatusCode::FORBIDDEN, "Rejected"),
        Err(Declined::NoSpace) => return status(StatusCode::INSUFFICIENT_STORAGE, "Rejected"),
    };

    // F-C4: a lone text arrives with the offer; nothing is uploaded.
    if accepted.offer.files.is_empty() {
        if let Some(text) = accepted.offer.text.clone() {
            shared.ctx.emit(Event::TextReceived {
                transfer: accepted.transfer.id(),
                from: accepted.offer.sender.clone(),
                text,
            });
        }
        accepted.transfer.finish(Outcome::Done, Vec::new());
        return empty(StatusCode::NO_CONTENT);
    }

    let mut files = HashMap::with_capacity(ids.len());
    let mut tokens = HashMap::with_capacity(ids.len());
    for (id, file) in ids.into_iter().zip(accepted.offer.files.iter().cloned()) {
        let Ok(token) = wire::random_token() else {
            accepted.transfer.finish_with(Err(internal()));
            return status(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
        };
        tokens.insert(id.clone(), token.clone());
        files.insert(
            id,
            SessionFile {
                file,
                token,
                state: FileState::Pending,
            },
        );
    }
    let Ok(session_id) = wire::random_token() else {
        accepted.transfer.finish_with(Err(internal()));
        return status(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
    };
    let (uploads_tx, uploads_rx) = mpsc::channel(UPLOAD_QUEUE);
    let sender_cancel = CancellationToken::new();
    let activated = pending.activate(Active {
        session_id: session_id.clone(),
        sender: Party::of(caller),
        uploads: uploads_tx,
        cancel: sender_cancel.clone(),
    });
    if !activated {
        // Receiving was switched off in the instant the user said yes.
        accepted.transfer.finish_with(Err(cancelled()));
        return status(StatusCode::FORBIDDEN, "Cancelled");
    }
    let session = Session {
        inner: inner.clone(),
        transfer: accepted.transfer,
        files,
        uploads: uploads_rx,
        sender_cancel,
        session_id: session_id.clone(),
        sender: (sender_port != 0).then(|| {
            (
                SocketAddr::new(caller.ip, sender_port),
                caller.fingerprint.clone(),
            )
        }),
    };
    shared.tasks.spawn(session.run());
    json(
        StatusCode::OK,
        &PrepareUploadResponseDtoV2 {
            session_id,
            files: tokens,
        },
    )
}

/// The offer as the peer described it, and the file ids in the same order.
pub(super) fn raw_offer(dto: &PrepareUploadRequestDtoV2) -> (RawOffer, Vec<String>) {
    let mut raw = RawOffer::new(Protocol::LocalSend, dto.info.alias.clone());
    raw.model = dto.info.device_model.clone();
    // LocalSend sends a message as one text file whose `preview` is the
    // text; the receiver shows it and asks for nothing (F-C4).
    if dto.files.len() == 1
        && let Some(only) = dto.files.values().next()
        && only.preview.is_some()
        && only
            .file_type
            .trim()
            .to_ascii_lowercase()
            .starts_with("text/plain")
    {
        raw.text = only.preview.clone();
        return (raw, Vec::new());
    }
    // Sorted, so the consent dialog lists the same offer the same way.
    let mut entries: Vec<_> = dto.files.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut ids = Vec::with_capacity(entries.len());
    for (id, f) in entries {
        ids.push(id.clone());
        raw.files.push(RawFile {
            name: f.file_name.clone(),
            // Widened without loss; the core refuses what is too large.
            size: i128::from(f.size),
            mime: Some(f.file_type.clone()),
            sha256: f.sha256.clone(),
        });
    }
    (raw, ids)
}

async fn upload(inner: &Inner, caller: &Caller, req: Request<Incoming>) -> Reply {
    let query = req.uri().query();
    let (Some(session_id), Some(file_id), Some(token)) = (
        wire::query_value(query, "sessionId"),
        wire::query_value(query, "fileId"),
        wire::query_value(query, "token"),
    ) else {
        return status(StatusCode::BAD_REQUEST, "Missing parameters");
    };
    let uploads = match &*lock(&inner.slot) {
        Slot::Active(a) if a.sender.is(caller) && wire::same_secret(&a.session_id, &session_id) => {
            a.uploads.clone()
        }
        // No session, not this one, or not this sender: nothing is read.
        _ => return status(StatusCode::FORBIDDEN, "Invalid token or IP address"),
    };
    let (reply, answer) = oneshot::channel();
    let request = Upload {
        file_id,
        token,
        body: req.into_body(),
        reply,
    };
    if uploads.try_send(request).is_err() {
        return status(StatusCode::SERVICE_UNAVAILABLE, "Busy");
    }
    match answer.await {
        Ok(code) if code == StatusCode::OK => empty(StatusCode::OK),
        Ok(code) => status(code, "Upload failed"),
        Err(_) => status(StatusCode::FORBIDDEN, "Invalid token or IP address"),
    }
}

fn cancel(inner: &Inner, caller: &Caller, req: &Request<Incoming>) -> Reply {
    let session_id = wire::query_value(req.uri().query(), "sessionId");
    {
        let slot = lock(&inner.slot);
        match &*slot {
            // A sender does not know the session id before its offer is
            // answered, so a pending offer is cancelled by address alone.
            Slot::Pending { sender, cancel, .. } if sender.is(caller) => {
                cancel.cancel();
                return empty(StatusCode::OK);
            }
            Slot::Active(a)
                if a.sender.is(caller)
                    && session_id
                        .as_deref()
                        .is_some_and(|id| wire::same_secret(&a.session_id, id)) =>
            {
                a.cancel.cancel();
                return empty(StatusCode::OK);
            }
            _ => {}
        }
    }
    // Not ours: the peer may be cancelling something we are sending to it.
    if let Some(id) = session_id {
        inner.shared.remote_cancel(caller.ip, &id);
    }
    empty(StatusCode::OK)
}

async fn read_body<T: serde::de::DeserializeOwned>(
    inner: &Inner,
    body: Incoming,
) -> Result<T, Reply> {
    let opts = &inner.shared.opts;
    wire::read_json(body, opts.idle_timeout(), opts.handshake_timeout())
        .await
        .map_err(|e| match e {
            JsonError::Read(ReadError::TooLarge) => {
                status(StatusCode::PAYLOAD_TOO_LARGE, "Request too large")
            }
            JsonError::Read(ReadError::TimedOut) => status(StatusCode::REQUEST_TIMEOUT, "Timeout"),
            JsonError::Read(ReadError::Broken) | JsonError::Malformed => {
                status(StatusCode::BAD_REQUEST, "Invalid JSON body")
            }
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileState {
    Pending,
    Done,
    Failed,
}

struct SessionFile {
    file: OfferFile,
    token: String,
    state: FileState,
}

/// One accepted offer, receiving its files.
struct Session {
    inner: Arc<Inner>,
    transfer: TransferHandle,
    files: HashMap<String, SessionFile>,
    uploads: mpsc::Receiver<Upload>,
    sender_cancel: CancellationToken,
    session_id: String,
    /// Where the sender listens and who it is, to tell it when we cancel.
    sender: Option<(SocketAddr, String)>,
}

impl Session {
    async fn run(mut self) {
        let idle = self.inner.shared.opts.idle_timeout();
        let mut saved: Vec<String> = Vec::new();
        let mut failure: Option<ErrorInfo> = None;
        let mut end: Result<(), ErrorInfo> = Ok(());
        let mut by_sender = false;
        let mut notified = false;
        while self.files.values().any(|f| f.state == FileState::Pending) {
            let next = tokio::select! {
                biased;
                () = self.transfer.token().cancelled() => {
                    end = Err(cancelled());
                    break;
                }
                () = self.sender_cancel.cancelled() => {
                    by_sender = true;
                    end = Err(cancelled());
                    break;
                }
                next = tokio::time::timeout(idle, self.uploads.recv()) => next,
            };
            let upload = match next {
                Ok(Some(upload)) => upload,
                Ok(None) => {
                    end = Err(cancelled());
                    break;
                }
                Err(_) => {
                    end = Err(ErrorInfo::new(
                        ErrorCode::Network,
                        "the sender stopped sending",
                    ));
                    break;
                }
            };
            let Some(entry) = self.files.get_mut(&upload.file_id) else {
                let _ = upload.reply.send(StatusCode::FORBIDDEN);
                continue;
            };
            if entry.state != FileState::Pending || !wire::same_secret(&entry.token, &upload.token)
            {
                let _ = upload.reply.send(StatusCode::FORBIDDEN);
                continue;
            }
            let file = entry.file.clone();
            let result = receive_file(
                &self.inner,
                &self.transfer,
                &file,
                upload.body,
                &self.sender_cancel,
            )
            .await;
            let state = if result.is_ok() {
                FileState::Done
            } else {
                FileState::Failed
            };
            if let Some(entry) = self.files.get_mut(&upload.file_id) {
                entry.state = state;
            }
            match result {
                Ok(name) => {
                    saved.push(name);
                    let _ = upload.reply.send(StatusCode::OK);
                }
                Err(e) => {
                    let here = self.transfer.is_cancelled();
                    if here && !self.sender_cancel.is_cancelled() {
                        // Tell the sender before failing its upload, so it
                        // ends as cancelled rather than as a network error.
                        self.notify_sender().await;
                        notified = true;
                    }
                    let _ = upload.reply.send(error_status(&e));
                    if here || self.sender_cancel.is_cancelled() {
                        by_sender = !here;
                        end = Err(e);
                        break;
                    }
                    failure.get_or_insert(e);
                }
            }
        }

        // Anything still queued is refused; then the slot is free again.
        self.uploads.close();
        while let Ok(upload) = self.uploads.try_recv() {
            let _ = upload.reply.send(StatusCode::FORBIDDEN);
        }
        self.inner.release(&self.session_id);

        let cancelled_here = self.transfer.is_cancelled() && !by_sender;
        if by_sender {
            // The sender's cancel ends the transfer as cancelled, not failed.
            self.transfer.token().cancel();
        }
        if cancelled_here && !notified {
            self.notify_sender().await;
        }
        let outcome = match (end, failure) {
            (Err(_), _) if self.transfer.is_cancelled() => Outcome::Cancelled,
            (Err(error), _) | (Ok(()), Some(error)) => Outcome::Failed { error },
            (Ok(()), None) => Outcome::Done,
        };
        self.transfer.finish(outcome, saved);
    }

    /// Tells the sender we cancelled, as LocalSend receivers do: `POST
    /// /cancel` to its server, pinned to the certificate it offered with.
    /// Best effort and bounded; skipped when the engine is stopping.
    async fn notify_sender(&self) {
        let shared = &self.inner.shared;
        if shared.ctx.shutdown_token().is_cancelled() {
            return;
        }
        if let Some((addr, fingerprint)) = &self.sender {
            client::cancel(
                shared,
                &self.inner.identity,
                *addr,
                fingerprint,
                Some(&self.session_id),
            )
            .await;
        }
    }
}

/// Streams one upload body into the inbox. Returns the saved name.
async fn receive_file(
    inner: &Inner,
    transfer: &TransferHandle,
    file: &OfferFile,
    mut body: Incoming,
    sender_cancel: &CancellationToken,
) -> Result<String, ErrorInfo> {
    // A body that says up front it is not the declared size is refused
    // before a staging file exists.
    if let Some(len) = body.size_hint().exact()
        && len != file.size
    {
        return Err(ErrorInfo::new(
            ErrorCode::Network,
            "the upload is not the size the offer declared",
        ));
    }
    let idle = inner.shared.opts.idle_timeout();
    let mut incoming = inner.shared.ctx.begin_file(transfer, file).await?;
    loop {
        let frame = tokio::select! {
            biased;
            () = transfer.token().cancelled() => return Err(cancelled()),
            () = sender_cancel.cancelled() => return Err(cancelled()),
            frame = tokio::time::timeout(idle, body.frame()) => frame,
        };
        match frame {
            Err(_) => {
                return Err(ErrorInfo::new(
                    ErrorCode::Network,
                    "the sender stopped sending",
                ));
            }
            Ok(None) => break,
            Ok(Some(Err(_))) => {
                return Err(ErrorInfo::new(ErrorCode::Network, "the upload broke off"));
            }
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    incoming.write(&data).await?;
                }
            }
        }
    }
    let saved = incoming.commit().await?;
    Ok(saved.name.as_str().to_owned())
}

fn error_status(e: &ErrorInfo) -> StatusCode {
    match e.code {
        ErrorCode::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::Storage | ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Refused => StatusCode::FORBIDDEN,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn internal() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Internal, "no randomness for a session token")
}

impl Inner {
    fn is_session_sender(&self, ip: IpAddr) -> bool {
        matches!(&*lock(&self.slot), Slot::Active(a) if a.sender.ip == ip)
    }

    /// Frees the slot after the session `session_id`; stops a draining
    /// server.
    fn release(&self, session_id: &str) {
        let mut slot = lock(&self.slot);
        if matches!(&*slot, Slot::Active(a) if a.session_id == session_id) {
            *slot = Slot::Free;
            if !self.accepting.load(Ordering::Acquire) {
                self.closing.cancel();
            }
        }
    }
}

/// Holds the slot for a pending offer; frees it when dropped unless the
/// offer became a session.
struct PendingGuard<'a> {
    inner: &'a Inner,
    id: u64,
    cancel: CancellationToken,
    armed: bool,
}

impl<'a> PendingGuard<'a> {
    fn claim(inner: &'a Inner, sender: Party) -> Option<PendingGuard<'a>> {
        let mut slot = lock(&inner.slot);
        if !matches!(&*slot, Slot::Free) {
            return None;
        }
        let id = inner.next_pending.fetch_add(1, Ordering::Relaxed);
        let cancel = inner.closing.child_token();
        *slot = Slot::Pending {
            id,
            sender,
            cancel: cancel.clone(),
        };
        Some(PendingGuard {
            inner,
            id,
            cancel,
            armed: true,
        })
    }

    /// Turns the pending offer into an active session. False if the slot is
    /// no longer this offer's (receiving stopped meanwhile).
    fn activate(&mut self, active: Active) -> bool {
        let mut slot = lock(&self.inner.slot);
        let ours = matches!(&*slot, Slot::Pending { id, .. } if *id == self.id);
        if ours && !self.cancel.is_cancelled() {
            *slot = Slot::Active(active);
            self.armed = false;
            true
        } else {
            false
        }
    }
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut slot = lock(&self.inner.slot);
        if matches!(&*slot, Slot::Pending { id, .. } if *id == self.id) {
            *slot = Slot::Free;
            if !self.inner.accepting.load(Ordering::Acquire) {
                self.inner.closing.cancel();
            }
        }
    }
}

/// Counts one connection against its address.
struct IpGuard {
    inner: Arc<Inner>,
    ip: IpAddr,
}

impl IpGuard {
    fn claim(inner: &Arc<Inner>, ip: IpAddr) -> Option<IpGuard> {
        let mut per_ip = lock(&inner.per_ip);
        let n = per_ip.entry(ip).or_insert(0);
        if *n >= MAX_CONNECTIONS_PER_IP {
            return None;
        }
        *n = n.saturating_add(1);
        Some(IpGuard {
            inner: inner.clone(),
            ip,
        })
    }
}

impl Drop for IpGuard {
    fn drop(&mut self) {
        let mut per_ip = lock(&self.inner.per_ip);
        if let Some(n) = per_ip.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                per_ip.remove(&self.ip);
            }
        }
    }
}

/// F-LS4: wrong PINs, per address and in all.
#[derive(Default)]
struct PinFailures {
    per_ip: HashMap<IpAddr, (u32, Instant)>,
    global: Option<(u32, Instant)>,
}

#[derive(Debug, PartialEq, Eq)]
enum PinCheck {
    Ok,
    Missing,
    Wrong,
    Locked,
}

impl PinFailures {
    fn check(&mut self, ip: IpAddr, given: Option<&str>, pin: &str, now: Instant) -> PinCheck {
        let fresh = |since: Instant| now.saturating_duration_since(since) < PIN_WINDOW;
        if self.global.is_some_and(|(_, since)| !fresh(since)) {
            self.global = None;
        }
        self.per_ip.retain(|_, (_, since)| fresh(*since));
        // Locked out: even the right PIN is refused, or guessing would go on
        // at full speed with only the right answer let through.
        let locked_here = self
            .per_ip
            .get(&ip)
            .is_some_and(|(n, _)| *n >= PIN_FAILURES_PER_IP);
        let locked_all = self.global.is_some_and(|(n, _)| n >= PIN_FAILURES_GLOBAL);
        if locked_here || locked_all {
            return PinCheck::Locked;
        }
        let Some(given) = given else {
            return PinCheck::Missing;
        };
        if wire::same_secret(given, pin) {
            self.per_ip.remove(&ip);
            return PinCheck::Ok;
        }
        if !self.per_ip.contains_key(&ip) && self.per_ip.len() >= RATE_LIMIT_ENTRIES {
            // Full of fresh entries: forget the oldest. The global count
            // still bounds the guessing.
            if let Some(oldest) = self
                .per_ip
                .iter()
                .min_by_key(|(_, (_, since))| *since)
                .map(|(ip, _)| *ip)
            {
                self.per_ip.remove(&oldest);
            }
        }
        let entry = self.per_ip.entry(ip).or_insert((0, now));
        entry.0 = entry.0.saturating_add(1);
        let global = self.global.get_or_insert((0, now));
        global.0 = global.0.saturating_add(1);
        PinCheck::Wrong
    }
}

fn status(code: StatusCode, message: &str) -> Reply {
    json(
        code,
        &ErrorResponse {
            message: message.to_owned(),
        },
    )
}

fn empty(code: StatusCode) -> Reply {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = code;
    response
}

fn json<T: Serialize>(code: StatusCode, value: &T) -> Reply {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = code;
    response.headers_mut().insert(
        CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    response
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn pin_guessing_is_locked_out_per_address_and_in_all() {
        let mut p = PinFailures::default();
        let t0 = Instant::now();
        let a: IpAddr = "192.168.1.2".parse().unwrap();
        assert_eq!(p.check(a, None, "1234", t0), PinCheck::Missing);
        assert_eq!(p.check(a, Some("1234"), "1234", t0), PinCheck::Ok);
        for _ in 0..PIN_FAILURES_PER_IP {
            assert_eq!(p.check(a, Some("0000"), "1234", t0), PinCheck::Wrong);
        }
        assert_eq!(
            p.check(a, Some("1234"), "1234", t0),
            PinCheck::Locked,
            "even the right PIN, while locked"
        );
        assert_eq!(
            p.check(a, Some("1234"), "1234", t0 + PIN_WINDOW),
            PinCheck::Ok,
            "the lockout ends"
        );

        let mut p = PinFailures::default();
        for i in 0..PIN_FAILURES_GLOBAL {
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, u8::try_from(i).unwrap()));
            assert_eq!(p.check(ip, Some("x"), "1234", t0), PinCheck::Wrong);
        }
        let fresh: IpAddr = "10.9.9.9".parse().unwrap();
        assert_eq!(p.check(fresh, Some("1234"), "1234", t0), PinCheck::Locked);
    }

    #[test]
    fn pin_memory_is_bounded() {
        let mut p = PinFailures::default();
        let t0 = Instant::now();
        for i in 0..2000u32 {
            let ip = IpAddr::V4(Ipv4Addr::from(0x0A00_0000 | i));
            p.check(ip, Some("x"), "1234", t0);
        }
        assert!(p.per_ip.len() <= RATE_LIMIT_ENTRIES);
    }

    proptest::proptest! {
        /// Whatever a peer puts in an offer, the core either refuses it or
        /// makes it safe, and every file keeps its id.
        #[test]
        fn any_offer_is_refused_or_made_safe(
            files in proptest::collection::vec(
                (".{0,40}", ".{0,300}", proptest::num::u64::ANY, ".{0,20}", proptest::option::of(".{0,80}")),
                0..20,
            ),
            alias in ".{0,100}",
        ) {
            use localsend::model::transfer::FileDto;
            use sukkula_core::offer::Offer;
            let mut dto: PrepareUploadRequestDtoV2 = serde_json::from_str(
                r#"{"info":{"alias":"","version":"2.2","fingerprint":"x","port":1,"protocol":"https"},"files":{}}"#,
            ).unwrap();
            dto.info.alias = alias;
            for (id, name, size, mime, preview) in files {
                dto.files.insert(id.clone(), FileDto {
                    id, file_name: name, size, file_type: mime, sha256: None, preview, metadata: None,
                });
            }
            let (raw, ids) = raw_offer(&dto);
            proptest::prop_assert_eq!(ids.len(), raw.files.len());
            if let Ok(offer) = Offer::validate(raw) {
                proptest::prop_assert_eq!(offer.files.len(), ids.len());
                for f in &offer.files {
                    proptest::prop_assert!(sukkula_core::name::is_safe(f.name.as_str()));
                }
            }
        }
    }

    #[test]
    fn a_lone_text_file_with_a_preview_is_a_message() {
        use localsend::model::transfer::FileDto;
        let mut dto: PrepareUploadRequestDtoV2 = serde_json::from_str(
            r#"{"info":{"alias":"A","version":"2.2","fingerprint":"x","port":1,"protocol":"https"},"files":{}}"#,
        )
        .unwrap();
        let text = FileDto {
            id: "t".into(),
            file_name: "m.txt".into(),
            size: 2,
            file_type: "text/plain".into(),
            sha256: None,
            preview: Some("hi".into()),
            metadata: None,
        };
        dto.files.insert("t".into(), text.clone());
        let (raw, ids) = raw_offer(&dto);
        assert_eq!(raw.text.as_deref(), Some("hi"));
        assert!(raw.files.is_empty() && ids.is_empty());

        let mut other = text.clone();
        other.preview = None;
        dto.files.insert("u".into(), other);
        let (raw, ids) = raw_offer(&dto);
        assert_eq!(raw.text, None, "with other files it is just a file");
        assert_eq!(ids, vec!["t".to_owned(), "u".to_owned()]);
        assert_eq!(raw.files.len(), 2);
        assert_eq!(raw.files[0].size, 2);
    }
}
