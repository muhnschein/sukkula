//! Receiving with a code the user typed.
//!
//! The order is what S5 needs: the code is checked before the network is
//! touched; the file list is read, checked and shown; until the user says
//! yes nothing goes to the sender but the handshake, the PAKE and croc's
//! empty `externalip`, and nothing is asked for. Only after the yes does a
//! file get requested and written, through the inbox.
//!
//! As for Magic Wormhole, the transfer is registered when the offer is
//! accepted, and that is when `receive_code` returns its id; a wrong code,
//! an unreachable relay or a declined offer ends the command with an
//! error instead. Every wait before that is bounded by
//! [`super::Tuning::handshake`] and then by the consent timeout.
//!
//! # Chunks from several rooms
//!
//! The sender spreads a file's chunks over the data rooms, each room in
//! order, the rooms in no order among themselves, and the inbox writes a
//! file front to back. So the chunks are merged: each room holds at most
//! one chunk read ahead, and the next chunk the file needs is always the
//! first one some room has, since every room delivers in order. Nothing
//! more is buffered, and a room that runs ahead is held back by TCP. A
//! chunk that is not the next one any room can give -- sent twice,
//! replayed, from another file, past the end -- fails the transfer.

use std::sync::Arc;
use std::task::Poll;

use sukkula_core::Protocol;
use sukkula_core::offer::{Offer, RawFile, RawOffer};

use super::code::{self, Code};
use super::conn::{Conn, PING};
use super::crypt::{self, Cipher};
use super::message::{
    self, CHUNK_BYTES, Kind, MAX_CHUNK_PLAIN, MAX_CONTROL_BYTES, Message, OfferError,
};
use super::pake::{Curve, MAX_PAKE_BYTES, Pake};
use super::relay::{self, Relay};
use super::send::{join_rooms, next_message, send_message};
use super::xxh64::Xxh64;
use super::{CLEANUP_WAIT, Inner, PEER_LABEL, cancellable, deadline, protocol, random};
use crate::api::{ErrorCode, ErrorInfo, Event, TransferId};
use crate::by_code::declined_error;
use crate::ctx::{Accepted, ReceivingFile, TransferHandle};

/// What Go's receivers say for every refusal.
const REFUSING: &str = "refusing files";

/// How long to listen, once in the room, for the keepalive the relay sends
/// a client that is there first. croc's relay sends one a second.
const FIRST_PING_WAIT: std::time::Duration = std::time::Duration::from_millis(1200);

/// How long the keepalives must stop for before the sender counts as
/// there.
const PINGS_STOPPED: std::time::Duration = std::time::Duration::from_secs(2);

/// Everything open after the offer is read.
struct Session {
    control: Conn,
    data: Vec<Conn>,
    cipher: Cipher,
    offer: message::Offer,
}

/// Receives with `code`: returns once the user has accepted the offer,
/// with the transfer the rest runs as.
pub(super) async fn start(inner: Arc<Inner>, raw_code: String) -> Result<TransferId, ErrorInfo> {
    let code = code::parse(&raw_code)?;
    let relay = Relay::from_settings(&inner.ctx.settings().croc)?;
    let shutdown = inner.ctx.shutdown_token().clone();
    let (accepted, session) = {
        let _slot = inner.connecting.slot().ok_or_else(|| {
            ErrorInfo::new(ErrorCode::TooLarge, "too many receives are connecting")
        })?;
        cancellable(&shutdown, until_accepted(&inner, &code, &relay)).await?
    };
    let id = accepted.transfer.id();
    tokio::spawn(async move {
        let Accepted {
            offer, transfer, ..
        } = accepted;
        let mut session = session;
        let result = cancellable(
            transfer.token(),
            after_accept(&inner, &transfer, &offer, &mut session),
        )
        .await;
        if result.is_err() && !inner.ctx.shutdown_token().is_cancelled() {
            let mut m = Message::new(Kind::Error);
            m.m = REFUSING.to_owned();
            let _ = send_message(&mut session.control, &session.cipher, &m, CLEANUP_WAIT).await;
        }
        transfer.finish_with(result);
    });
    Ok(id)
}

