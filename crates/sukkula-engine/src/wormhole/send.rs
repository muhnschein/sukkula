//! F-MW1: send one file or one text, behind a code we allocate.

use std::sync::Arc;
use std::time::Duration;

use magic_wormhole::transfer::APP_CONFIG;
use magic_wormhole::transit::{Abilities, TransitRole};
use magic_wormhole::{MailboxConnection, Wormhole};
use sha2::{Digest, Sha256};
use sukkula_core::Protocol;
use sukkula_core::hex;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;

use super::session::{
    CatchUnwind, Panicked, Servers, Session, cancellable, lib, off_runtime, panicked, protocol,
};
use super::transit::{self, Role};
use super::wire::{self, Answer, PeerMsg};
use super::{CLEANUP_WAIT, CODE_WORDS, Inner, PEER_LABEL, RECORD_BYTES, code, mailbox};
use crate::adapter::{Outgoing, OutgoingFile};
use crate::api::{Direction, ErrorCode, ErrorInfo, Event, SendTarget, TransferId};
use crate::ctx::TransferHandle;

/// Checks the request, registers the transfer and starts it. Returns as
/// soon as the transfer exists; the code follows in `Event::WormholeCode`.
pub(super) fn start(
    inner: &Arc<Inner>,
    target: SendTarget,
    mut items: Vec<Outgoing>,
) -> Result<TransferId, ErrorInfo> {
    if target != SendTarget::Wormhole {
        return Err(ErrorInfo::new(
            ErrorCode::BadCommand,
            "not a wormhole target",
        ));
    }
    if items.len() > 1 {
        return Err(ErrorInfo::new(
            ErrorCode::TooLarge,
            "a wormhole carries one file or one text",
        ));
    }
    let item = items
        .pop()
        .ok_or_else(|| ErrorInfo::new(ErrorCode::BadCommand, "nothing to send"))?;
    let servers = Servers::from_settings(&inner.ctx.settings().wormhole)?;
    let view = item.view();
    let total = view.size;
    let handle = inner
        .ctx
        .transfers()
        .begin_views(
            &inner.ctx,
            Direction::Outgoing,
            Protocol::Wormhole,
            PEER_LABEL,
            vec![view],
            total,
        )
        .ok_or_else(|| ErrorInfo::new(ErrorCode::TooLarge, "too many transfers running"))?;
    let id = handle.id();
    let inner = inner.clone();
    tokio::spawn(async move {
        let result = CatchUnwind::new(run(&inner, &handle, item, servers)).await;
        handle.finish_with(result.unwrap_or_else(|Panicked| Err(panicked())));
    });
    Ok(id)
}

