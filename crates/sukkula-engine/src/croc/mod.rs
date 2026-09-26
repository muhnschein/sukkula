//! croc v10 (spec v0.5): files or a text to anyone with the code, through
//! a croc relay on the internet.
//!
//! # Why ours
//!
//! There is no croc library for Rust that speaks croc v10 with Go's
//! peers: the crates that carry the name speak protocols of their own, or
//! only half of croc's. So the protocol is implemented here, from croc
//! v10.7.0's source, as the Go and TypeScript clients croc ships speak
//! it, against Go's own test vectors, and checked against the Go binary
//! (`tests/croc_interop.rs`). `docs/SECURITY.md` says what that means for
//! a report.
//!
//! # The protocol, in short
//!
//! - **Relay** (`relay.rs`): every connection starts with a PAKE on croc's
//!   own curve, SIEC255 (`siec.rs`), under a public password, then the
//!   relay's password, its list of data ports and the room, sealed.
//! - **Rendezvous**: the room is the SHA-256 of the code's first four
//!   bytes (`code.rs`). The sender waits there; the receiver joins and
//!   says `handshake`.
//! - **Key** (`pake.rs`): a PAKE between the peers on P-256 under the
//!   rest of the code, then PBKDF2 with the sender's salt; AES-256-GCM from
//!   there on (`crypt.rs`). Both sides then join one data room per relay
//!   port.
//! - **Transfer** (`message.rs`): the sender lists its files; the receiver
//!   asks for them one at a time; the chunks, 32 KiB each with their
//!   position, go over the data rooms; each file is closed by both sides
//!   and checked against its XXH64 (`xxh64.rs`).
//!
//! # What is not done
//!
//! - No LAN shortcut: croc's sender runs a relay of its own and announces
//!   it by multicast. Ours answers a receiver's LAN probe with no
//!   addresses, and data always goes through the relay.
//! - No resume and no reconnection: a transfer asks for whole files and
//!   says it cannot reconnect, which Go peers accept.
//! - Hashes other than XXH64 (croc's `--hash`, which nobody changes) are
//!   refused before the user is asked.
//! - Folders arrive flat: each file under its own name in the download
//!   directory; empty folders and symbolic links are left out.
//!
//! # Trust
//!
//! The relay is someone else's server on the internet: until the peers
//! have their key it sees everything, and after it can drop, delay,
//! replay or reflect sealed messages (croc numbers nothing), though it
//! cannot read or forge them. The peer is whoever has the code. So every
//! frame is capped before it is read and every message before it is
//! inflated; the file list is checked before the user sees it and S1-S6
//! apply to it as to any offer; the sender serves only chunks of its own
//! files, each once; the receiver takes each chunk once, in order, from any
//! of the data rooms, and a file only whole and with its hash. Nothing of
//! ours -- no address, no machine id, no request -- goes to the peer before
//! the user has said yes, beyond the PAKE and croc's `externalip`, which
//! carries nothing.

mod code;
mod conn;
mod crypt;
#[doc(hidden)]
pub mod fuzzing;
mod message;
mod pake;
mod receive;
mod relay;
mod send;
mod siec;
mod words;
mod xxh64;

use std::sync::Arc;
use std::time::Duration;

use sukkula_core::Protocol;

use crate::adapter::{Adapter, BoxFuture, Outgoing};
use crate::api::{ErrorCode, ErrorInfo, SendTarget, TransferId};
use crate::by_code::{self, ByCode, Inner};
use crate::ctx::Ctx;

/// What the UI shows as the other side: croc has no names.
pub const PEER_LABEL: &str = "croc";

/// Most data rooms a send uses. croc's relay offers four.
const MAX_SEND_ROOMS: usize = 4;

pub use crate::by_code::{ANSWER_WAIT, MAX_CONNECTING_RECEIVES, PEER_WAIT, Tuning};

/// Bound on the goodbye.
const CLEANUP_WAIT: Duration = Duration::from_secs(2);

/// The adapter.
#[must_use]
pub fn adapter(ctx: Arc<Ctx>) -> Arc<dyn Adapter> {
    adapter_with(ctx, Tuning::default())
}

/// The adapter with shorter timeouts.
#[must_use]
pub fn adapter_with(ctx: Arc<Ctx>, tuning: Tuning) -> Arc<dyn Adapter> {
    by_code::adapter::<Croc>(ctx, tuning)
}

struct Croc;

impl ByCode for Croc {
    const PROTOCOL: Protocol = Protocol::Croc;

    fn send(
        inner: &Arc<Inner>,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> Result<TransferId, ErrorInfo> {
        send::start(inner, target, items)
    }

    fn receive(
        inner: Arc<Inner>,
        code: String,
    ) -> BoxFuture<'static, Result<TransferId, ErrorInfo>> {
        Box::pin(receive::start(inner, code))
    }
}

/// `N` bytes from the system's generator.
///
/// # Errors
///
/// [`ErrorCode::Internal`]: the generator failed.
fn random<const N: usize>() -> Result<[u8; N], ErrorInfo> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|_| ErrorInfo::new(ErrorCode::Internal, "no randomness"))?;
    Ok(out)
}

/// A protocol violation by the peer or the relay.
fn protocol(what: &str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Network, what)
}

/// Runs `fut` unless `token` is cancelled first.
async fn cancellable<T>(
    token: &tokio_util::sync::CancellationToken,
    fut: impl std::future::Future<Output = Result<T, ErrorInfo>>,
) -> Result<T, ErrorInfo> {
    tokio::select! {
        r = fut => r,
        () = token.cancelled() => Err(crate::ctx::cancelled()),
    }
}

fn deadline(after: Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(after).unwrap_or(now)
}

/// Lower-case hex, for test vectors.
#[cfg(test)]
pub(crate) fn hexs(bytes: &[u8]) -> String {
    sukkula_core::hex::encode(bytes)
}

/// Hex to bytes, for test vectors.
#[cfg(test)]
pub(crate) fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[cfg(test)]
mod loopback;
