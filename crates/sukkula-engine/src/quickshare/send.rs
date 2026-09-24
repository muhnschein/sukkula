//! Sending (F-QS1): files, or one text, to a peer that discovery found.
//!
//! The receiver is as hostile as any sender: its frames are bounded and
//! checked by rqs_lib the same way, every read and write here has a
//! timeout, and it is never sent more than was announced -- at most each
//! file's checked size, read in [`IO_CHUNK_BYTES`] chunks.

use std::net::SocketAddr;
use std::sync::Arc;

use rqs_lib::hdl::TextPayloadType;
use rqs_lib::hdl::info::{OutgoingFile as QsFile, OutgoingText};
use rqs_lib::{OutboundEvent, OutboundPayload, OutboundRequest};
use sukkula_core::Protocol;
use sukkula_core::limits::{IO_CHUNK_BYTES, MAX_ALIAS_CHARS};
use sukkula_core::text;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{Shared, Timeouts, discovery, new_endpoint_id, within};
use crate::adapter::{Outgoing, OutgoingFile};
use crate::api::{Direction, ErrorCode, ErrorInfo, TransferId};
use crate::ctx::{TransferHandle, cancelled};

/// How long the end of a transfer may take: rqs_lib lingers up to ten
/// seconds for the receiver to hang up first (see `OutboundRequest::finish`).
const FINISH_WAIT: std::time::Duration = std::time::Duration::from_secs(12);

/// How long a goodbye may take.
const GOODBYE: std::time::Duration = std::time::Duration::from_secs(2);

/// Most frames from the receiver handled between two chunks.
const MAX_FRAMES_PER_POLL: usize = 16;

/// What goes over the wire.
enum Payload {
    Files(Vec<OutgoingFile>),
    Text(String),
}

/// Starts a send to the peer `peer_id` and returns its transfer.
pub(super) fn start(
    shared: &Arc<Shared>,
    peer_id: &str,
    items: Vec<Outgoing>,
) -> Result<TransferId, ErrorInfo> {
    let entry = discovery::lookup(shared, peer_id)
        .ok_or_else(|| ErrorInfo::new(ErrorCode::NotFound, "no such Quick Share device"))?;
    if !shared.ctx.permits(entry.addr.ip()) {
        return Err(ErrorInfo::new(
            ErrorCode::NotFound,
            "no such Quick Share device",
        ));
    }
    let views = items.iter().map(Outgoing::view).collect::<Vec<_>>();
    let total = views
        .iter()
        .fold(0u64, |acc, v| acc.saturating_add(v.size));
    let payload = payload(items)?;
    let handle = shared
        .ctx
        .transfers()
        .begin_views(
            &shared.ctx,
            Direction::Outgoing,
            Protocol::QuickShare,
            &entry.peer.name,
            views,
            total,
        )
        .ok_or_else(|| ErrorInfo::new(ErrorCode::TooLarge, "too many transfers running"))?;
    let id = handle.id();
    let shared = shared.clone();
    // Detached: bounded by the timeouts and by the transfer's token, and it
    // finishes the handle itself. Were it to panic, dropping the handle
    // reports the transfer as failed.
    drop(tokio::spawn(async move {
        let result = run(&shared, entry.addr, payload, &handle).await;
        handle.finish_with(result.map(|()| Vec::new()));
    }));
    Ok(id)
}

/// Files, or exactly one text: what a Quick Share introduction carries and
/// receivers accept.
fn payload(items: Vec<Outgoing>) -> Result<Payload, ErrorInfo> {
    let mixed = || {
        ErrorInfo::new(
            ErrorCode::Unavailable,
            "Quick Share sends files, or one text, at a time",
        )
    };
    let mut files = Vec::new();
    let mut texts = Vec::new();
    for item in items {
        match item {
            Outgoing::File(f) => files.push(f),
            Outgoing::Text(t) => texts.push(t),
        }
    }
    match (files.is_empty(), texts.len()) {
        (false, 0) => Ok(Payload::Files(files)),
        (true, 1) => texts.pop().map(Payload::Text).ok_or_else(mixed),
        _ => Err(mixed()),
    }
}

async fn run(
    shared: &Shared,
    addr: SocketAddr,
    payload: Payload,
    handle: &TransferHandle,
) -> Result<(), ErrorInfo> {
    let timeouts = shared.options.timeouts;
    let token = handle.token().clone();
    let socket = tokio::select! {
        () = token.cancelled() => return Err(cancelled()),
        r = tokio::time::timeout(timeouts.connect, TcpStream::connect(addr)) => match r {
            Ok(Ok(s)) => s,
            _ => return Err(ErrorInfo::new(ErrorCode::Network, "could not reach the device")),
        },
    };
    let _ = socket.set_nodelay(true);

    let wire = match &payload {
        Payload::Files(files) => OutboundPayload::Files(
            files
                .iter()
                .map(|f| QsFile {
                    name: f.name.as_str().to_owned(),
                    size: i64::try_from(f.size).unwrap_or(i64::MAX),
                    mime_type: f
                        .mime
                        .clone()
                        .unwrap_or_else(|| "application/octet-stream".to_owned()),
                })
                .collect(),
        ),
        Payload::Text(t) => OutboundPayload::Text(OutgoingText {
            kind: TextPayloadType::Text,
            title: text::display(t, MAX_ALIAS_CHARS),
            text: t.clone(),
        }),
    };
    let mut or = OutboundRequest::new(new_endpoint_id(), socket, shared.ctx.device_name(), wire);
    let result = session(&mut or, &timeouts, &token, &payload, handle).await;
    if result.is_err() && token.is_cancelled() {
        // F-C5: tell the receiver, briefly.
        let _ = within(GOODBYE, or.cancel_transfer()).await;
    }
    result
}

