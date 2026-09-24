//! Sending (F-LS3): offer, then stream each accepted file, over one TLS
//! connection pinned to the fingerprint the peer announced.
//!
//! A file is read in [`IO_CHUNK_BYTES`] chunks and never past the size it
//! had when the hub checked it: a file that grows is sent as it was, one
//! that shrinks fails the transfer rather than sending a short body. Every
//! chunk handed to the connection waits at most the idle timeout, so a
//! receiver that stops reading ends the transfer instead of holding it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use hyper::{Method, StatusCode};
use localsend::http::dto_v2::{PrepareUploadRequestDtoV2, PrepareUploadResponseDtoV2};
use localsend::model::transfer::FileDto;
use sukkula_core::limits::IO_CHUNK_BYTES;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use super::Shared;
use super::client::{self, Conn, Failure};
use super::identity::Identity;
use super::peers::Target;
use super::wire::{self, OutBody};
use crate::adapter::Outgoing;
use crate::api::{ErrorCode, ErrorInfo};
use crate::ctx::{TransferHandle, cancelled};

/// What the MIME type of a file without one is announced as.
const OCTET_STREAM: &str = "application/octet-stream";

/// One thing to upload.
enum Content {
    File { path: PathBuf, size: u64 },
    Text(Bytes),
}

impl Content {
    fn len(&self) -> u64 {
        match self {
            Content::File { size, .. } => *size,
            Content::Text(b) => u64::try_from(b.len()).unwrap_or(u64::MAX),
        }
    }
}

/// Runs one send to its end and reports it.
pub(super) async fn run(
    shared: Arc<Shared>,
    identity: Arc<Identity>,
    target: Target,
    items: Vec<Outgoing>,
    transfer: TransferHandle,
) {
    let id = transfer.id();
    shared.outgoing_begin(id, target.addr.ip());
    let token = transfer.token().clone();
    let mut remote_session: Option<String> = None;
    let result = tokio::select! {
        biased;
        () = token.cancelled() => Err(cancelled()),
        result = deliver(&shared, &identity, &target, items, &transfer, &mut remote_session) => result,
    };
    let by_peer = shared.outgoing_end(id);
    // Tell the receiver when we give up, as LocalSend senders do: with the
    // session id once there is one, by address alone while the offer is
    // pending. A failure after the offer was accepted counts too, so the
    // receiver ends at once rather than at its idle timeout. Not when the
    // receiver cancelled, nor when the certificate was wrong (that is not
    // the peer to tell), nor while the engine stops.
    let failed_mid_session = result
        .as_ref()
        .is_err_and(|e| e.code != ErrorCode::PeerMismatch)
        && remote_session.is_some();
    if (transfer.is_cancelled() || failed_mid_session)
        && !by_peer
        && !shared.ctx.shutdown_token().is_cancelled()
    {
        client::cancel(
            &shared,
            &identity,
            target.addr,
            &target.fingerprint,
            remote_session.as_deref(),
        )
        .await;
    }
    transfer.finish_with(result.map(|()| Vec::new()));
}

