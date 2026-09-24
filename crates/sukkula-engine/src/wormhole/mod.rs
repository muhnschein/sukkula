//! Magic Wormhole (F-MW), transfer protocol v1, over `magic-wormhole` 0.8.1.
//!
//! # What of the library is used
//!
//! Only its core and transit layers: [`magic_wormhole::MailboxConnection`]
//! (`create` with two words, `connect` without allocating),
//! [`magic_wormhole::Wormhole`] (`connect`, `send`, `receive`, `close`,
//! `key`), the transit key derivation, and [`magic_wormhole::transit`]
//! (`init`, `TransitConnector::connect`, `Transit::send_record`,
//! `receive_record`, `flush`). The v1 "text-or-file-xfer" messages on top
//! of those -- a dozen lines of JSON, see `wire.rs` -- are ours.
//!
//! Not used: `transfer::send_file`, `request_file` and `ReceiveRequest`,
//! the library's v1 file API, because it cannot carry text either way (it
//! has no text API, and `request_file` insists on the sender's transit
//! hints before the offer, which a text sender never sends -- the Python
//! client's `wormhole send --text`), because it reads peer messages of any
//! size, and because its receive loop underflows (`remaining_size -=
//! plaintext.len()`) on a last record longer than what is left. Nothing of
//! transfer v2 is compiled at all: it sits behind the
//! `experimental-transfer-v2` feature, which is off, and its accept path is
//! the one with `panic!`/`expect` on unexpected offer shapes (spec §5).
//!
//! # Why guards
//!
//! Reading the v1 path turned up hangs, panics and unbounded allocations
//! reachable from the mailbox server, the transit relay, the STUN server
//! or the peer. The ones that cannot be fixed from outside the library are
//! kept from ever being reached:
//!
//! - the library talks to the mailbox through a loopback WebSocket proxy
//!   (`mailbox.rs`) that holds the server to size, count and shape limits;
//! - every transit connection goes through a loopback TCP guard
//!   (`transit.rs`) that caps record lengths, and the library is told
//!   relay-only, so it neither listens nor asks STUN;
//! - every library future runs inside [`session::CatchUnwind`] and a
//!   timeout, so a panic that is still reachable fails the transfer rather
//!   than the task.
//!
//! # Upstream findings (magic-wormhole 0.8.1)
//!
//! | # | Where | What | Reachable from | Here |
//! | --- | --- | --- | --- | --- |
//! | W1 | `util::hashcash` | mints hashcash of any `bits` in a loop that never yields | mailbox server, anyone on the `ws://` path | guard refuses `bits` > 20 |
//! | W2 | `Wormhole::receive` | `todo!()` on a non-numeric phase | server, peer | guard drops such phases |
//! | W3 | `key::decrypt_data` | `split_at(24)` on a shorter body | server, peer | guard drops short bodies |
//! | W4 | `WsConnection::receive_message` | `expect` when the stream ends | server closing | contained ([`session::CatchUnwind`]) |
//! | W5 | rendezvous | 64 MiB messages, unbounded queue and phase set | server | guard: 1 MiB, 128 messages, 4 MiB |
//! | W6 | `read_transit_message` | `Vec::with_capacity(len)` from the unauthenticated length prefix (up to 4 GiB) | peer, relay, path | transit guard caps records |
//! | W7 | `v1::receive_records` | `remaining_size -= len` underflows on a long last record | peer | not used; ours checks first |
//! | W8 | `transport::tcp_get_external_ip` | `buf[20..][..len]` with the STUN server's `len` | STUN server, path | STUN never asked (relay-only) |
//! | W9 | `transport::wrap_tcp_connection` | `peer_addr().expect` after the peer reset | anyone reaching our listener | never listens (relay-only) |
//! | W10 | `transit::init` | asks `stun.piegames.de` (hard-coded) for our address | -- (privacy) | never asked |
//! | W11 | `transit::init` | listens on `[::]` for anyone, one handshake at a time | anyone (slow-loris) | never listens |
//! | W12 | `WelcomeMessage` | a `permission-required` without a `none` key does not parse, so no hashcash server is reachable at all | -- (interop) | -- |
//! | W13 | `Transit::send_record` | `assert!` on an empty record | our own calls | we never send one |
//!
//! All are worth reporting upstream; W1, W2, W3, W6 first. Checked and
//! fine: `Code::from_str`'s entropy check passes all 65,536 two-word PGP
//! codes, so a strictly validated code never fails it.
//!
//! # Threads
//!
//! The library's I/O is async-io's, and polling it from tokio works: its
//! reactor runs in a thread of its own, "async-io", which async-io starts
//! the first time anything registers with it -- here, the first wormhole
//! transfer, not engine start -- and never stops. Once the transfer is
//! over nothing is registered, and the thread sits in `epoll_wait` with no
//! timeout: idle, holding no socket, never calling into Sukkula. async-io
//! has no API to stop it, so it outlives `Engine::stop`. Host-name lookups
//! by the library go through the `blocking` crate's pool, whose threads
//! exit on their own once idle.
//!
//! # Sending (F-MW1)
//!
//! One file or one text. The transfer is registered and its id returned at
//! once; the file is opened once, read-only and non-blocking, and the
//! handle checked (regular, the size the hub measured) before a code is
//! allocated; the code (two words) follows in `Event::WormholeCode` with a
//! QR code of the `wormhole-transfer:` URI. The receiver then has
//! [`Tuning::peer_wait`] to type it and [`Tuning::answer_wait`] to accept.
//!
//! # Receiving (F-MW2, F-MW3)
//!
//! See `receive.rs`: the code is checked strictly before the network is
//! touched, and nothing about us reaches the peer -- no hints, no
//! connection, no ack -- before the user accepts. A folder arrives as the
//! single `.zip` (Python) or `.tar` (magic-wormhole's own `send_folder`) the
//! sender packed and is saved as that file, unopened.
//!
//! # Servers (F-MW4)
//!
//! The library's defaults, `ws://relay.magic-wormhole.io:4000/v1` and
//! `tcp://transit.magic-wormhole.io:4001`, unless `Settings.wormhole` names
//! others; the URLs are checked again at every use. `wss://` works, over
//! the guard's rustls, which is LocalSend's rustls with ring and the
//! platform verifier: no second TLS stack. The library's own TLS features
//! would have added `futures-rustls` and, in the only variant without the
//! verifier duplicate, a baked-in root store.

