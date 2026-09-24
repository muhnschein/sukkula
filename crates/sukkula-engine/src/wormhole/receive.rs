//! F-MW2 and F-MW3: receive with a code the user typed.
//!
//! The order is what S5 needs: the code is checked before the network is
//! touched; the offer is read and shown; until the user says yes nothing of
//! ours goes to the peer but the PAKE and version messages every wormhole
//! exchanges -- not our transit hints, so not even our addresses -- and no
//! transit connection exists. Only after the yes do we send hints and the
//! `file_ack`, connect, and write through the inbox.
//!
//! The transfer is registered when the offer is accepted (`Ctx::offer`, as
//! for every protocol), and that is when `receive_code` returns its id and
//! the UI gets its `Reply`. Until then the UI shows the command as pending:
//! a wrong code, an unreachable server or a declined offer ends it with an
//! error reply instead, so there is never a transfer that did not happen.
//! Every wait before that point is bounded: [`super::Tuning::handshake`]
//! for the mailbox and the key exchange, [`super::Tuning::idle`] for the
//! offer, and the consent timeout for the user.

use std::sync::Arc;

use magic_wormhole::transfer::APP_CONFIG;
use magic_wormhole::transit::{Abilities, TransitRole};
use magic_wormhole::{MailboxConnection, Wormhole};
use sha2::{Digest, Sha256};
use sukkula_core::hex;
use sukkula_core::offer::{Offer, OfferError};
use tokio::time::Instant;

use super::session::{
    CatchUnwind, Panicked, Servers, Session, lib, panicked, protocol, wormhole_error,
};
use super::transit::{self, Role};
use super::wire::{self, OfferMsg, PeerMsg, TheirTransit};
use super::{CLEANUP_WAIT, Inner, MAX_EMPTY_RECORDS, code, mailbox};
use crate::api::{ErrorCode, ErrorInfo, Event, TransferId};
use crate::ctx::{Accepted, Declined, TransferHandle, cancelled};
use sukkula_core::consent::Refusal;

/// The v1 decline, as the reference clients word it.
const REJECTED: &str = "transfer rejected";

/// Receives with `code`: returns once the user has accepted the offer, with
/// the transfer the rest runs as.
pub(super) async fn start(inner: Arc<Inner>, raw_code: String) -> Result<TransferId, ErrorInfo> {
    let code = code::parse(&raw_code)?;
    let servers = Servers::from_settings(&inner.ctx.settings().wormhole)?;
    let shutdown = inner.ctx.shutdown_token().clone();
    let (accepted, session, their) = {
        let _slot = inner.connecting_slot().ok_or_else(|| {
            ErrorInfo::new(ErrorCode::TooLarge, "too many receives are connecting")
        })?;
        let before = CatchUnwind::new(until_accepted(&inner, code, &servers));
        tokio::select! {
            r = before => r.unwrap_or_else(|Panicked| Err(panicked()))?,
            () = shutdown.cancelled() => return Err(cancelled()),
        }
    };
    let id = accepted.transfer.id();
    tokio::spawn(async move {
        let Accepted {
            offer, transfer, ..
        } = accepted;
        let mut session = session;
        let result = CatchUnwind::new(after_accept(
            &inner,
            &transfer,
            &offer,
            &mut session,
            their,
            &servers,
        ))
        .await;
        let result = result.unwrap_or_else(|Panicked| Err(panicked()));
        let reason = match &result {
            Ok(_) => None,
            Err(_) if transfer.is_cancelled() => Some("transfer cancelled"),
            Err(_) => Some("transfer failed"),
        };
        if !inner.ctx.shutdown_token().is_cancelled() {
            session.goodbye(reason).await;
        }
        transfer.finish_with(result);
    });
    Ok(id)
}

type Ready = (Accepted, Session, Option<TheirTransit>);