async fn deliver(
    shared: &Arc<Shared>,
    identity: &Identity,
    target: &Target,
    items: Vec<Outgoing>,
    transfer: &TransferHandle,
    remote_session: &mut Option<String>,
) -> Result<(), ErrorInfo> {
    let mut files = HashMap::with_capacity(items.len());
    let mut contents: Vec<(String, Content)> = Vec::with_capacity(items.len());
    for (i, item) in items.into_iter().enumerate() {
        let id = i.to_string();
        let (dto, content) = match item {
            Outgoing::File(f) => (
                FileDto {
                    id: id.clone(),
                    file_name: f.name.as_str().to_owned(),
                    size: f.size,
                    file_type: f.mime.unwrap_or_else(|| OCTET_STREAM.to_owned()),
                    sha256: None,
                    preview: None,
                    metadata: None,
                },
                Content::File {
                    path: f.path,
                    size: f.size,
                },
            ),
            // LocalSend's form of a message: a text file whose preview is
            // the text. Alone, the receiver shows it and asks for nothing;
            // with files, it may ask for it as a file.
            Outgoing::Text(t) => (
                FileDto {
                    id: id.clone(),
                    file_name: format!("message-{i}.txt"),
                    size: u64::try_from(t.len()).unwrap_or(u64::MAX),
                    file_type: "text/plain".to_owned(),
                    sha256: None,
                    preview: Some(t.clone()),
                    metadata: None,
                },
                Content::Text(Bytes::from(t)),
            ),
        };
        files.insert(id.clone(), dto);
        contents.push((id, content));
    }

    let mut conn = open(shared, identity, target).await?;
    let offer = PrepareUploadRequestDtoV2 {
        info: shared.register_dto(identity),
        files,
    };
    let path = format!("{}/prepare-upload", wire::API_V2);
    let response = conn
        .call(
            Method::POST,
            &path,
            OutBody::json(&offer),
            shared.opts.prepare_timeout(),
        )
        .await
        .map_err(Failure::into_error)?;
    match response.status() {
        StatusCode::OK => {}
        // Accepted, nothing to upload: a message on its own.
        StatusCode::NO_CONTENT => return Ok(()),
        other => {
            client::drain(shared, response).await;
            return Err(Failure::Status(other).into_error());
        }
    }
    let answer: PrepareUploadResponseDtoV2 = client::json(shared, response)
        .await
        .map_err(Failure::into_error)?;
    let session = wire::token(&answer.session_id)
        .ok_or_else(|| malformed("session id"))?
        .to_owned();
    *remote_session = Some(session.clone());
    shared.outgoing_session(transfer.id(), &session);

    for (id, content) in contents {
        // Files the receiver did not ask for were declined there.
        let Some(token) = answer.files.get(&id) else {
            continue;
        };
        let token = wire::token(token).ok_or_else(|| malformed("file token"))?;
        if conn.closed() {
            conn = open(shared, identity, target).await?;
        }
        upload(shared, &mut conn, &session, &id, token, content, transfer).await?;
    }
    Ok(())
}

async fn open(
    shared: &Arc<Shared>,
    identity: &Identity,
    target: &Target,
) -> Result<Conn, ErrorInfo> {
    Conn::open(shared, identity, target.addr, Some(&target.fingerprint))
        .await
        .map_err(Failure::into_error)
}

fn malformed(what: &str) -> ErrorInfo {
    ErrorInfo::new(
        ErrorCode::Network,
        format!("the receiver sent a malformed {what}"),
    )
}

async fn upload(
    shared: &Arc<Shared>,
    conn: &mut Conn,
    session: &str,
    id: &str,
    token: &str,
    content: Content,
    transfer: &TransferHandle,
) -> Result<(), ErrorInfo> {
    let idle = shared.opts.idle_timeout();
    let len = content.len();
    // Opened and checked before the request starts: a file that fails the
    // check must not become a request, least of all an empty one, whose
    // body would be complete the moment its head was sent.
    let source = open_source(idle, content).await?;
    let path = wire::path_with_query(
        &format!("{}/upload", wire::API_V2),
        &[("sessionId", session), ("fileId", id), ("token", token)],
    );
    let (tx, body) = client::stream_body(len);
    let pending = conn
        .start(Method::POST, &path, body, false)
        .await
        .map_err(Failure::into_error)?;
    tokio::pin!(pending);
    let feeding = feed(idle, source, tx, transfer);
    tokio::pin!(feeding);
    let mut fed = false;
    let response = loop {
        tokio::select! {
            result = &mut feeding, if !fed => {
                result?;
                fed = true;
            }
            response = &mut pending => {
                break response.map_err(|_| {
                    ErrorInfo::new(ErrorCode::Network, "the upload failed")
                })?;
            }
            () = tokio::time::sleep(idle), if fed => {
                return Err(ErrorInfo::new(ErrorCode::Network, "the receiver did not confirm the file"));
            }
        }
    };
    let code = response.status();
    client::drain(shared, response).await;
    if code == StatusCode::OK {
        Ok(())
    } else {
        Err(Failure::Status(code).into_error())
    }
}