/// Joins the room, agrees a key, reads the offer and asks the user.
async fn until_accepted(
    inner: &Inner,
    code: &Code,
    relay: &Relay,
) -> Result<(Accepted, Session), ErrorInfo> {
    let t = inner.tuning;
    let room = code.room();
    let joined = relay::join(relay, relay.port, &room, MAX_CONTROL_BYTES, t.handshake).await?;
    let mut control = joined.conn;
    wait_for_sender(&mut control, t.handshake).await?;
    control.send(b"handshake", t.handshake).await?;

    let mut pake =
        Pake::start(Curve::P256, code.password(), random()?).ok_or_else(|| protocol("no key"))?;
    let mut hello = Message::new(Kind::Pake);
    hello.b = pake.message();
    hello.b2 = Curve::P256.name().as_bytes().to_vec();
    control.send(&hello.encode(None)?, t.handshake).await?;
    let frame = control.recv_skipping(deadline(t.handshake)).await?;
    let reply = Message::decode(&frame, None)
        .filter(|m| m.kind == Kind::Pake && m.b.len() <= MAX_PAKE_BYTES)
        .ok_or_else(|| protocol("the croc sender did not answer the key exchange"))?;
    pake.finish(&reply.b)
        .map_err(|_| protocol("the croc sender's key exchange is malformed"))?;
    let salt: [u8; crypt::SALT_BYTES] = reply
        .b2
        .as_slice()
        .try_into()
        .map_err(|_| protocol("the croc sender's salt is malformed"))?;
    let key = pake.key().ok_or_else(|| protocol("no key"))?;
    let cipher = Cipher::new(&crypt::derive(&key, &salt));

    if joined.ports.is_empty() {
        return Err(protocol("the croc relay offers no data rooms"));
    }
    let data = join_rooms(relay, &joined.ports, &room, t.handshake).await?;
    send_message(
        &mut control,
        &cipher,
        &Message::new(Kind::ExternalIp),
        t.handshake,
    )
    .await?;

    // The sender's address, then its list. A message that does not
    // decrypt is a code that does not match.
    let until = deadline(t.handshake);
    let listed = loop {
        // A sender whose key is another's cannot read our `externalip`
        // and hangs up: before anything has decrypted, an ended
        // connection is most likely a wrong code.
        let m = next_message(&mut control, &cipher, until)
            .await
            .map_err(|e| {
                if e.code == ErrorCode::Network {
                    ErrorInfo::new(
                        ErrorCode::BadCode,
                        "the sender left before its list: a different code?",
                    )
                } else {
                    e
                }
            })?;
        match m.kind {
            Kind::ExternalIp | Kind::Other => {}
            Kind::FileInfo => break m.b,
            Kind::Error => return Err(ErrorInfo::new(ErrorCode::Refused, "the sender cancelled")),
            _ => return Err(protocol("the croc sender spoke out of turn")),
        }
    };
    let offer = match message::read_offer(&listed) {
        Ok(o) => o,
        Err(e) => {
            let mut m = Message::new(Kind::Error);
            m.m = REFUSING.to_owned();
            let _ = send_message(&mut control, &cipher, &m, CLEANUP_WAIT).await;
            return Err(match e {
                OfferError::Hash => ErrorInfo::new(
                    ErrorCode::Unavailable,
                    "the sender hashes with something other than xxhash",
                ),
                OfferError::Text => ErrorInfo::new(ErrorCode::TooLarge, "the text is too long"),
                OfferError::Malformed | OfferError::Empty => {
                    ErrorInfo::new(ErrorCode::Refused, "the offer was malformed")
                }
            });
        }
    };
    let raw = raw_offer(&offer);

    // Ask, while still listening: a sender that gives up takes its offer
    // off the screen. The consent timeout bounds this wait.
    // CONTRACT: the user typed the code, so the offer is one they asked
    // for, outside the LAN's global offer limit.
    let answer = {
        let mut asking = Box::pin(inner.ctx.offer_requested(raw));
        loop {
            tokio::select! {
                r = &mut asking => break r,
                f = control.recv_within(t.idle) => match f {
                    Ok(None) => {}
                    Ok(Some(f)) if f == PING => {}
                    Ok(Some(f)) => match Message::decode(&f, Some(&cipher)).map(|m| m.kind) {
                        Some(Kind::Other | Kind::ExternalIp) => {}
                        _ => {
                            return Err(ErrorInfo::new(ErrorCode::Refused, "the sender cancelled"));
                        }
                    },
                    Err(_) => return Err(ErrorInfo::new(ErrorCode::Refused, "the sender cancelled")),
                },
            }
        }
    };
    match answer {
        Ok(accepted) => {
            if accepted.offer.files.len() != offer.files.len() {
                return Err(protocol("the offer changed on the way"));
            }
            Ok((
                accepted,
                Session {
                    control,
                    data,
                    cipher,
                    offer,
                },
            ))
        }
        Err(declined) => {
            let mut m = Message::new(Kind::Error);
            m.m = REFUSING.to_owned();
            let _ = send_message(&mut control, &cipher, &m, CLEANUP_WAIT).await;
            Err(declined_error(&declined))
        }
    }
}