/// Connects, reads the offer and asks the user.
async fn until_accepted(
    inner: &Inner,
    code: magic_wormhole::Code,
    servers: &Servers,
) -> Result<Ready, ErrorInfo> {
    let t = inner.tuning;
    let guard = mailbox::open(&servers.mailbox, &t, inner.ctx.shutdown_token()).await?;
    let config = APP_CONFIG.rendezvous_url(guard.url.clone().into());
    // `false`: a nameplate nobody holds is a wrong code, not a new mailbox.
    let mailbox = lib(
        t.handshake,
        "connecting to the mailbox",
        MailboxConnection::connect(config, code, false),
    )
    .await?
    .map_err(|e| guard.verdict().unwrap_or_else(|| wormhole_error(&e)))?;
    let wormhole = lib(t.handshake, "the key exchange", Wormhole::connect(mailbox))
        .await?
        .map_err(|e| guard.verdict().unwrap_or_else(|| wormhole_error(&e)))?;
    let mut session = Session::new(wormhole, guard);

    let (offer, mut their) = match read_offer(inner, &mut session).await {
        Ok(v) => v,
        Err(e) => {
            session.goodbye(Some("transfer failed")).await;
            return Err(e);
        }
    };
    let Some(raw) = wire::raw_offer(&offer) else {
        session.goodbye(Some("unsupported offer")).await;
        return Err(ErrorInfo::new(
            ErrorCode::Refused,
            "the offer is of a kind Sukkula does not take",
        ));
    };

    // Ask, while still listening: a sender that gives up takes its offer
    // off the screen. The consent timeout bounds this wait.
    let answer = {
        // Boxed so that dropping it -- which withdraws the offer -- can
        // happen before the goodbye rather than after.
        let mut asking = Box::pin(inner.ctx.offer(raw));
        loop {
            tokio::select! {
                r = &mut asking => break r.map_err(Some),
                m = session.receive_unbounded() => match m {
                    Ok(PeerMsg::Transit(tr)) => { their.get_or_insert(tr); }
                    Ok(PeerMsg::Other) => {}
                    Ok(PeerMsg::Error) => break Err(None),
                    Ok(PeerMsg::Offer(_) | PeerMsg::Answer(_)) => {
                        break Err(Some(Declined::Invalid(OfferError::Empty)));
                    }
                    Err(e) => {
                        drop(asking);
                        session.goodbye(Some("transfer failed")).await;
                        return Err(e);
                    }
                },
            }
        }
    };
    match answer {
        Ok(accepted) => Ok((accepted, session, their)),
        Err(None) => {
            session.goodbye(None).await;
            Err(ErrorInfo::new(ErrorCode::Refused, "the sender cancelled"))
        }
        Err(Some(declined)) => {
            session.goodbye(Some(REJECTED)).await;
            Err(declined_error(&declined))
        }
    }
}

/// The first offer, with any transit hints that came before it.
async fn read_offer(
    inner: &Inner,
    session: &mut Session,
) -> Result<(OfferMsg, Option<TheirTransit>), ErrorInfo> {
    let deadline = deadline(inner.tuning.idle);
    let mut their = None;
    loop {
        match session.receive_until(deadline).await? {
            PeerMsg::Transit(tr) => {
                their.get_or_insert(tr);
            }
            PeerMsg::Offer(o) => return Ok((o, their)),
            PeerMsg::Error => {
                return Err(ErrorInfo::new(ErrorCode::Refused, "the sender cancelled"));
            }
            PeerMsg::Answer(_) => return Err(protocol("the sender answered an offer nobody made")),
            PeerMsg::Other => {}
        }
    }
}

fn declined_error(d: &Declined) -> ErrorInfo {
    match d {
        Declined::Invalid(
            OfferError::FileTooLarge(_) | OfferError::OfferTooLarge | OfferError::TextTooLarge,
        ) => ErrorInfo::new(ErrorCode::TooLarge, "the offer is over the limits"),
        Declined::Invalid(_) => ErrorInfo::new(ErrorCode::Refused, "the offer was malformed"),
        Declined::Refused(Refusal::Declined) => ErrorInfo::new(ErrorCode::Refused, "declined"),
        Declined::Refused(Refusal::TimedOut) => {
            ErrorInfo::new(ErrorCode::Refused, "the offer was not answered in time")
        }
        Declined::Refused(Refusal::Busy) => {
            ErrorInfo::new(ErrorCode::Refused, "too many offers are waiting")
        }
        Declined::Refused(Refusal::Shutdown) => cancelled(),
        Declined::NoSpace => ErrorInfo::new(ErrorCode::Storage, "not enough free space"),
        Declined::Busy => ErrorInfo::new(ErrorCode::TooLarge, "too many transfers running"),
    }
}

