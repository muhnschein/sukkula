//! Sending: files, or one text, to whoever types the code we make.
//!
//! The transfer is registered and its id returned at once. Every file is
//! opened once, read-only and non-blocking, and the handle checked (a
//! regular file of the size the hub measured) before anything goes to the
//! relay; nothing after that looks at the path again. They are hashed
//! while the relay is reached, and the code follows in `Event::CrocCode`.
//! The receiver then has [`super::Tuning::peer_wait`] to type it and
//! [`super::Tuning::answer_wait`] to accept.
//!
//! The code is made for the relay: with croc's public relays, the first
//! that answers, trying them in order, and a code that picks it
//! ([`Code::relay_index`]). The first is croc 10's public relay too, and
//! our codes read the same to croc 10 and croc 11 (`code.rs`), so either
//! can receive. Which one did, its first PAKE message says (`"v": 2` or
//! not), and the key is agreed its way: croc 11's, confirmed
//! (`pakekey.rs`), or croc 10's.
//!
//! What the receiver asks for is served exactly: a file of the list with
//! bytes in it, the chunks named (or all of them), each once, and nothing
//! else. Chunk `k` goes over data room `k mod n`, as croc 10's sender
//! does.

use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::Duration;

use sukkula_core::Protocol;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::code::{self, Code};
use super::conn::{Conn, PING};
use super::crypt::{self, Cipher};
use super::message::{
    self, CHUNK_BYTES, Kind, MAX_CONTROL_BYTES, MAX_DATA_FRAME, Message, Sending, SimpleMessage,
};
use super::pake::{Curve, MAX_PAKE_BYTES, Pake};
use super::pakekey::{self, Side};
use super::relay::{self, Joined, Relay, Relays};
use super::xxh64::Xxh64;
use super::{
    CLEANUP_WAIT, Inner, MAX_DATA_ROOMS, PEER_LABEL, cancellable, deadline, protocol, random,
};
use crate::adapter::Outgoing;
use crate::api::{Direction, ErrorCode, ErrorInfo, Event, SendTarget, TransferId};
use crate::by_code::{bad_file, open_checked};
use crate::ctx::TransferHandle;

/// What is being sent: files, or one text sent as croc sends one.
enum Payload {
    Files(Vec<Source>),
    Text(String),
}

/// A file to send, open.
struct Source {
    name: String,
    size: u64,
    file: Arc<std::fs::File>,
}

/// Checks the request, registers the transfer and starts it. Returns as
/// soon as the transfer exists; the code follows in `Event::CrocCode`.
pub(super) fn start(
    inner: &Arc<Inner>,
    target: SendTarget,
    items: Vec<Outgoing>,
) -> Result<TransferId, ErrorInfo> {
    if target != SendTarget::Croc {
        return Err(ErrorInfo::new(ErrorCode::BadCommand, "not a croc target"));
    }
    if items.is_empty() {
        return Err(ErrorInfo::new(ErrorCode::BadCommand, "nothing to send"));
    }
    let texts = items
        .iter()
        .filter(|i| matches!(i, Outgoing::Text(_)))
        .count();
    if texts > 0 && items.len() > 1 {
        return Err(ErrorInfo::new(
            ErrorCode::TooLarge,
            "croc carries files, or one text on its own",
        ));
    }
    let relays = Relays::from_settings(&inner.ctx.settings().croc)?;
    let views: Vec<_> = items.iter().map(Outgoing::view).collect();
    let total = views.iter().fold(0u64, |a, v| a.saturating_add(v.size));
    let handle = inner
        .ctx
        .transfers()
        .begin_views(
            &inner.ctx,
            Direction::Outgoing,
            Protocol::Croc,
            PEER_LABEL,
            views,
            total,
        )
        .ok_or_else(|| ErrorInfo::new(ErrorCode::TooLarge, "too many transfers running"))?;
    let id = handle.id();
    let inner = inner.clone();
    tokio::spawn(async move {
        let result = cancellable(handle.token(), run(&inner, &handle, items, &relays)).await;
        handle.finish_with(result.map(|()| Vec::new()));
    });
    Ok(id)
}