async fn run(
    inner: &Inner,
    handle: &TransferHandle,
    item: Outgoing,
    servers: Servers,
) -> Result<Vec<String>, ErrorInfo> {
    let t = inner.tuning;
    let token = handle.token();
    // The file is opened before anything else: a code is not worth
    // showing for a file that is gone.
    let source = match &item {
        Outgoing::File(file) => Some(open_checked(file, t.idle).await?),
        Outgoing::Text(_) => None,
    };
    let guard = mailbox::open(&servers.mailbox, &t, token).await?;
    let config = APP_CONFIG.rendezvous_url(guard.url.clone().into());
    // Off the runtime: this is where the library mints hashcash (W1).
    let mailbox = cancellable(
        token,
        off_runtime(
            t.handshake,
            "allocating a code",
            MailboxConnection::create(config, CODE_WORDS),
        ),
    )
    .await?
    .map_err(|e| {
        guard
            .verdict()
            .unwrap_or_else(|| super::session::wormhole_error(&e))
    })?;
    // The nameplate half of the code is the mailbox server's choice, and
    // the server is not trusted: the code goes to the screen only if it
    // passes the same check a code typed by the user does (digits, a hyphen,
    // short lowercase words), so a server cannot put control or bidi
    // characters, or a kilobyte of text, in front of the user.
    let shown = code::parse(&mailbox.code().to_string()).map_err(|_| {
        ErrorInfo::new(
            ErrorCode::Network,
            "the mailbox server allocated a malformed code",
        )
    })?;
    let custom = servers.custom_mailbox.then_some(&servers.mailbox);
    let qr = code::qr(&shown, custom)?;
    inner.ctx.emit(Event::WormholeCode {
        transfer: handle.id(),
        code: shown.to_string(),
        qr,
    });
    // The receiver has to read the code off this screen and type it in.
    let wormhole = cancellable(
        token,
        lib(
            t.peer_wait,
            "waiting for the receiver",
            Wormhole::connect(mailbox),
        ),
    )
    .await?
    .map_err(|e| {
        guard.verdict().unwrap_or_else(|| match e {
            magic_wormhole::WormholeError::PakeFailed => {
                ErrorInfo::new(ErrorCode::BadCode, "the receiver typed a different code")
            }
            other => super::session::wormhole_error(&other),
        })
    })?;
    let mut session = Session::new(wormhole, guard);
    let result = cancellable(token, async {
        match (&item, source) {
            (Outgoing::Text(text), _) => send_text(inner, &mut session, handle, text).await,
            (Outgoing::File(file), Some(source)) => {
                send_file(inner, &mut session, handle, file, source, &servers).await
            }
            (Outgoing::File(_), None) => Err(bad_file()),
        }
    })
    .await;
    let reason = match &result {
        Ok(()) => None,
        Err(_) if handle.is_cancelled() => Some("transfer cancelled"),
        Err(_) => Some("transfer failed"),
    };
    if !inner.ctx.shutdown_token().is_cancelled() {
        session.goodbye(reason).await;
    }
    result.map(|()| Vec::new())
}

fn declined() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Refused, "the receiver declined")
}

fn bad_file() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::BadFile, "the file cannot be read")
}

/// Opens the file to send, once, and checks the handle rather than the
/// path: a regular file of exactly the size the hub measured. The open is
/// read-only and non-blocking, so a FIFO put in the file's place after the
/// hub looked cannot hang it (and is then refused by the check). Nothing
/// after this looks at the path again.
#[allow(clippy::disallowed_methods)] // S3 bans opening for writing; this is O_RDONLY.
async fn open_checked(file: &OutgoingFile, limit: Duration) -> Result<tokio::fs::File, ErrorInfo> {
    use rustix::fs::{Mode, OFlags};
    let path = file.path.clone();
    let expected = file.size;
    let opening = tokio::task::spawn_blocking(move || {
        let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY;
        let fd = rustix::fs::open(&path, flags, Mode::empty()).map_err(|_| bad_file())?;
        let f = std::fs::File::from(fd);
        let meta = f.metadata().map_err(|_| bad_file())?;
        if !meta.is_file() {
            return Err(ErrorInfo::new(ErrorCode::BadFile, "not a regular file"));
        }
        if meta.len() != expected {
            return Err(ErrorInfo::new(
                ErrorCode::BadFile,
                "the file changed since it was chosen",
            ));
        }
        Ok(f)
    });
    let f = tokio::time::timeout(limit, opening)
        .await
        .map_err(|_| bad_file())?
        .map_err(|_| bad_file())??;
    Ok(tokio::fs::File::from_std(f))
}

async fn send_text(
    inner: &Inner,
    session: &mut Session,
    handle: &TransferHandle,
    text: &str,
) -> Result<(), ErrorInfo> {
    let t = inner.tuning;
    session.send(t.idle, wire::offer_text(text)).await?;
    let deadline = deadline(t.answer_wait);
    loop {
        match session.receive_until(deadline).await? {
            PeerMsg::Answer(Answer::Message(true)) => {
                handle.add_progress(u64::try_from(text.len()).unwrap_or(u64::MAX));
                return Ok(());
            }
            PeerMsg::Answer(_) | PeerMsg::Error => return Err(declined()),
            PeerMsg::Offer(_) => return Err(protocol("the receiver sent an offer")),
            // A receiver may send transit hints even for a text.
            PeerMsg::Transit(_) | PeerMsg::Other => {}
        }
    }
}