mod code;
mod mailbox;
mod receive;
mod send;
mod session;
#[cfg(test)]
mod sweep;
mod transit;
mod wire;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sukkula_core::Protocol;
use sukkula_core::limits::{HANDSHAKE_TIMEOUT, NETWORK_IDLE_TIMEOUT};

use crate::adapter::{Adapter, BoxFuture, Outgoing};
use crate::api::{ErrorInfo, SendTarget, TransferId};
use crate::ctx::Ctx;

/// Words in a code we allocate (F-MW1).
const CODE_WORDS: usize = 2;

/// What the UI shows as the other side: the protocol has no names.
pub const PEER_LABEL: &str = "Magic Wormhole";

/// Plaintext bytes per transit record we send, as the reference clients do.
const RECORD_BYTES: usize = 16 * 1024;

/// Largest transit record accepted from the wire, framing aside: 4 MiB of
/// plaintext, a 24-byte nonce and a 16-byte tag. The reference clients
/// send 16 KiB.
const MAX_RECORD_WIRE_BYTES: u32 = 4 * 1024 * 1024 + 24 + 16;

/// Most peer messages read in one session. A transfer needs four.
const MAX_PEER_MESSAGES: usize = 32;

/// Largest decrypted peer message. A 64 KiB text JSON-escaped at worst.
const MAX_PEER_MESSAGE_BYTES: usize = 512 * 1024;

/// Direct hints kept from the peer.
const MAX_DIRECT_HINTS: usize = 16;

/// Relays kept from the peer.
const MAX_RELAY_HINTS: usize = 2;

/// Empty records tolerated in one file. No reference client sends any.
const MAX_EMPTY_RECORDS: u32 = 16;