async fn run(
    inner: &Inner,
    handle: &TransferHandle,
    items: Vec<Outgoing>,
    relays: &Relays,
) -> Result<(), ErrorInfo> {
    let t = inner.tuning;
    // Opened before anything else: a code is not worth showing for a file
    // that is gone.
    let payload = open_all(items, t.idle).await?;
    // Hashed while the relay is reached and the receiver types the code.
    let hashing = match &payload {
        Payload::Files(sources) => {
            let files: Vec<_> = sources.iter().map(|s| (s.file.clone(), s.size)).collect();
            tokio::task::spawn_blocking(move || hash_all(&files))
        }
        Payload::Text(text) => {
            let mut h = Xxh64::default();
            h.update(text.as_bytes());
            let hash = h.finish();
            tokio::task::spawn_blocking(move || Ok(vec![hash]))
        }
    };
    let (code, relay, mut control) = meet(relays, t.handshake).await?;
    let room = code.room();
    inner.ctx.emit(Event::CrocCode {
        transfer: handle.id(),
        code: code.to_string(),
    });
    wait_for_receiver(&mut control.conn, &code, t.peer_wait, t.idle).await?;
    let cipher = key_exchange(&mut control.conn, &code, t.handshake).await?;
    let ports: Vec<u16> = control.ports.iter().take(MAX_DATA_ROOMS).copied().collect();
    if ports.is_empty() {
        return Err(protocol("the croc relay offers no data rooms"));
    }
    let mut data = join_rooms(&relay, &ports, &room, t.handshake).await?;
    // The receiver says where it is; we say nothing.
    let limit = t.handshake;
    loop {
        let m = next_message(&mut control.conn, &cipher, deadline(limit)).await?;
        match m.kind {
            Kind::ExternalIp => break,
            Kind::Error => return Err(declined()),
            Kind::Other => {}
            _ => return Err(protocol("the croc receiver spoke out of turn")),
        }
    }
    send_message(
        &mut control.conn,
        &cipher,
        &Message::new(Kind::ExternalIp),
        t.idle,
    )
    .await?;

    let hashes = hashing
        .await
        .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "hashing failed"))??;
    let (sources, text) = match payload {
        Payload::Files(s) => (s, None),
        Payload::Text(text) => (Vec::new(), Some(text)),
    };
    let listed: Vec<Sending<'_>> = match &text {
        Some(text) => vec![Sending {
            // croc's own name for a text, which its receivers look for.
            name: TEXT_NAME,
            size: u64::try_from(text.len()).unwrap_or(u64::MAX),
            hash: hashes.first().copied().unwrap_or_default(),
        }],
        None => sources
            .iter()
            .zip(&hashes)
            .map(|(s, h)| Sending {
                name: &s.name,
                size: s.size,
                hash: *h,
            })
            .collect(),
    };
    let sizes: Vec<u64> = listed.iter().map(|l| l.size).collect();
    let mut fileinfo = Message::new(Kind::FileInfo);
    fileinfo.b = message::write_offer(&listed, text.is_some());
    send_message(&mut control.conn, &cipher, &fileinfo, t.idle).await?;

    // Until the receiver has asked for something, it may be a person
    // deciding; after, only the network.
    let mut wait = t.answer_wait;
    loop {
        let m = next_message(&mut control.conn, &cipher, deadline(wait)).await?;
        wait = t.idle;
        match m.kind {
            Kind::RecipientReady => {
                let request = message::read_request(&m.b, &sizes).ok_or_else(|| {
                    protocol("the croc receiver asked for something we do not have")
                })?;
                let source = match &text {
                    Some(text) => Chunks::Text(Arc::from(text.as_bytes())),
                    None => {
                        let s = sources
                            .get(request.index)
                            .ok_or_else(|| protocol("no such file"))?;
                        Chunks::File(s.file.clone(), s.size)
                    }
                };
                let (back, mut closed) = serve(
                    handle,
                    &mut control.conn,
                    &cipher,
                    data,
                    source,
                    request.chunks,
                    t.idle,
                )
                .await?;
                data = back;
                // The receiver closes the file -- perhaps already, while
                // the last chunks' tasks were being collected; we answer.
                while !closed {
                    let m = next_message(&mut control.conn, &cipher, deadline(t.idle)).await?;
                    match m.kind {
                        Kind::CloseSender => closed = true,
                        Kind::Error => return Err(declined()),
                        Kind::Other => {}
                        _ => return Err(protocol("the croc receiver spoke out of turn")),
                    }
                }
                send_message(
                    &mut control.conn,
                    &cipher,
                    &Message::new(Kind::CloseRecipient),
                    t.idle,
                )
                .await?;
            }
            Kind::Finished => {
                let _ = send_message(
                    &mut control.conn,
                    &cipher,
                    &Message::new(Kind::Finished),
                    CLEANUP_WAIT,
                )
                .await;
                return Ok(());
            }
            Kind::Error => return Err(declined()),
            Kind::Other | Kind::ExternalIp => {}
            _ => return Err(protocol("the croc receiver spoke out of turn")),
        }
    }
}