/// Opens a file to send, once, read-only. `O_NONBLOCK` makes opening a
/// FIFO or a device return at once instead of waiting for a writer; for the
/// regular file it must be, the flag changes nothing. Symlinks are followed
/// on purpose, as the hub's check does: the picker hands out links.
#[allow(clippy::disallowed_methods)] // S3 bans opening for writing; this reads.
async fn open_to_send(path: &std::path::Path) -> std::io::Result<tokio::fs::File> {
    let flags = rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::NOCTTY;
    tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(i32::try_from(flags.bits()).unwrap_or(0))
        .open(path)
        .await
}

/// What an upload reads from.
enum Source {
    /// The file, opened and checked.
    File { file: tokio::fs::File, size: u64 },
    /// A text.
    Text(Bytes),
}

/// Opens a file to upload and checks the handle it will read from: a
/// regular file of the size the hub saw. Anything swapped in since -- a
/// FIFO, a device, a file that grew or shrank -- is refused, not sent.
async fn open_source(idle: Duration, content: Content) -> Result<Source, ErrorInfo> {
    let (path, size) = match content {
        Content::Text(bytes) => return Ok(Source::Text(bytes)),
        Content::File { path, size } => (path, size),
    };
    let unreadable = || ErrorInfo::new(ErrorCode::BadFile, "the file cannot be read");
    let file = tokio::time::timeout(idle, open_to_send(&path))
        .await
        .map_err(|_| unreadable())?
        .map_err(|_| unreadable())?;
    let meta = file.metadata().await.map_err(|_| unreadable())?;
    if !meta.is_file() || meta.len() != size {
        return Err(ErrorInfo::new(
            ErrorCode::BadFile,
            "the file changed after it was chosen",
        ));
    }
    Ok(Source::File { file, size })
}

/// Hands `source` to the connection chunk by chunk.
async fn feed(
    idle: Duration,
    source: Source,
    tx: mpsc::Sender<Bytes>,
    transfer: &TransferHandle,
) -> Result<(), ErrorInfo> {
    let stalled = || ErrorInfo::new(ErrorCode::Network, "the receiver stopped reading");
    let closed = || ErrorInfo::new(ErrorCode::Network, "the connection closed");
    match source {
        Source::Text(bytes) => {
            let n = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if n > 0 {
                tokio::time::timeout(idle, tx.send(bytes))
                    .await
                    .map_err(|_| stalled())?
                    .map_err(|_| closed())?;
            }
            transfer.add_progress(n);
            Ok(())
        }
        Source::File { mut file, size } => {
            let unreadable = || ErrorInfo::new(ErrorCode::BadFile, "the file cannot be read");
            // Never past the checked size, however much the file grows.
            let mut remaining = size;
            while remaining > 0 {
                let want = usize::try_from(remaining)
                    .unwrap_or(IO_CHUNK_BYTES)
                    .min(IO_CHUNK_BYTES);
                let mut chunk = BytesMut::zeroed(want);
                let n = tokio::time::timeout(idle, file.read(chunk.as_mut()))
                    .await
                    .map_err(|_| unreadable())?
                    .map_err(|_| unreadable())?;
                if n == 0 {
                    return Err(ErrorInfo::new(
                        ErrorCode::BadFile,
                        "the file got shorter while it was being sent",
                    ));
                }
                chunk.truncate(n);
                let n64 = u64::try_from(n).unwrap_or(u64::MAX);
                remaining = remaining.saturating_sub(n64);
                tokio::time::timeout(idle, tx.send(chunk.freeze()))
                    .await
                    .map_err(|_| stalled())?
                    .map_err(|_| closed())?;
                transfer.add_progress(n64);
            }
            Ok(())
        }
    }
}