/// Bound on the goodbye (an error message, the mailbox close).
const CLEANUP_WAIT: Duration = Duration::from_secs(2);

/// Receives that may be between a typed code and an answered offer at
/// once. Each holds a mailbox connection and a guard, and the consent
/// queue takes only two offers anyway (F-C3).
pub const MAX_CONNECTING_RECEIVES: usize = 4;

/// How long the receiver may take to type our code.
pub const PEER_WAIT: Duration = Duration::from_secs(10 * 60);

/// How long the receiver may take to accept.
pub const ANSWER_WAIT: Duration = Duration::from_secs(5 * 60);

/// The adapter's timeouts. [`Tuning::default`] is the spec's; tests
/// shorten them. They can only be shortened: [`adapter_with`] clamps every
/// one to its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// Mailbox connection, key exchange, transit connection
    /// ([`HANDSHAKE_TIMEOUT`]).
    pub handshake: Duration,
    /// Any read or write making no progress ([`NETWORK_IDLE_TIMEOUT`]).
    pub idle: Duration,
    /// The receiver typing our code ([`PEER_WAIT`]).
    pub peer_wait: Duration,
    /// The receiver accepting ([`ANSWER_WAIT`]).
    pub answer_wait: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            handshake: HANDSHAKE_TIMEOUT,
            idle: NETWORK_IDLE_TIMEOUT,
            peer_wait: PEER_WAIT,
            answer_wait: ANSWER_WAIT,
        }
    }
}

impl Tuning {
    fn clamped(self) -> Tuning {
        let d = Tuning::default();
        Tuning {
            handshake: self.handshake.min(d.handshake),
            idle: self.idle.min(d.idle),
            peer_wait: self.peer_wait.min(d.peer_wait),
            answer_wait: self.answer_wait.min(d.answer_wait),
        }
    }
}

/// The adapter.
#[must_use]
pub fn adapter(ctx: Arc<Ctx>) -> Arc<dyn Adapter> {
    adapter_with(ctx, Tuning::default())
}

/// The adapter with shorter timeouts.
#[must_use]
pub fn adapter_with(ctx: Arc<Ctx>, tuning: Tuning) -> Arc<dyn Adapter> {
    Arc::new(WormholeAdapter {
        inner: Arc::new(Inner {
            ctx,
            tuning: tuning.clamped(),
            connecting: AtomicUsize::new(0),
        }),
    })
}

struct Inner {
    ctx: Arc<Ctx>,
    tuning: Tuning,
    /// Receives before their offer is answered.
    connecting: AtomicUsize,
}

impl Inner {
    /// A slot for one receive before its answer, if one is free.
    fn connecting_slot(&self) -> Option<Slot<'_>> {
        self.connecting
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_CONNECTING_RECEIVES).then_some(n.saturating_add(1))
            })
            .ok()
            .map(|_| Slot(&self.connecting))
    }
}

/// Gives its slot back when dropped.
struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
    }
}

struct WormholeAdapter {
    inner: Arc<Inner>,
}

impl Adapter for WormholeAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Wormhole
    }

    /// Receiving is by code only (F-MW2): nothing listens.
    fn receives(&self) -> bool {
        false
    }

    fn start_receiving(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_receiving(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn send(
        &self,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        Box::pin(async move { send::start(&self.inner, target, items) })
    }

    fn receive_code(&self, code: String) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        Box::pin(receive::start(self.inner.clone(), code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuning_can_only_be_shortened() {
        let long = Tuning {
            handshake: Duration::from_secs(3600),
            idle: Duration::from_secs(1),
            peer_wait: Duration::from_secs(3600 * 24),
            answer_wait: Duration::ZERO,
        };
        let c = long.clamped();
        assert_eq!(c.handshake, HANDSHAKE_TIMEOUT);
        assert_eq!(c.idle, Duration::from_secs(1));
        assert_eq!(c.peer_wait, PEER_WAIT);
        assert_eq!(c.answer_wait, Duration::ZERO);
    }
}