async fn send_file(
    inner: &Inner,
    session: &mut Session,
    handle: &TransferHandle,
    file: &OutgoingFile,
    mut source: tokio::fs::File,
    servers: &Servers,
) -> Result<(), ErrorInfo> {
    let t = inner.tuning;
    session
        .send(t.idle, wire::transit(servers.relay_hint()))
        .await?;
    session
        .send(t.idle, wire::offer_file(file.name.as_str(), file.size))
        .await?;

    // The receiver's hints come before its answer from every reference
    // client; a late one is still taken.
    let deadline = deadline(t.answer_wait);
    let mut their = None;
    loop {
        match session.receive_until(deadline).await? {
            PeerMsg::Transit(tr) => {
                their.get_or_insert(tr);
            }
            PeerMsg::Answer(Answer::File(true)) => break,
            PeerMsg::Answer(_) | PeerMsg::Error => return Err(declined()),
            PeerMsg::Offer(_) => return Err(protocol("the receiver sent an offer")),
            PeerMsg::Other => {}
        }
    }
    let their = match their {
        Some(tr) => tr,
        None => loop {
            match session.receive_until(self::deadline(t.idle)).await? {
                PeerMsg::Transit(tr) => break tr,
                PeerMsg::Error => return Err(declined()),
                _ => {}
            }
        },
    };

    let (transit_key, relay_token) = session.transit_key();
    let targets = transit::targets(&servers.relay, &their, inner.ctx.reach());
    let plan = transit::plan(
        targets,
        &relay_token,
        Role::Leader,
        &t,
        inner.ctx.reach(),
        handle.token(),
    )
    .await?;
    let connector = lib(
        t.handshake,
        "preparing transit",
        magic_wormhole::transit::init(Abilities::FORCE_RELAY, None, plan.ours.clone()),
    )
    .await?
    .map_err(|_| ErrorInfo::new(ErrorCode::Network, "transit could not start"))?;
    let (mut transit, _info) = lib(
        t.handshake,
        "connecting to the receiver",
        connector.connect(
            TransitRole::Leader,
            transit_key,
            Abilities::FORCE_RELAY,
            Arc::new(plan.theirs.clone()),
        ),
    )
    .await?
    .map_err(|_| ErrorInfo::new(ErrorCode::Network, "no transit connection to the receiver"))?;
    plan.connected();

    // Exactly the size we announced, even if the file has grown since; a
    // file that shrank fails.
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; RECORD_BYTES];
    let mut left = file.size;
    while left > 0 {
        let want = usize::try_from(left.min(u64::try_from(RECORD_BYTES).unwrap_or(u64::MAX)))
            .unwrap_or(RECORD_BYTES);
        let chunk = buf.get_mut(..want).ok_or_else(bad_file)?;
        let n = tokio::time::timeout(t.idle, source.read(chunk))
            .await
            .map_err(|_| bad_file())?
            .map_err(|_| bad_file())?;
        if n == 0 {
            return Err(ErrorInfo::new(
                ErrorCode::BadFile,
                "the file shrank while sending",
            ));
        }
        let data = chunk.get(..n).ok_or_else(bad_file)?;
        hasher.update(data);
        lib(t.idle, "sending to the receiver", transit.send_record(data))
            .await?
            .map_err(|_| ErrorInfo::new(ErrorCode::Network, "the transit connection failed"))?;
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        left = left.saturating_sub(n);
        handle.add_progress(n);
    }
    lib(t.idle, "sending to the receiver", transit.flush())
        .await?
        .map_err(|_| ErrorInfo::new(ErrorCode::Network, "the transit connection failed"))?;
    let digest = hex::encode(&hasher.finalize());
    let ack = lib(
        t.idle,
        "waiting for the receiver's ack",
        transit.receive_record(),
    )
    .await?
    .map_err(|_| ErrorInfo::new(ErrorCode::Network, "the receiver did not confirm"))?;
    if !wire::transit_ack_matches(&ack, &digest) {
        return Err(ErrorInfo::new(
            ErrorCode::Network,
            "the receiver got something other than what was sent",
        ));
    }
    drop(transit);
    plan.finish(CLEANUP_WAIT).await;
    Ok(())
}

fn deadline(after: Duration) -> Instant {
    Instant::now()
        .checked_add(after)
        .unwrap_or_else(Instant::now)
}
