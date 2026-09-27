//! What Magic Wormhole and croc share: both send to a person who types the
//! code this phone shows, and receive by typing theirs, through servers on
//! the internet. Nothing listens; the timeouts, the cap on receives waiting
//! for their answer, the adapter itself, the reading of the user's answer
//! and the opening of a file to send are the same for both, and live here.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use sukkula_core::Protocol;
use sukkula_core::consent::Refusal;
use sukkula_core::limits::{HANDSHAKE_TIMEOUT, NETWORK_IDLE_TIMEOUT};
use sukkula_core::offer::OfferError;

use crate::adapter::{Adapter, BoxFuture, CodeReceive, Outgoing, OutgoingFile};
use crate::api::{ErrorCode, ErrorInfo, QrCode, SendTarget, TransferId};
use crate::ctx::{Ctx, Declined, cancelled};

/// Receives that may be between a typed code and an answered offer at
/// once. Each holds a connection to a server, and the consent queue takes
/// only two offers anyway (F-C3).
pub const MAX_CONNECTING_RECEIVES: usize = 4;

/// How long the receiver may take to type our code.
pub const PEER_WAIT: Duration = Duration::from_secs(10 * 60);

/// How long the receiver may take to accept.
pub const ANSWER_WAIT: Duration = Duration::from_secs(5 * 60);

/// An adapter's timeouts. [`Tuning::default`] is the spec's; tests
/// shorten them. They can only be shortened: each adapter's `adapter_with`
/// clamps every one to its default ([`Tuning::clamped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// Each step of reaching the servers and the peer: a connection, a
    /// key exchange ([`HANDSHAKE_TIMEOUT`]).
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
    /// Each timeout no longer than its default.
    #[must_use]
    pub fn clamped(self) -> Tuning {
        let d = Tuning::default();
        Tuning {
            handshake: self.handshake.min(d.handshake),
            idle: self.idle.min(d.idle),
            peer_wait: self.peer_wait.min(d.peer_wait),
            answer_wait: self.answer_wait.min(d.answer_wait),
        }
    }
}

/// Receives before their offer is answered, at most
/// [`MAX_CONNECTING_RECEIVES`].
#[derive(Default)]
pub(crate) struct Connecting(AtomicUsize);

impl Connecting {
    /// A slot for one receive before its answer, if one is free.
    pub(crate) fn slot(&self) -> Option<Slot<'_>> {
        crate::slots::take(&self.0, MAX_CONNECTING_RECEIVES).then_some(Slot(&self.0))
    }
}

/// Gives its slot back when dropped.
pub(crate) struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        crate::slots::give_back(self.0);
    }
}

/// What each adapter holds: the engine's context, its timeouts, and its
/// receives waiting for their answer.
pub(crate) struct Inner {
    pub(crate) ctx: Arc<Ctx>,
    pub(crate) tuning: Tuning,
    /// Receives before their offer is answered.
    pub(crate) connecting: Connecting,
}

/// One protocol's sending and receiving by code.
pub(crate) trait ByCode: Send + Sync + 'static {
    /// Which protocol.
    const PROTOCOL: Protocol;

    /// Starts a send; the code follows as an event.
    fn send(
        inner: &Arc<Inner>,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> Result<TransferId, ErrorInfo>;

    /// Receives with a code, up to the offer's answer.
    fn receive(
        inner: Arc<Inner>,
        request: CodeReceive,
    ) -> BoxFuture<'static, Result<TransferId, ErrorInfo>>;
}

/// `P`'s adapter, its timeouts clamped to the spec's ([`Tuning::clamped`]).
pub(crate) fn adapter<P: ByCode>(ctx: Arc<Ctx>, tuning: Tuning) -> Arc<dyn Adapter> {
    Arc::new(CodeAdapter::<P> {
        inner: Arc::new(Inner {
            ctx,
            tuning: tuning.clamped(),
            connecting: Connecting::default(),
        }),
        protocol: PhantomData,
    })
}

/// The adapter of a protocol that works by code.
struct CodeAdapter<P> {
    inner: Arc<Inner>,
    protocol: PhantomData<fn() -> P>,
}

impl<P: ByCode> Adapter for CodeAdapter<P> {
    fn protocol(&self) -> Protocol {
        P::PROTOCOL
    }

    /// Receiving is by code only (F-MW2, F-CR2): nothing listens.
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
        Box::pin(async move { P::send(&self.inner, target, items) })
    }

    fn receive_code(&self, request: CodeReceive) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        P::receive(self.inner.clone(), request)
    }
}

/// What the user's answer, or the lack of one, means to the receive.
pub(crate) fn declined_error(d: &Declined) -> ErrorInfo {
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

/// A file to send that cannot be read.
pub(crate) fn bad_file() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::BadFile, "the file cannot be read")
}

/// A QR code of `data`, as the code events carry it: rows of `0` and `1`,
/// dark modules `1` (F-MW1, F-CR1).
///
/// # Errors
///
/// [`ErrorCode::Internal`] if `data` does not fit a QR code, which a code
/// of ours never fails to.
pub(crate) fn qr_code(data: &[u8]) -> Result<QrCode, ErrorInfo> {
    let failed = || ErrorInfo::new(ErrorCode::Internal, "the QR code could not be made");
    let q = qrcode::QrCode::new(data).map_err(|_| failed())?;
    let width = q.width();
    if width == 0 {
        return Err(failed());
    }
    let rows: Vec<String> = q
        .to_colors()
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|c| if *c == qrcode::Color::Dark { '1' } else { '0' })
                .collect()
        })
        .collect();
    Ok(QrCode {
        size: u32::try_from(width).map_err(|_| failed())?,
        rows,
    })
}

/// Opens the file to send, once, and checks the handle rather than the
/// path: a regular file of exactly the size the hub measured. The open is
/// read-only and non-blocking, so a FIFO put in the file's place after the
/// hub looked cannot hang it (and is then refused by the check). Nothing
/// after this looks at the path again.
#[allow(clippy::disallowed_methods)] // S3 bans opening for writing; this is O_RDONLY.
pub(crate) async fn open_checked(
    file: &OutgoingFile,
    limit: Duration,
) -> Result<std::fs::File, ErrorInfo> {
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
    tokio::time::timeout(limit, opening)
        .await
        .map_err(|_| bad_file())?
        .map_err(|_| bad_file())?
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

    #[test]
    fn slots_are_counted_and_given_back() {
        let c = Connecting::default();
        let held: Vec<_> = (0..MAX_CONNECTING_RECEIVES)
            .map(|_| c.slot().unwrap())
            .collect();
        assert!(c.slot().is_none(), "no fifth");
        drop(held);
        assert!(c.slot().is_some(), "given back on drop");
    }
}
