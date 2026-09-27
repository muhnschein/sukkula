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
//!   (`mailbox.rs`) that holds the server to size, count and shape limits,
//!   and peer messages to the order the library reads them in;
//! - every transit connection goes through a loopback TCP guard
//!   (`transit.rs`) that caps record lengths and makes at most three
//!   connection attempts per transfer to addresses the peer chose, and the
//!   library is told relay-only, so it neither listens nor asks STUN;
//! - every library future runs inside `session::CatchUnwind` and a
//!   timeout, so a panic that is still reachable fails the transfer rather
//!   than the task.
//!
//! # Upstream findings (magic-wormhole 0.8.1)
//!
//! | # | Where | What | Reachable from | Here |
//! | --- | --- | --- | --- | --- |
//! | W1 | `util::hashcash` | mints hashcash of any `bits` in a loop that never yields | mailbox server, anyone on the `ws://` path | guard refuses `bits` > 20; the mailbox connection runs on a thread of its own (`session::off_runtime`), so a mint holds no engine worker, timeout or cancel |
//! | W2 | `Wormhole::receive` | `todo!()` on a non-numeric phase -- including a `pake` or `version` the key exchange did not take, since `connect` takes the first two new peer messages whatever their phase | server, peer | guard lets a new peer message through only in the place the library reads it: `pake`, `version`, then numbers |
//! | W3 | `key::decrypt_data` | `split_at(24)` on a shorter body -- including a short `pake` that arrives second and is decrypted as the version message | server, peer | guard drops short sealed bodies, and holds messages to that order |
//! | W4 | `WsConnection::receive_message` | `expect` when the stream ends | server closing | contained (`session::CatchUnwind`) |
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
//! The library's mailbox connection (`MailboxConnection::create` and
//! `connect`) runs on a thread of its own, "sukkula-mailbox", because it
//! mints the server's hashcash without yielding (W1). The engine waits for
//! it on a channel, bounded by the handshake timeout and ended at once by a
//! cancel or `Engine::stop`; the thread is then told to stop and does so at
//! its next poll, which is after the mint if one is running (20 bits at
//! most). At most `session::MAX_OFF_RUNTIME` such threads exist at once.
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

pub(crate) mod code;
#[doc(hidden)]
pub mod fuzzing;
mod mailbox;
mod receive;
mod send;
mod session;
#[cfg(test)]
mod sweep;
mod transit;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use sukkula_core::Protocol;

use crate::adapter::{Adapter, BoxFuture, CodeReceive, Outgoing};
use crate::api::{ErrorInfo, SendTarget, TransferId};
use crate::by_code::{self, ByCode, Inner};
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

pub use crate::by_code::{ANSWER_WAIT, MAX_CONNECTING_RECEIVES, PEER_WAIT, Tuning};

/// The adapter.
#[must_use]
pub fn adapter(ctx: Arc<Ctx>) -> Arc<dyn Adapter> {
    adapter_with(ctx, Tuning::default())
}

/// The adapter with shorter timeouts.
#[must_use]
pub fn adapter_with(ctx: Arc<Ctx>, tuning: Tuning) -> Arc<dyn Adapter> {
    by_code::adapter::<Wormhole>(ctx, tuning)
}

struct Wormhole;

impl ByCode for Wormhole {
    const PROTOCOL: Protocol = Protocol::Wormhole;

    fn send(
        inner: &Arc<Inner>,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> Result<TransferId, ErrorInfo> {
        send::start(inner, target, items)
    }

    fn receive(
        inner: Arc<Inner>,
        request: CodeReceive,
    ) -> BoxFuture<'static, Result<TransferId, ErrorInfo>> {
        Box::pin(receive::start(inner, request.code, request.mailbox_url))
    }
}