/// croc's file list as an offer: its files by name and size, from "croc".
/// A text is offered as the one small file it is on the wire.
pub(super) fn raw_offer(offer: &message::Offer) -> RawOffer {
    let mut raw = RawOffer::new(Protocol::Croc, PEER_LABEL);
    for f in &offer.files {
        raw.files.push(RawFile {
            name: if offer.text {
                "text.txt".to_owned()
            } else {
                f.name.clone()
            },
            size: i128::from(f.size),
            ..RawFile::default()
        });
    }
    raw
}

/// Once in the room: if the relay's keepalives come, the sender is not
/// there yet -- wait until they stop, which is when it came. croc's relay
/// passes on what the first client sends as it lets the second in, so
/// saying anything before then could reach the sender before the relay's
/// own `ok` does.
async fn wait_for_sender(control: &mut Conn, limit: std::time::Duration) -> Result<(), ErrorInfo> {
    let until = deadline(limit);
    let mut quiet = FIRST_PING_WAIT;
    loop {
        let left = until.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(ErrorInfo::new(
                ErrorCode::BadCode,
                "nobody is sending with this code",
            ));
        }
        match control.recv_within(quiet.min(left)).await? {
            None if quiet.min(left) == quiet => return Ok(()),
            None => {}
            Some(f) if f == PING => quiet = PINGS_STOPPED,
            Some(_) => return Err(protocol("the croc sender spoke first")),
        }
    }
}

/// Where a file's bytes go.
enum Sink<'a> {
    File(Box<ReceivingFile<'a>>),
    Text(Vec<u8>, &'a TransferHandle),
}

impl Sink<'_> {
    async fn put(&mut self, data: &[u8]) -> Result<(), ErrorInfo> {
        match self {
            Sink::File(f) => f.write(data).await,
            Sink::Text(buf, handle) => {
                buf.extend_from_slice(data);
                handle.add_progress(u64::try_from(data.len()).unwrap_or(u64::MAX));
                Ok(())
            }
        }
    }
}

/// Everything after the yes: each file asked for, merged, checked and
/// placed; then `finished`.
async fn after_accept(
    inner: &Inner,
    transfer: &TransferHandle,
    offer: &Offer,
    session: &mut Session,
) -> Result<Vec<String>, ErrorInfo> {
    let t = inner.tuning;
    let mut saved = Vec::new();
    let files = session.offer.files.clone();
    for (theirs, ours) in files.iter().zip(&offer.files) {
        if theirs.size == 0 && session.offer.text {
            // An empty text: nothing to show.
            continue;
        }
        if theirs.size == 0 {
            // croc never sends an empty file: the receiver makes it.
            let file = inner.ctx.begin_file(transfer, ours).await?;
            saved.push(file.commit().await?.name.as_str().to_owned());
            continue;
        }
        let mut request = Message::new(Kind::RecipientReady);
        request.b = message::write_request(theirs.index);
        let mut sink = if session.offer.text {
            Sink::Text(Vec::new(), transfer)
        } else {
            Sink::File(Box::new(inner.ctx.begin_file(transfer, ours).await?))
        };
        send_message(&mut session.control, &session.cipher, &request, t.idle).await?;
        let hash = receive_file(session, theirs.size, &mut sink, t.idle).await?;
        if Some(hash) != theirs.hash {
            return Err(ErrorInfo::new(
                ErrorCode::Network,
                "the file arrived other than it was sent",
            ));
        }
        send_message(
            &mut session.control,
            &session.cipher,
            &Message::new(Kind::CloseSender),
            t.idle,
        )
        .await?;
        loop {
            let m = next_message(&mut session.control, &session.cipher, deadline(t.idle)).await?;
            match m.kind {
                Kind::CloseRecipient => break,
                Kind::Error => {
                    return Err(ErrorInfo::new(ErrorCode::Refused, "the sender cancelled"));
                }
                Kind::Other | Kind::ExternalIp => {}
                _ => return Err(protocol("the croc sender spoke out of turn")),
            }
        }
        match sink {
            Sink::File(f) => saved.push(f.commit().await?.name.as_str().to_owned()),
            Sink::Text(bytes, _) => {
                let text = String::from_utf8_lossy(&bytes);
                inner.ctx.emit(Event::TextReceived {
                    transfer: transfer.id(),
                    from: offer.sender.clone(),
                    text: sukkula_core::text::message(&text),
                });
            }
        }
    }
    // Done; the sender echoes, and a lost echo costs nothing.
    let _ = send_message(
        &mut session.control,
        &session.cipher,
        &Message::new(Kind::Finished),
        CLEANUP_WAIT,
    )
    .await;
    let _ = next_message(
        &mut session.control,
        &session.cipher,
        deadline(CLEANUP_WAIT),
    )
    .await;
    Ok(saved)
}