async fn session(
    or: &mut OutboundRequest<TcpStream>,
    timeouts: &Timeouts,
    token: &CancellationToken,
    payload: &Payload,
    handle: &TransferHandle,
) -> Result<(), ErrorInfo> {
    within(timeouts.idle, or.send_connection_request()).await?;
    within(timeouts.idle, or.send_ukey2_client_init()).await?;

    // The handshake, to our introduction.
    let deadline = Instant::now()
        .checked_add(timeouts.handshake)
        .unwrap_or_else(Instant::now);
    loop {
        match next_event(or, timeouts, token, deadline).await? {
            Some(OutboundEvent::IntroductionSent) => break,
            None => {}
            Some(other) => return Err(ended(&other)),
        }
    }

    // The receiver's user decides.
    let deadline = Instant::now()
        .checked_add(timeouts.send_consent)
        .unwrap_or_else(Instant::now);
    loop {
        match next_event(or, timeouts, token, deadline).await? {
            Some(OutboundEvent::Accepted) => break,
            None | Some(OutboundEvent::IntroductionSent) => {}
            Some(other) => return Err(ended(&other)),
        }
    }

    match payload {
        Payload::Files(files) => {
            let ids = or.file_ids().to_vec();
            if ids.len() != files.len() {
                return Err(ErrorInfo::new(ErrorCode::Internal, "file ids out of step"));
            }
            for (id, file) in ids.into_iter().zip(files) {
                send_file(or, timeouts, token, handle, id, file).await?;
            }
        }
        Payload::Text(t) => {
            within(timeouts.idle, or.send_text()).await?;
            handle.add_progress(u64::try_from(t.len()).unwrap_or(u64::MAX));
        }
    }
    within(FINISH_WAIT, or.finish()).await
}

/// The next frame from the receiver, processed; before `deadline`, and
/// giving way to the transfer's token.
async fn next_event(
    or: &mut OutboundRequest<TcpStream>,
    timeouts: &Timeouts,
    token: &CancellationToken,
    deadline: Instant,
) -> Result<Option<OutboundEvent>, ErrorInfo> {
    let frame = tokio::select! {
        () = token.cancelled() => return Err(cancelled()),
        f = tokio::time::timeout_at(deadline, or.read_frame()) => f,
    };
    let frame = match frame {
        Ok(Ok(f)) => f,
        Ok(Err(_)) => {
            return Err(ErrorInfo::new(
                ErrorCode::Network,
                "the device hung up",
            ));
        }
        Err(_) => {
            return Err(ErrorInfo::new(
                ErrorCode::Network,
                "the device stopped responding",
            ));
        }
    };
    within(timeouts.idle, or.process_frame(frame)).await
}

/// The error for a transfer the receiver ended.
fn ended(event: &OutboundEvent) -> ErrorInfo {
    match event {
        OutboundEvent::Rejected(_) => ErrorInfo::new(ErrorCode::Refused, "the device declined"),
        OutboundEvent::Cancelled => ErrorInfo::new(ErrorCode::Refused, "the device cancelled"),
        OutboundEvent::Disconnected
        | OutboundEvent::Accepted
        | OutboundEvent::IntroductionSent => {
            ErrorInfo::new(ErrorCode::Network, "the device hung up")
        }
    }
}

/// Handles whatever the receiver sent meanwhile -- keep-alives, a cancel --
/// without waiting for more.
async fn poll_incoming(
    or: &mut OutboundRequest<TcpStream>,
    timeouts: &Timeouts,
) -> Result<(), ErrorInfo> {
    for _ in 0..MAX_FRAMES_PER_POLL {
        // `read_frame` is cancel-safe, so polling it once and giving up
        // loses nothing.
        let frame = tokio::select! {
            biased;
            f = or.read_frame() => f,
            () = std::future::ready(()) => return Ok(()),
        };
        let frame =
            frame.map_err(|_| ErrorInfo::new(ErrorCode::Network, "the device hung up"))?;
        match within(timeouts.idle, or.process_frame(frame)).await? {
            None | Some(OutboundEvent::Accepted | OutboundEvent::IntroductionSent) => {}
            Some(other) => return Err(ended(&other)),
        }
    }
    Ok(())
}

async fn send_file(
    or: &mut OutboundRequest<TcpStream>,
    timeouts: &Timeouts,
    token: &CancellationToken,
    handle: &TransferHandle,
    id: i64,
    file: &OutgoingFile,
) -> Result<(), ErrorInfo> {
    let unreadable = || ErrorInfo::new(ErrorCode::BadFile, "a file could not be read");
    let mut source = tokio::fs::File::open(&file.path)
        .await
        .map_err(|_| unreadable())?;
    let mut buf = vec![0u8; IO_CHUNK_BYTES];
    let mut remaining = file.size;
    while remaining > 0 {
        if token.is_cancelled() {
            return Err(cancelled());
        }
        poll_incoming(or, timeouts).await?;
        let want = usize::try_from(remaining)
            .unwrap_or(IO_CHUNK_BYTES)
            .min(IO_CHUNK_BYTES);
        let chunk = buf.get_mut(..want).ok_or_else(unreadable)?;
        let n = source.read(chunk).await.map_err(|_| unreadable())?;
        if n == 0 {
            return Err(ErrorInfo::new(
                ErrorCode::BadFile,
                "a file got shorter while it was sent",
            ));
        }
        let body = chunk.get(..n).ok_or_else(unreadable)?;
        within(timeouts.idle, or.send_file_chunk(id, body)).await?;
        let sent = u64::try_from(n).unwrap_or(u64::MAX);
        remaining = remaining.saturating_sub(sent);
        handle.add_progress(sent);
    }
    within(timeouts.idle, or.finish_file(id)).await
}
