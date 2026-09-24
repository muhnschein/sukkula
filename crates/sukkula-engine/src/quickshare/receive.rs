//! Receiving: the listener, and one task per connection.
//!
//! The order of things on a connection is the receive path of
//! [`crate::ctx`], with nothing written or buffered beyond one bounded
//! frame before the user says yes:
//!
//! 1. the address is checked before a byte is read (S7);
//! 2. the handshake runs to the sender's introduction, within
//!    [`Timeouts::handshake`](super::Timeouts);
//! 3. the introduction goes to the user through [`Ctx::offer`], with the
//!    PIN both screens show (F-QS3); meanwhile the connection is read only
//!    to answer keep-alives and to notice the sender giving up, and rqs_lib
//!    refuses any payload byte (S5);
//! 4. on a yes, each file's chunks go into the inbox (S1, S3) and a text is
//!    reported once complete (F-C4).

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use rqs_lib::hdl::info::Introduction;
use rqs_lib::sharing_nearby::connection_response_frame::Status;
use rqs_lib::{DeviceType, InboundEvent, InboundRequest, MDnsServer};
use sukkula_core::Protocol;
use sukkula_core::consent::Refusal;
use sukkula_core::offer::{RawFile, RawOffer};
use sukkula_core::text;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::{Running, Shared, Slot, lock, new_endpoint_id, within};
use crate::api::{ErrorCode, ErrorInfo, Event, Outcome};
use crate::ctx::{Accepted, Declined, ReceivingFile, idle_timeout};

/// How long to back off after a failed `accept` (out of file descriptors,
/// say) rather than spinning on it.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(250);

/// How long a goodbye (a refusal, a cancel, a disconnection frame) may
/// take. The peer may have stopped reading; nobody waits on it for long.
const GOODBYE: Duration = Duration::from_secs(2);

/// Most files of one transfer open for writing at once. Senders send one
/// file after another; this only bounds a sender that interleaves.
const MAX_OPEN_FILES: usize = 4;

/// Longest preview of an offered text kept for the consent dialog. The
/// text itself comes after acceptance and is limited on its own.
const MAX_TEXT_PREVIEW_BYTES: usize = 1024;

/// What an offered text is called when its preview shows nothing.
const TEXT_PLACEHOLDER: &str = "Text";

/// Starts listening, and announcing when mDNS is on.
pub(super) async fn start(shared: &Arc<Shared>) -> Result<Running, ErrorInfo> {
    let unavailable = || ErrorInfo::new(ErrorCode::Network, "cannot listen for Quick Share");
    let listener = TcpListener::bind(shared.options.listen)
        .await
        .map_err(|_| unavailable())?;
    let port = listener.local_addr().map_err(|_| unavailable())?.port();
    let token = shared.ctx.shutdown_token().child_token();
    let endpoint = new_endpoint_id();

    let mut tasks = Vec::new();
    if shared.options.mdns {
        let server = MDnsServer::new(
            endpoint,
            port,
            &shared.ctx.device_name(),
            DeviceType::Phone,
            shared.addr_filter(),
        )
        .map_err(|_| ErrorInfo::new(ErrorCode::Network, "cannot announce over mDNS"))?;
        let t = token.clone();
        tasks.push(tokio::spawn(async move {
            let mut server = server;
            if let Err(e) = server.run(t).await {
                tracing::debug!(error = %e, "quickshare: mDNS announcement ended");
            }
        }));
    }
    *lock(&shared.own_endpoint) = Some(endpoint);
    tasks.push(tokio::spawn(accept_loop(
        shared.clone(),
        listener,
        token.clone(),
    )));
    Ok(Running {
        token,
        tasks,
        port: Some(port),
    })
}

async fn accept_loop(shared: Arc<Shared>, listener: TcpListener, token: CancellationToken) {
    loop {
        let accepted = tokio::select! {
            () = token.cancelled() => return,
            r = listener.accept() => r,
        };
        let (socket, peer) = match accepted {
            Ok(a) => a,
            Err(e) => {
                tracing::debug!(error = %e, "quickshare: accept failed");
                tokio::select! {
                    () = token.cancelled() => return,
                    () = tokio::time::sleep(ACCEPT_BACKOFF) => continue,
                }
            }
        };
        let ip = canonical(peer.ip());
        // S7, before a byte is read. Dropping `socket` closes it.
        if !shared.ctx.permits(ip) {
            tracing::debug!("quickshare: connection from an address outside the LAN dropped");
            continue;
        }
        let Some(slot) = Slot::take(&shared, ip) else {
            tracing::debug!("quickshare: too many connections; one dropped");
            continue;
        };
        if !shared.ctx.allow_offer(ip) {
            tracing::debug!("quickshare: connection over the rate limit dropped");
            continue;
        }
        let _ = socket.set_nodelay(true);
        let shared = shared.clone();
        let receiving = token.clone();
        // Detached: bounded by the handshake and consent timeouts before
        // acceptance, by the transfer's token (the user, the engine) after.
        drop(tokio::spawn(connection(shared, socket, slot, receiving)));
    }
}