/// One file's chunks, merged from the data rooms in order into `sink`;
/// returns their XXH64.
async fn receive_file(
    session: &mut Session,
    size: u64,
    sink: &mut Sink<'_>,
    idle: std::time::Duration,
) -> Result<[u8; 8], ErrorInfo> {
    let chunk = u64::try_from(CHUNK_BYTES).unwrap_or(u64::MAX);
    let count = size.div_ceil(chunk);
    let rooms = session.data.len();
    let mut heads: Vec<Option<(u64, Vec<u8>)>> = (0..rooms).map(|_| None).collect();
    let mut ended = vec![false; rooms];
    let mut hash = Xxh64::default();
    let mut next = 0u64;
    let broken = |what: &str| protocol(what);
    while next < count {
        if let Some(slot) = heads
            .iter_mut()
            .find(|h| h.as_ref().is_some_and(|(k, _)| *k == next))
            && let Some((_, data)) = slot.take()
        {
            hash.update(&data);
            sink.put(&data).await?;
            next = next.saturating_add(1);
            continue;
        }
        let waiting = heads.iter().zip(&ended).any(|(h, e)| h.is_none() && !*e);
        if !waiting {
            return Err(broken("a chunk of the file never came"));
        }
        let (i, frame) = tokio::time::timeout(
            idle,
            std::future::poll_fn(|cx| {
                for (i, conn) in session.data.iter_mut().enumerate() {
                    let idle_room = heads.get(i).is_some_and(Option::is_none)
                        && !ended.get(i).copied().unwrap_or(true);
                    if idle_room && let Poll::Ready(f) = conn.poll_frame(cx) {
                        return Poll::Ready((i, f));
                    }
                }
                Poll::Pending
            }),
        )
        .await
        .map_err(|_| broken("the croc sender stopped sending"))?;
        let Ok(frame) = frame else {
            if let Some(e) = ended.get_mut(i) {
                *e = true;
            }
            continue;
        };
        if frame == PING {
            continue;
        }
        let plain = session
            .cipher
            .open(&frame)
            .ok_or_else(|| broken("a chunk that does not decrypt"))?;
        let body = if session.offer.compressed {
            crypt::inflate(&plain, MAX_CHUNK_PLAIN)
                .ok_or_else(|| broken("a chunk that does not inflate"))?
        } else {
            plain
        };
        let (pos, data) = body
            .split_first_chunk::<8>()
            .ok_or_else(|| broken("a chunk without its position"))?;
        let pos = u64::from_le_bytes(*pos);
        let k = pos
            .checked_div(chunk)
            .filter(|_| pos.is_multiple_of(chunk))
            .ok_or_else(|| broken("a chunk out of place"))?;
        let expected = size.saturating_sub(pos).min(chunk);
        if k < next || k >= count || u64::try_from(data.len()).ok() != Some(expected) {
            return Err(broken("a chunk out of place"));
        }
        if let Some(h) = heads.get_mut(i) {
            *h = Some((k, data.to_vec()));
        }
    }
    Ok(hash.finish())
}