/// A code, and the room for it on a relay: the first of croc's public
/// relays that answers, with a code that picks it, or the relay the
/// settings name.
async fn meet(relays: &Relays, limit: Duration) -> Result<(Code, Relay, Joined), ErrorInfo> {
    let mut failed = None;
    for (relay, on) in relays.for_sending() {
        let code = code::generate(on)?;
        match relay::join(&relay, relay.port, &code.room(), MAX_CONTROL_BYTES, limit).await {
            Ok(joined) => return Ok((code, relay, joined)),
            // Unreachable, or not a croc relay: the next one.
            Err(e) if e.code == ErrorCode::Network => failed = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(failed.unwrap_or_else(|| protocol("no croc relay")))
}

/// croc's name for a text sent as a file (`croc-stdin-…`).
const TEXT_NAME: &str = "croc-stdin-sukkula";

fn declined() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Refused, "the receiver declined")
}

async fn open_all(items: Vec<Outgoing>, limit: Duration) -> Result<Payload, ErrorInfo> {
    let mut sources = Vec::new();
    let mut names = std::collections::HashSet::new();
    for item in items {
        match item {
            Outgoing::Text(text) => return Ok(Payload::Text(text)),
            Outgoing::File(f) => {
                let file = open_checked(&f, limit).await?;
                // Go's receivers refuse two files of one name, and names
                // croc 11 would not write.
                let name = unique(&mut names, &croc_name(f.name.as_str()));
                sources.push(Source {
                    name,
                    size: f.size,
                    file: Arc::new(file),
                });
            }
        }
    }
    Ok(Payload::Files(sources))
}

/// The name as croc's receivers take it (croc 11's `receivefs`): every
/// kind of space an ASCII one, and whatever croc 11 refuses in a name --
/// a character that does not print, `:`, a slash of either kind, a
/// trailing dot or space -- a `_`; a name croc 11 will not write (a
/// Windows device's, `.git`, `.ssh`, `.gnupg`) gets a `_` in front.
fn croc_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            c if c.is_whitespace() => ' ',
            ':' | '/' | '\\' => '_',
            c if !prints(c) => '_',
            c => c,
        })
        .collect();
    if out.is_empty() || out.ends_with(['.', ' ']) {
        out.pop();
        out.push('_');
    }
    let stem = out.split('.').next().unwrap_or_default().to_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    let hidden = matches!(out.to_lowercase().as_str(), ".git" | ".ssh" | ".gnupg");
    if reserved || hidden {
        out.insert(0, '_');
    }
    out
}

/// Whether Go counts `c` as printable in a name: not a control or format
/// character, nor a private or non-character code point. (Code points
/// Unicode has not assigned yet are not known here.)
fn prints(c: char) -> bool {
    !c.is_control()
        && !matches!(
            u32::from(c),
            0xAD | 0x600..=0x605
                | 0x61C
                | 0x6DD
                | 0x70F
                | 0x890..=0x891
                | 0x8E2
                | 0x180E
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x206F
                | 0xFDD0..=0xFDEF
                | 0xFEFF
                | 0xFFF9..=0xFFFB
                | 0x110BD
                | 0x110CD
                | 0x13430..=0x1343F
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0001
                | 0xE0020..=0xE007F
                | 0xE000..=0xF8FF
                | 0xF0000..=0x10FFFF
        )
        && (u32::from(c) & 0xFFFE) != 0xFFFE
}