/// An IPv4-mapped IPv6 address as the IPv4 address it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

async fn connection(
    shared: Arc<Shared>,
    socket: TcpStream,
    _slot: Slot,
    receiving: CancellationToken,
) {
    let timeouts = shared.options.timeouts;
    let mut ir = InboundRequest::new(socket);

    let introduction = tokio::select! {
        () = receiving.cancelled() => return,
        r = tokio::time::timeout(timeouts.handshake, handshake(&mut ir)) => match r {
            Ok(Some(i)) => i,
            Ok(None) => return,
            Err(_) => {
                tracing::debug!("quickshare: handshake timed out");
                return;
            }
        },
    };

    let files: Vec<i64> = introduction.files.iter().map(|f| f.payload_id).collect();
    let text_id = introduction.text.as_ref().map(|t| t.payload_id);
    let raw = raw_offer(&ir, &introduction);
    let accepted = match consent(&shared, &mut ir, raw, &receiving).await {
        Consent::Accepted(a) => a,
        Consent::Declined(status) => {
            let _ = within(GOODBYE, ir.reject_transfer(Some(status))).await;
            let _ = within(GOODBYE, ir.disconnection()).await;
            return;
        }
        Consent::Gone => return,
    };

    let result = receive(&shared, &mut ir, &accepted, &files, text_id).await;
    match result {
        Ok(saved) => accepted.transfer.finish(Outcome::Done, saved),
        Err(Ended::CancelledHere) => {
            // F-C5: tell the sender, briefly.
            let _ = within(GOODBYE, ir.cancel_transfer()).await;
            accepted.transfer.finish(Outcome::Cancelled, Vec::new());
        }
        Err(Ended::CancelledThere) => accepted.transfer.finish(Outcome::Cancelled, Vec::new()),
        Err(Ended::Failed(e)) => {
            let _ = within(GOODBYE, ir.cancel_transfer()).await;
            accepted.transfer.finish_with(Err(e));
        }
    }
}

/// Runs the handshake to the sender's introduction. `None` on anything
/// else: an error, a cancel, a hang-up.
async fn handshake(ir: &mut InboundRequest<TcpStream>) -> Option<Introduction> {
    loop {
        match ir.next_event().await {
            Ok(None) => {}
            Ok(Some(InboundEvent::Introduction(i))) => return Some(i),
            Ok(Some(_)) => return None,
            Err(e) => {
                tracing::debug!(error = %e, "quickshare: handshake failed");
                return None;
            }
        }
    }
}

/// The offer as the sender described it, for [`Ctx::offer`] to check.
/// Sizes are widened, not converted, so a negative one reaches the check.
fn raw_offer(ir: &InboundRequest<TcpStream>, introduction: &Introduction) -> RawOffer {
    let sender = ir
        .remote_device_info()
        .map(|r| r.name.clone())
        .unwrap_or_default();
    let mut raw = RawOffer::new(Protocol::QuickShare, sender);
    raw.files = introduction
        .files
        .iter()
        .map(|f| RawFile {
            name: f.name.clone(),
            size: i128::from(f.size),
            mime: Some(f.mime_type.clone()),
            sha256: None,
        })
        .collect();
    raw.text = introduction.text.as_ref().map(|t| {
        let preview = text::truncate_bytes(&t.title, MAX_TEXT_PREVIEW_BYTES);
        if text::message(preview).is_empty() {
            TEXT_PLACEHOLDER.to_owned()
        } else {
            preview.to_owned()
        }
    });
    raw.pin = ir.pin_code().map(str::to_owned);
    raw
}

enum Consent {
    Accepted(Box<Accepted>),
    Declined(Status),
    /// The sender went away, cancelled, or broke the protocol.
    Gone,
}

/// Asks the user, reading the connection meanwhile for keep-alives and
/// the sender giving up. Dropping the offer's future (by returning) takes
/// it off the screen.
async fn consent(
    shared: &Arc<Shared>,
    ir: &mut InboundRequest<TcpStream>,
    raw: RawOffer,
    receiving: &CancellationToken,
) -> Consent {
    let idle = shared.options.timeouts.idle;
    let offer = shared.ctx.offer(raw);
    tokio::pin!(offer);
    loop {
        tokio::select! {
            biased;
            () = receiving.cancelled() => return Consent::Declined(Status::Reject),
            r = &mut offer => {
                return match r {
                    Ok(a) => Consent::Accepted(Box::new(a)),
                    Err(d) => Consent::Declined(status_for(&d)),
                };
            }
            // Cancel-safe: a frame half read when the answer comes stays
            // buffered for the receive loop.
            f = ir.read_frame() => {
                let Ok(frame) = f else { return Consent::Gone };
                match within(idle, ir.process_frame(frame)).await {
                    Ok(None) => {}
                    Ok(Some(_)) | Err(_) => return Consent::Gone,
                }
            }
        }
    }
}