/// Everything after the yes.
async fn after_accept(
    inner: &Inner,
    transfer: &TransferHandle,
    offer: &Offer,
    session: &mut Session,
    their: Option<TheirTransit>,
    servers: &Servers,
) -> Result<Vec<String>, ErrorInfo> {
    super::session::cancellable(transfer.token(), async {
        match (&offer.text, offer.files.first()) {
            (Some(text), None) => {
                inner.ctx.emit(Event::TextReceived {
                    transfer: transfer.id(),
                    from: offer.sender.clone(),
                    text: text.clone(),
                });
                // The text is here; a lost ack only leaves the sender
                // unsure, so it does not fail the transfer.
                let _ = session.send(inner.tuning.idle, wire::message_ack()).await;
                Ok(Vec::new())
            }
            (None, Some(_)) => receive_file(inner, transfer, offer, session, their, servers).await,
            _ => Err(protocol("the offer is neither one file nor one text")),
        }
    })
    .await
}

async fn receive_file(
    inner: &Inner,
    transfer: &TransferHandle,
    offer: &Offer,
    session: &mut Session,
    their: Option<TheirTransit>,
    servers: &Servers,
) -> Result<Vec<String>, ErrorInfo> {
    let t = inner.tuning;
    let file = offer
        .files
        .first()
        .ok_or_else(|| protocol("the offer has no file"))?;
    let their = match their {
        Some(tr) => tr,
        None => loop {
            match session.receive_until(deadline(t.idle)).await? {
                PeerMsg::Transit(tr) => break tr,
                PeerMsg::Error => {
                    return Err(ErrorInfo::new(ErrorCode::Refused, "the sender cancelled"));
                }
                _ => {}
            }
        },
    };

    let (transit_key, relay_token) = session.transit_key();
    let targets = transit::targets(&servers.relay, &their, inner.ctx.reach());
    let plan = transit::plan(
        targets,
        &relay_token,
        Role::Follower,
        &t,
        inner.ctx.reach(),
        transfer.token(),
    )
    .await?;
    let connector = lib(
        t.handshake,
        "preparing transit",
        magic_wormhole::transit::init(Abilities::FORCE_RELAY, None, plan.ours.clone()),
    )
    .await?
    .map_err(|_| ErrorInfo::new(ErrorCode::Network, "transit could not start"))?;
    session
        .send(t.idle, wire::transit(servers.relay_hint()))
        .await?;
    session.send(t.idle, wire::file_ack()).await?;
    let (mut transit, _info) = lib(
        t.handshake,
        "connecting to the sender",
        connector.connect(
            TransitRole::Follower,
            transit_key,
            Abilities::FORCE_RELAY,
            Arc::new(plan.theirs.clone()),
        ),
    )
    .await?
    .map_err(|_| ErrorInfo::new(ErrorCode::Network, "no transit connection to the sender"))?;
    plan.connected();

    // S3: staged, capped at the declared size, gone on any failure.
    let mut incoming = inner.ctx.begin_file(transfer, file).await?;
    let mut hasher = Sha256::new();
    let mut empty = 0u32;
    let broken = || ErrorInfo::new(ErrorCode::Network, "the transit connection failed");
    while incoming.remaining() > 0 {
        let record = lib(t.idle, "waiting for the sender", transit.receive_record())
            .await?
            .map_err(|_| broken())?;
        if record.is_empty() {
            empty = empty.saturating_add(1);
            if empty > MAX_EMPTY_RECORDS {
                return Err(protocol("the sender sent empty records"));
            }
            continue;
        }
        let len = u64::try_from(record.len()).unwrap_or(u64::MAX);
        if len > incoming.remaining() {
            return Err(ErrorInfo::new(
                ErrorCode::Network,
                "the sender sent more than it offered",
            ));
        }
        hasher.update(&record);
        incoming.write(&record).await?;
    }
    let saved = incoming.commit().await?;
    let digest = hex::encode(&hasher.finalize());
    // The file is placed: an ack that does not arrive leaves the sender
    // unsure, not us.
    let ack = wire::transit_ack(&digest);
    let _ = lib(t.idle, "confirming to the sender", async {
        transit.send_record(&ack).await?;
        transit.flush().await
    })
    .await;
    drop(transit);
    plan.finish(CLEANUP_WAIT).await;
    Ok(vec![saved.name.as_str().to_owned()])
}

fn deadline(after: std::time::Duration) -> Instant {
    Instant::now()
        .checked_add(after)
        .unwrap_or_else(Instant::now)
}