/// `name`, or `name (2)`, `name (3)`, … if it is taken, letter case aside:
/// croc 11 takes two names that differ only in case for one.
fn unique(taken: &mut std::collections::HashSet<String>, name: &str) -> String {
    if taken.insert(name.to_lowercase()) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let mut n = 2u32;
    loop {
        let candidate = format!("{stem} ({n}){ext}");
        if taken.insert(candidate.to_lowercase()) {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}

/// Every file's XXH64, read through the handle.
fn hash_all(files: &[(Arc<std::fs::File>, u64)]) -> Result<Vec<[u8; 8]>, ErrorInfo> {
    let mut out = Vec::with_capacity(files.len());
    let mut buf = vec![0u8; 256 * 1024];
    for (file, size) in files {
        let mut h = Xxh64::default();
        let mut at = 0u64;
        while at < *size {
            let want = usize::try_from(size.saturating_sub(at))
                .unwrap_or(usize::MAX)
                .min(buf.len());
            let chunk = buf.get_mut(..want).ok_or_else(bad_file)?;
            file.read_exact_at(chunk, at)
                .map_err(|_| ErrorInfo::new(ErrorCode::BadFile, "the file shrank while sending"))?;
            h.update(chunk);
            at = at.saturating_add(u64::try_from(want).unwrap_or(u64::MAX));
        }
        out.push(h.finish());
    }
    Ok(out)
}

/// Waits for the receiver's `handshake`, answering croc 10's LAN probe
/// on the way (a croc 10 receiver waits for its answer before anything
/// else). The probe is answered on the curve it came on, and its `ips?`
/// with no addresses: we run no relay of our own. croc 11's probe is not
/// answered at all.
async fn wait_for_receiver(
    control: &mut Conn,
    code: &Code,
    wait: Duration,
    idle: Duration,
) -> Result<(), ErrorInfo> {
    let until = deadline(wait);
    let mut probe: Option<Cipher> = None;
    let mut probes = 0u8;
    loop {
        let frame = control.recv_skipping(until).await.map_err(|e| {
            if tokio::time::Instant::now() >= until {
                ErrorInfo::new(ErrorCode::Network, "nobody typed the code in time")
            } else {
                e
            }
        })?;
        if frame == b"handshake" {
            return Ok(());
        }
        if let Some(p) = &probe {
            match p.open(&frame).as_deref() {
                Some(b"ips?") => {
                    let none = p
                        .seal(b"[]", random()?)
                        .ok_or_else(|| protocol("sealing failed"))?;
                    control.send(&none, idle).await?;
                    continue;
                }
                Some(_) => return Err(protocol("the croc receiver spoke out of turn")),
                None => {
                    return Err(ErrorInfo::new(
                        ErrorCode::BadCode,
                        "the receiver typed a different code",
                    ));
                }
            }
        }
        probes = probes.saturating_add(1);
        let pake1 = SimpleMessage::decode(&frame)
            .filter(|p| p.kind == "pake1" && p.bytes.len() <= MAX_PAKE_BYTES && probes <= 1);
        let Some(pake1) = pake1 else {
            return Err(protocol("the croc receiver spoke out of turn"));
        };
        if pake1.version != 0 {
            // croc 11's: it waits half a second for an answer, then goes on
            // to the handshake, and takes one that comes later for a broken
            // transfer. So none is given.
            continue;
        }
        let bytes = pake1.bytes;
        let answered = Curve::of_message(&bytes)
            .and_then(|curve| Pake::answer(curve, code.password(), random().ok()?, &bytes).ok());
        match answered.and_then(|p| Some((p.message(), p.key()?))) {
            Some((reply, key)) => {
                control
                    .send(&SimpleMessage::encode("pake2", &reply), idle)
                    .await?;
                // The probe's key is the PAKE's, used as it is.
                probe = Some(Cipher::new(&key));
            }
            // A curve we do not speak: any other kind makes a Go receiver
            // give the probe up and go on to the handshake.
            None => {
                control
                    .send(&SimpleMessage::encode("none", b""), idle)
                    .await?;
            }
        }
    }
}

/// The peers' PAKE, the receiver speaking first, croc 11's way or croc
/// 10's, as the receiver's message says; returns the session cipher.
async fn key_exchange(
    control: &mut Conn,
    code: &Code,
    limit: Duration,
) -> Result<Cipher, ErrorInfo> {
    let frame = control.recv_skipping(deadline(limit)).await?;
    let m = Message::decode(&frame, None)
        .filter(|m| m.kind == Kind::Pake && m.b.len() <= MAX_PAKE_BYTES)
        .ok_or_else(|| protocol("the croc receiver did not start the key exchange"))?;
    if m.v != 0 && m.v != pakekey::VERSION {
        return Err(ErrorInfo::new(
            ErrorCode::Unavailable,
            "the croc receiver speaks a newer croc than Sukkula",
        ));
    }
    let curve = Curve::from_name(&m.b2).ok_or_else(|| {
        ErrorInfo::new(
            ErrorCode::Unavailable,
            "the croc receiver asked for a curve Sukkula does not speak",
        )
    })?;
    let pake = Pake::answer(curve, code.password(), random()?, &m.b)
        .map_err(|_| protocol("the croc receiver's key exchange is malformed"))?;
    let mut reply = Message::new(Kind::Pake);
    reply.b = pake.message();
    if m.v == 0 {
        // croc 10: the key through PBKDF2 with our salt, unconfirmed.
        let key = pake.key().ok_or_else(|| protocol("no key"))?;
        let salt: [u8; crypt::SALT_BYTES] = random()?;
        reply.b2 = salt.to_vec();
        control.send(&reply.encode(None)?, limit).await?;
        return Ok(Cipher::new(&crypt::derive(&key, &salt)));
    }
    let salt: [u8; pakekey::SALT_BYTES] = random()?;
    reply.v = pakekey::VERSION;
    reply.b2 = salt.to_vec();
    let keys = pakekey::for_transfer(&pake, &code.room(), &m.b, &reply.b, &salt)
        .ok_or_else(|| protocol("no key"))?;
    control.send(&reply.encode(None)?, limit).await?;
    let frame = control.recv_skipping(deadline(limit)).await?;
    let theirs = Message::decode(&frame, None)
        .filter(|c| c.kind == Kind::PakeConfirm && c.v == pakekey::VERSION)
        .ok_or_else(|| protocol("the croc receiver did not confirm the key"))?;
    if !keys.verify(Side::Receiver, &theirs.b) {
        return Err(ErrorInfo::new(
            ErrorCode::BadCode,
            "the receiver typed a different code",
        ));
    }
    let mut ours = Message::new(Kind::PakeConfirm);
    ours.v = pakekey::VERSION;
    ours.b = keys.tag(Side::Sender).to_vec();
    control.send(&ours.encode(None)?, limit).await?;
    Ok(Cipher::new(keys.encryption()))
}

/// Joins the data room on each port at once.
pub(super) async fn join_rooms(
    relay: &Relay,
    ports: &[u16],
    room: &str,
    limit: Duration,
) -> Result<Vec<Conn>, ErrorInfo> {
    let mut set = JoinSet::new();
    for (i, port) in ports.iter().enumerate() {
        let relay = relay.clone();
        let port = *port;
        let room = format!("{room}-{i}");
        set.spawn(async move {
            relay::join(&relay, port, &room, MAX_DATA_FRAME, limit)
                .await
                .map(|j| (i, j.conn))
        });
    }
    let mut out: Vec<Option<Conn>> = ports.iter().map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        let (i, conn) = joined.map_err(|_| protocol("a data room failed"))??;
        if let Some(slot) = out.get_mut(i) {
            *slot = Some(conn);
        }
    }
    out.into_iter()
        .map(|c| c.ok_or_else(|| protocol("a data room failed")))
        .collect()
}

/// The next sealed control message that is not a keepalive, by `until`.
pub(super) async fn next_message(
    control: &mut Conn,
    cipher: &Cipher,
    until: tokio::time::Instant,
) -> Result<Message, ErrorInfo> {
    let frame = control.recv_skipping(until).await?;
    Message::decode(&frame, Some(cipher)).ok_or_else(|| {
        ErrorInfo::new(
            ErrorCode::BadCode,
            "a croc message did not decrypt: a different code, or a meddling relay",
        )
    })
}

pub(super) async fn send_message(
    control: &mut Conn,
    cipher: &Cipher,
    m: &Message,
    limit: Duration,
) -> Result<(), ErrorInfo> {
    control.send(&m.encode(Some(cipher))?, limit).await
}

/// Where chunks come from.
#[derive(Clone)]
enum Chunks {
    File(Arc<std::fs::File>, u64),
    Text(Arc<[u8]>),
}

impl Chunks {
    fn size(&self) -> u64 {
        match self {
            Chunks::File(_, size) => *size,
            Chunks::Text(t) => u64::try_from(t.len()).unwrap_or(u64::MAX),
        }
    }

    /// Chunk `k`: its position, then its bytes, as croc frames them.
    fn read(&self, k: u64) -> Result<Vec<u8>, ErrorInfo> {
        let chunk = u64::try_from(CHUNK_BYTES).unwrap_or(u64::MAX);
        let pos = k.checked_mul(chunk).ok_or_else(bad_file)?;
        let len =
            usize::try_from(self.size().saturating_sub(pos).min(chunk)).map_err(|_| bad_file())?;
        let mut out = Vec::with_capacity(len.saturating_add(8));
        out.extend_from_slice(&pos.to_le_bytes());
        out.resize(len.saturating_add(8), 0);
        let body = out.get_mut(8..).ok_or_else(bad_file)?;
        match self {
            Chunks::File(file, _) => file
                .read_exact_at(body, pos)
                .map_err(|_| ErrorInfo::new(ErrorCode::BadFile, "the file shrank while sending"))?,
            Chunks::Text(t) => {
                let start = usize::try_from(pos).map_err(|_| bad_file())?;
                let src = t
                    .get(start..start.saturating_add(len))
                    .ok_or_else(bad_file)?;
                body.copy_from_slice(src);
            }
        }
        Ok(out)
    }
}

/// Sends `chunks` of `source` over the data rooms, chunk `k` over room
/// `k mod n`, each room in a task of its own, and counts the bytes. A
/// receiver that gives up on the control connection meanwhile ends it.
/// Returns the data rooms, and whether the receiver has closed the file
/// already: it can, once the last chunk is on the wire, before every task
/// that sent one has been collected here.
async fn serve(
    handle: &TransferHandle,
    control: &mut Conn,
    cipher: &Cipher,
    data: Vec<Conn>,
    source: Chunks,
    chunks: Vec<u64>,
    idle: Duration,
) -> Result<(Vec<Conn>, bool), ErrorInfo> {
    let n = u64::try_from(data.len()).unwrap_or(1).max(1);
    let (progress, mut sent) = mpsc::channel::<u64>(64);
    let mut set = JoinSet::new();
    for (i, mut conn) in data.into_iter().enumerate() {
        let mine: Vec<u64> = chunks
            .iter()
            .copied()
            .filter(|k| k.checked_rem(n) == u64::try_from(i).ok())
            .collect();
        let source = source.clone();
        let cipher = cipher.clone();
        let progress = progress.clone();
        set.spawn(async move {
            for k in mine {
                let source = source.clone();
                let plain = tokio::task::spawn_blocking(move || source.read(k))
                    .await
                    .map_err(|_| bad_file())??;
                let len = u64::try_from(plain.len().saturating_sub(8)).unwrap_or(0);
                let sealed = cipher
                    .seal(&plain, random()?)
                    .ok_or_else(|| protocol("sealing failed"))?;
                conn.send(&sealed, idle).await?;
                let _ = progress.send(len).await;
            }
            Ok::<_, ErrorInfo>((i, conn))
        });
    }
    drop(progress);
    let mut back: Vec<Option<Conn>> = (0..n).map(|_| None).collect();
    let mut closed = false;
    loop {
        tokio::select! {
            done = set.join_next() => match done {
                None => break,
                Some(r) => {
                    let (i, conn) = r.map_err(|_| protocol("sending failed"))??;
                    if let Some(slot) = back.get_mut(i) {
                        *slot = Some(conn);
                    }
                }
            },
            Some(n) = sent.recv() => handle.add_progress(n),
            frame = control.recv_within(idle), if !closed => {
                // An error, a keepalive, or -- once it has the last chunk
                // -- the receiver closing the file; nothing else.
                match frame? {
                    None => {}
                    Some(f) if f == PING => {}
                    Some(f) => match Message::decode(&f, Some(cipher)).map(|m| m.kind) {
                        Some(Kind::CloseSender) => closed = true,
                        Some(Kind::Other) => {}
                        Some(Kind::Error) => return Err(declined()),
                        _ => return Err(protocol("the croc receiver spoke out of turn")),
                    },
                }
            }
        }
    }
    while let Ok(n) = sent.try_recv() {
        handle.add_progress(n);
    }
    let rooms = back
        .into_iter()
        .map(|c| c.ok_or_else(|| protocol("a data room failed")))
        .collect::<Result<_, _>>()?;
    Ok((rooms, closed))
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;

    #[test]
    fn names_are_made_unique() {
        let mut taken = std::collections::HashSet::new();
        assert_eq!(unique(&mut taken, "a.txt"), "a.txt");
        assert_eq!(unique(&mut taken, "a.txt"), "a (2).txt");
        assert_eq!(unique(&mut taken, "a.txt"), "a (3).txt");
        assert_eq!(unique(&mut taken, ".profile"), ".profile");
        assert_eq!(unique(&mut taken, ".profile"), ".profile (2)");
        assert_eq!(unique(&mut taken, "x"), "x");
        assert_eq!(unique(&mut taken, "x"), "x (2)");
        assert_eq!(unique(&mut taken, "A.TXT"), "A (4).TXT", "case aside");
    }

    #[test]
    fn names_are_ones_croc_11_writes() {
        for (name, sent) in [
            ("plain.txt", "plain.txt"),
            ("a\u{a0}b\tc.txt", "a b c.txt"),
            ("a:b\\c.txt", "a_b_c.txt"),
            ("zero\u{200b}width\u{feff}.txt", "zero_width_.txt"),
            ("private\u{e000}.txt", "private_.txt"),
            ("trailing.", "trailing_"),
            ("space ", "space_"),
            ("con", "_con"),
            ("Con.txt", "_Con.txt"),
            ("com1.log", "_com1.log"),
            ("LPT\u{b9}", "_LPT\u{b9}"),
            ("com10.log", "com10.log"),
            ("console.txt", "console.txt"),
            (".git", "_.git"),
            (".SSH", "_.SSH"),
            (".gnupg.txt", ".gnupg.txt"),
            ("émoji 😀.png", "émoji 😀.png"),
            ("", "_"),
        ] {
            assert_eq!(croc_name(name), sent, "{name:?}");
        }
    }

    #[test]
    fn chunks_are_croc_s() {
        let text: Vec<u8> = (0..70_000u32)
            .map(|i| u8::try_from(i % 256).unwrap())
            .collect();
        let c = Chunks::Text(Arc::from(text.as_slice()));
        let first = c.read(0).unwrap();
        assert_eq!(&first[..8], &0u64.to_le_bytes());
        assert_eq!(first.len(), 8 + CHUNK_BYTES);
        let last = c.read(2).unwrap();
        assert_eq!(&last[..8], &65_536u64.to_le_bytes());
        assert_eq!(&last[8..], &text[65_536..]);
    }
}