/// What the sender is told when the offer does not go ahead.
fn status_for(declined: &Declined) -> Status {
    match declined {
        Declined::NoSpace => Status::NotEnoughSpace,
        Declined::Refused(Refusal::TimedOut) => Status::TimedOut,
        Declined::Invalid(_) | Declined::Refused(_) | Declined::Busy => Status::Reject,
    }
}

enum Ended {
    /// Cancelled here: by the user, or the engine stopping.
    CancelledHere,
    /// The sender cancelled.
    CancelledThere,
    Failed(ErrorInfo),
}

fn protocol_error() -> Ended {
    Ended::Failed(ErrorInfo::new(
        ErrorCode::Network,
        "the sender broke the protocol",
    ))
}

/// Receives an accepted transfer: every file into the inbox, the text into
/// an event. The names the files were saved under, on success.
async fn receive(
    shared: &Arc<Shared>,
    ir: &mut InboundRequest<TcpStream>,
    accepted: &Accepted,
    files: &[i64],
    text_id: Option<i64>,
) -> Result<Vec<String>, Ended> {
    let idle = shared.options.timeouts.idle;
    let transfer = &accepted.transfer;
    within(idle, ir.accept_transfer())
        .await
        .map_err(Ended::Failed)?;

    let mut pending: HashSet<i64> = files.iter().copied().collect();
    let mut text_pending = text_id.is_some();
    let mut open: HashMap<i64, ReceivingFile<'_>> = HashMap::new();
    let mut saved = Vec::with_capacity(files.len());

    while !pending.is_empty() || text_pending {
        let frame = tokio::select! {
            () = transfer.token().cancelled() => return Err(Ended::CancelledHere),
            f = tokio::time::timeout(idle, ir.read_frame()) => f,
        };
        let frame = match frame {
            Ok(Ok(f)) => f,
            Ok(Err(_)) => {
                return Err(Ended::Failed(ErrorInfo::new(
                    ErrorCode::Network,
                    "the sender hung up",
                )));
            }
            Err(_) => {
                return Err(Ended::Failed(ErrorInfo::new(
                    ErrorCode::Network,
                    "the sender stopped responding",
                )));
            }
        };
        let event = match within(idle, ir.process_frame(frame)).await {
            Ok(e) => e,
            Err(_) if transfer.is_cancelled() => return Err(Ended::CancelledHere),
            Err(_) => return Err(protocol_error()),
        };
        match event {
            None => {}
            Some(InboundEvent::FileChunk(chunk)) => {
                if !pending.contains(&chunk.payload_id) {
                    return Err(protocol_error());
                }
                if !open.contains_key(&chunk.payload_id) {
                    if open.len() >= MAX_OPEN_FILES {
                        return Err(protocol_error());
                    }
                    let file = files
                        .iter()
                        .position(|id| *id == chunk.payload_id)
                        .and_then(|i| accepted.offer.files.get(i))
                        .ok_or_else(protocol_error)?;
                    // The inbox's calls are file system calls: a stuck one
                    // (a full or hung file system) is given up on like a
                    // stuck peer (S6).
                    let incoming = idle_timeout(shared.ctx.begin_file(transfer, file))
                        .await
                        .and_then(|r| r)
                        .map_err(Ended::Failed)?;
                    open.insert(chunk.payload_id, incoming);
                }
                let incoming = open.get_mut(&chunk.payload_id).ok_or_else(protocol_error)?;
                idle_timeout(incoming.write(&chunk.body))
                    .await
                    .and_then(|r| r)
                    .map_err(|e| {
                        if transfer.is_cancelled() {
                            Ended::CancelledHere
                        } else {
                            Ended::Failed(e)
                        }
                    })?;
                if chunk.last {
                    let incoming = open.remove(&chunk.payload_id).ok_or_else(protocol_error)?;
                    let placed = idle_timeout(incoming.commit())
                        .await
                        .and_then(|r| r)
                        .map_err(Ended::Failed)?;
                    saved.push(placed.name.as_str().to_owned());
                    pending.remove(&chunk.payload_id);
                }
            }
            Some(InboundEvent::Text { payload_id, text }) => {
                if !text_pending || Some(payload_id) != text_id {
                    return Err(protocol_error());
                }
                text_pending = false;
                // F-C4: plain text after S2, shown with a Copy button and
                // never opened, URLs included.
                shared.ctx.emit(Event::TextReceived {
                    transfer: transfer.id(),
                    from: accepted.offer.sender.clone(),
                    text: text::message(&text),
                });
            }
            Some(InboundEvent::Cancelled) => return Err(Ended::CancelledThere),
            Some(InboundEvent::Disconnected) => {
                return Err(Ended::Failed(ErrorInfo::new(
                    ErrorCode::Network,
                    "the sender hung up before the end",
                )));
            }
            Some(InboundEvent::Introduction(_)) => return Err(protocol_error()),
        }
    }
    let _ = within(GOODBYE, ir.disconnection()).await;
    Ok(saved)
}
