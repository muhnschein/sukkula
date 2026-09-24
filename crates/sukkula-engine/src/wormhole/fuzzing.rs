//! Entry points for the fuzz targets in `fuzz/` (spec §7 "Parsers"): the
//! parsers of this module that eat bytes from the peer, the mailbox server
//! or the user, reachable without a network. Each is a thin wrapper that
//! runs the real code and hands back what it produced in public types; none
//! is used by the engine itself.

use std::net::{IpAddr, SocketAddr};

use sukkula_core::config::WormholeSettings;
use sukkula_core::offer::RawOffer;
use sukkula_core::reach::ReachPolicy;

use super::mailbox::{self, Budget};
use super::session::{Endpoint, Servers, protocol};
use super::wire::{self, Answer, PeerMsg};
use super::{MAX_DIRECT_HINTS, MAX_PEER_MESSAGE_BYTES, MAX_RELAY_HINTS, code, transit};
use crate::api::ErrorInfo;

/// Longest code accepted, after trimming.
pub const CODE_BYTES: usize = code::MAX_CODE_BYTES;
/// Most messages the guard lets through on one mailbox connection.
pub const SERVER_MESSAGES: usize = mailbox::MAX_SERVER_MESSAGES;
/// Most bytes the guard lets through on one mailbox connection.
pub const SERVER_BYTES: usize = mailbox::MAX_SERVER_BYTES;
/// Hardest hashcash the guard lets the library mint.
pub const HASHCASH_BITS: u64 = mailbox::MAX_HASHCASH_BITS;
/// Most transit connections one transfer tries.
pub const TARGETS: usize = transit::MAX_TARGETS;
/// Most direct hints kept from one transit message.
pub const DIRECT_HINTS: usize = MAX_DIRECT_HINTS;
/// Most relays kept from one transit message.
pub const RELAY_HINTS: usize = MAX_RELAY_HINTS;
/// Largest peer message parsed at all.
pub const PEER_MESSAGE_BYTES: usize = MAX_PEER_MESSAGE_BYTES;

/// One decrypted peer message, as the adapter reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerMessage {
    /// Transit hints, reduced and bounded, and where the adapter would
    /// connect because of them.
    Transit(Transit),
    /// An offer, as the consent dialog's [`RawOffer`]; `None` for a kind
    /// the adapter declines without asking.
    Offer(Option<RawOffer>),
    /// `message_ack`, and whether it said `ok`.
    MessageAck(bool),
    /// `file_ack`, and whether it said `ok`.
    FileAck(bool),
    /// Another answer.
    OtherAnswer,
    /// `error`.
    Error,
    /// A message this version ignores.
    Other,
}

/// A transit message, reduced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transit {
    /// It announced `direct-tcp-v1`.
    pub direct: bool,
    /// It announced `relay-v1`.
    pub relay: bool,
    /// The direct hints kept.
    pub direct_hints: Vec<(IpAddr, u16)>,
    /// The relays kept, each its endpoints.
    pub relays: Vec<Vec<(String, u16)>>,
    /// What `transit::targets` makes of it with the default relay and
    /// reach policy: every connection this message can cause, in order.
    pub targets: Vec<Target>,
}

/// One connection the adapter would try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Straight to one of the peer's direct hints.
    Direct(SocketAddr),
    /// Through a relay.
    Relay {
        /// Its endpoints, tried in order.
        endpoints: Vec<(String, u16)>,
        /// Ours rather than the peer's.
        ours: bool,
    },
}

fn endpoints(list: &[Endpoint]) -> Vec<(String, u16)> {
    list.iter().map(|e| (e.host.clone(), e.port)).collect()
}

/// Parses one decrypted peer message as `Session::receive_until` does --
/// the size cap, then `wire::parse` -- and turns an offer into the raw
/// offer the adapter asks the user about.
///
/// # Errors
///
/// What the adapter would fail the transfer with.
pub fn peer_message(bytes: &[u8]) -> Result<PeerMessage, ErrorInfo> {
    if bytes.len() > MAX_PEER_MESSAGE_BYTES {
        return Err(protocol("the peer sent an oversized message"));
    }
    Ok(match wire::parse(bytes)? {
        PeerMsg::Transit(t) => {
            let ours = Servers::from_settings(&WormholeSettings::default())?.relay;
            let targets = transit::targets(&ours, &t, ReachPolicy::default())
                .into_iter()
                .map(|target| match target {
                    transit::Target::Direct(a) => Target::Direct(a),
                    transit::Target::Relay { endpoints: e, ours } => Target::Relay {
                        endpoints: endpoints(&e),
                        ours,
                    },
                })
                .collect();
            PeerMessage::Transit(Transit {
                direct: t.direct,
                relay: t.relay,
                direct_hints: t.direct_hints.clone(),
                relays: t.relays.iter().map(|r| endpoints(r)).collect(),
                targets,
            })
        }
        PeerMsg::Offer(o) => PeerMessage::Offer(wire::raw_offer(&o)),
        PeerMsg::Answer(Answer::Message(ok)) => PeerMessage::MessageAck(ok),
        PeerMsg::Answer(Answer::File(ok)) => PeerMessage::FileAck(ok),
        PeerMsg::Answer(Answer::Other) => PeerMessage::OtherAnswer,
        PeerMsg::Error => PeerMessage::Error,
        PeerMsg::Other => PeerMessage::Other,
    })
}

/// Whether the peer's last transit record acknowledges `sha256_hex`.
#[must_use]
pub fn transit_ack_matches(record: &[u8], sha256_hex: &str) -> bool {
    wire::transit_ack_matches(record, sha256_hex)
}

/// A typed code as `code::parse` accepts it, in the form the library is
/// given.
///
/// # Errors
///
/// `ErrorCode::BadCode`.
pub fn code(raw: &str) -> Result<String, ErrorInfo> {
    code::parse(raw).map(|c| c.to_string())
}

/// What the mailbox guard has let through on one connection.
#[derive(Debug, Default)]
pub struct ServerBudget(Budget);

/// The mailbox guard's check of one server message, against the budget of
/// the connection it arrived on.
///
/// # Errors
///
/// The guard's verdict: the connection is closed.
pub fn server_message(text: &str, budget: &mut ServerBudget) -> Result<(), ErrorInfo> {
    mailbox::check_server_message(text, &mut budget.0)
}
