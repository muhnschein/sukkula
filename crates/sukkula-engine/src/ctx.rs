//! What every protocol adapter shares: the consent queue, the inbox, the
//! transfer registry, the reach policy, and the way out to the UI.
//!
//! The receive path is the same for every protocol, and it lives here so
//! that it is written once:
//!
//! 1. the adapter checks the peer's address with [`Ctx::permits`] and
//!    [`Ctx::allow_offer`] before parsing anything it sent;
//! 2. it builds a [`RawOffer`] from the wire and calls [`Ctx::offer`] (or,
//!    for an offer the user asked for, [`Ctx::offer_requested`]), which
//!    validates it (S1, S2, S4, S6), asks the user (S5), checks free space
//!    and registers a transfer;
//! 3. for each file it calls [`Ctx::begin_file`] and feeds the
//!    [`ReceivingFile`] chunks, which enforces the declared size, the
//!    digest, cancellation and progress (S3);
//! 4. it finishes the [`TransferHandle`].
//!
//! Nothing before step 2 returns may write a byte of payload anywhere.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use sukkula_core::Protocol;
use sukkula_core::config::Settings;
use sukkula_core::consent::{ConsentBroker, OfferId, Refusal};
use sukkula_core::inbox::{Inbox, InboxError, Incoming, Saved};
use sukkula_core::limits::{
    DISCOVERY_BURST, DISCOVERY_WINDOW, GLOBAL_OFFER_BURST, GLOBAL_OFFER_WINDOW,
    MAX_ACTIVE_TRANSFERS, NETWORK_IDLE_TIMEOUT, OFFER_BURST, OFFER_WINDOW,
};
use sukkula_core::offer::{Offer, OfferError, OfferFile, RawOffer};
use sukkula_core::reach::{RateLimiter, ReachPolicy};
use sukkula_core::store::Store;
use tokio_util::sync::CancellationToken;

use crate::api::{
    Direction, ErrorCode, ErrorInfo, Event, FileView, MAX_LISTED_FILES, Outcome, TransferId,
    TransferView,
};

/// Where events go. Called from engine threads; must not block for long.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// How often a transfer reports progress, at most.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// The shared state. One per engine.
pub struct Ctx {
    settings: RwLock<Settings>,
    device_model: String,
    store: Store,
    inbox: Inbox,
    consent: ConsentBroker,
    reach: ReachPolicy,
    transfers: Transfers,
    offer_limiter: Mutex<RateLimiter>,
    global_offer_limiter: Mutex<RateLimiter>,
    discovery_limiter: Mutex<RateLimiter>,
    events: EventSink,
    shutdown: CancellationToken,
}

impl Ctx {
    /// Builds the context. `consent` must already report to `events`.
    #[must_use]
    pub fn new(
        settings: Settings,
        device_model: String,
        store: Store,
        inbox: Inbox,
        consent: ConsentBroker,
        reach: ReachPolicy,
        events: EventSink,
    ) -> Self {
        Ctx {
            settings: RwLock::new(settings),
            device_model,
            store,
            inbox,
            consent,
            reach,
            transfers: Transfers::default(),
            offer_limiter: Mutex::new(RateLimiter::new(OFFER_BURST, OFFER_WINDOW)),
            global_offer_limiter: Mutex::new(RateLimiter::new(
                GLOBAL_OFFER_BURST,
                GLOBAL_OFFER_WINDOW,
            )),
            discovery_limiter: Mutex::new(RateLimiter::new(DISCOVERY_BURST, DISCOVERY_WINDOW)),
            events,
            shutdown: CancellationToken::new(),
        }
    }

    /// Sends an event to the UI.
    pub fn emit(&self, event: Event) {
        (self.events)(event);
    }

    /// The current settings.
    #[must_use]
    pub fn settings(&self) -> Settings {
        self.settings
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replaces the settings. The caller has validated and saved them.
    pub fn set_settings(&self, settings: Settings) {
        *self
            .settings
            .write()
            .unwrap_or_else(PoisonError::into_inner) = settings;
    }

    /// The name peers see (F-C7).
    #[must_use]
    pub fn device_name(&self) -> String {
        self.settings().effective_device_name(&self.device_model)
    }

    /// The device model, after S2.
    #[must_use]
    pub fn device_model(&self) -> &str {
        &self.device_model
    }

    /// The app's private files (settings, TLS key).
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The inbox.
    #[must_use]
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// The consent queue.
    #[must_use]
    pub fn consent(&self) -> &ConsentBroker {
        &self.consent
    }

    /// The reach policy.
    #[must_use]
    pub fn reach(&self) -> ReachPolicy {
        self.reach
    }

    /// Whether a LAN peer at `ip` may be answered at all (S7).
    #[must_use]
    pub fn permits(&self, ip: IpAddr) -> bool {
        self.reach.permits(ip)
    }

    /// Whether `ip` may put another offer in the consent queue now. Asked
    /// before anything the peer sent is parsed; [`Ctx::offer`] adds a limit
    /// on all peers together once an offer has proved well-formed.
    #[must_use]
    pub fn allow_offer(&self, ip: IpAddr) -> bool {
        self.permits(ip) && lock(&self.offer_limiter).allow(ip, Instant::now())
    }

    /// Whether `ip` may get another discovery reply now (S7).
    #[must_use]
    pub fn allow_discovery(&self, ip: IpAddr) -> bool {
        self.permits(ip) && lock(&self.discovery_limiter).allow(ip, Instant::now())
    }

    /// Cancelled when the engine stops. Every task an adapter spawns selects
    /// on it.
    #[must_use]
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.shutdown
    }

    /// The transfer registry.
    #[must_use]
    pub fn transfers(&self) -> &Transfers {
        &self.transfers
    }

    /// Validates `raw`, asks the user, checks the space, and registers an
    /// incoming transfer. Steps 2 of the module docs. For an offer nobody
    /// asked for: one from the LAN.
    ///
    /// # Errors
    ///
    /// Why the offer is not going ahead; the adapter declines it at the
    /// protocol level. The UI has already been told whatever it needs to be.
    pub async fn offer(self: &Arc<Self>, raw: RawOffer) -> Result<Accepted, Declined> {
        self.offer_as(raw, Asked::No).await
    }

    /// As [`offer`](Self::offer), for an offer the user asked for: a
    /// wormhole receive exists only because the user typed its code. It is
    /// outside the limit on offers from all peers together, which is there
    /// for offers nobody asked for, so that a LAN flood cannot make a code
    /// the user typed fail as busy and waste it ("Global offer budget is
    /// shared with user-initiated wormhole receives"). Every other rule and
    /// limit applies as to any offer.
    ///
    /// # Errors
    ///
    /// As [`offer`](Self::offer).
    // CONTRACT: new (additive); the wormhole adapter's receive calls it.
    pub async fn offer_requested(self: &Arc<Self>, raw: RawOffer) -> Result<Accepted, Declined> {
        self.offer_as(raw, Asked::Yes).await
    }

    async fn offer_as(self: &Arc<Self>, raw: RawOffer, asked: Asked) -> Result<Accepted, Declined> {
        let protocol = raw.protocol;
        let result = self.decide(raw, asked).await;
        // S9: counts, sizes and reasons; never a name, the text, the
        // sender or the PIN.
        match &result {
            Ok(a) => tracing::debug!(
                ?protocol,
                offer = a.offer_id,
                transfer = a.transfer.id(),
                files = a.offer.files.len(),
                bytes = a.offer.total_bytes,
                text = a.offer.text.is_some(),
                "offer accepted"
            ),
            Err(why) => tracing::debug!(?protocol, ?why, "offer not going ahead"),
        }
        result
    }

    async fn decide(self: &Arc<Self>, raw: RawOffer, asked: Asked) -> Result<Accepted, Declined> {
        let offer = Offer::validate(raw).map_err(Declined::Invalid)?;
        // Not worth asking the user about an offer that cannot run: they
        // would see it accepted and then nothing happen. (Checked again
        // below; a slot may go while the user thinks.) Free space is
        // checked only after consent, on purpose: checked before, it would
        // let any peer on the LAN measure the phone's free space by
        // bisection without the user ever seeing an offer.
        // CONTRACT: same signature; Busy can now come before consent.
        if self.transfers.active() >= MAX_ACTIVE_TRANSFERS {
            return Err(Declined::Busy);
        }
        // The per-IP limit is no limit against a peer that rotates its
        // source address, which on IPv6 costs nothing: at most
        // GLOBAL_OFFER_BURST well-formed offers a minute reach the user from
        // everyone together. Malformed ones never get this far, so they
        // cannot use up the shared allowance, and offers the user asked for
        // neither use it nor wait for it.
        if asked == Asked::No
            && !lock(&self.global_offer_limiter)
                .allow(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), Instant::now())
        {
            return Err(Declined::Busy);
        }
        let id = self.consent.ask(&offer).await.map_err(Declined::Refused)?;
        if self.inbox.check_space(offer.total_bytes).is_err() {
            return Err(Declined::NoSpace);
        }
        let handle = self
            .transfers
            .begin(
                self,
                Direction::Incoming,
                offer.protocol,
                &offer.sender,
                &offer.files,
            )
            .ok_or(Declined::Busy)?;
        Ok(Accepted {
            offer_id: id,
            offer,
            transfer: handle,
        })
    }

    /// Starts writing one accepted file. Step 3 of the module docs.
    ///
    /// # Errors
    ///
    /// The staging file could not be created.
    pub async fn begin_file<'a>(
        &self,
        transfer: &'a TransferHandle,
        file: &OfferFile,
    ) -> Result<ReceivingFile<'a>, ErrorInfo> {
        let incoming = self
            .inbox
            .begin(&file.name, file.size, file.sha256)
            .await
            .map_err(storage_error)?;
        Ok(ReceivingFile { incoming, transfer })
    }

    /// Declines everything and cancels every transfer. The engine calls this
    /// once, when it stops.
    pub fn shut_down(&self) {
        self.shutdown.cancel();
        self.consent.shutdown();
        self.transfers.cancel_all();
    }
}

/// Whether the user asked for an offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// A wormhole receive: the user typed the code.
    Yes,
    /// Anything from the LAN.
    No,
}

/// An offer the user accepted.
#[derive(Debug)]
pub struct Accepted {
    /// The consent id it was accepted under.
    pub offer_id: OfferId,
    /// What was accepted.
    pub offer: Offer,
    /// The transfer it runs as.
    pub transfer: TransferHandle,
}

/// Why an offer is not going ahead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declined {
    /// It broke a rule before anyone saw it.
    Invalid(OfferError),
    /// The user said no, did not answer, the queue was full, or the engine
    /// is stopping.
    Refused(Refusal),
    /// Not enough free space for it.
    NoSpace,
    /// Too many transfers running.
    Busy,
}

/// One file being received: the inbox's [`Incoming`] plus cancellation and
/// progress.
#[derive(Debug)]
pub struct ReceivingFile<'a> {
    incoming: Incoming,
    transfer: &'a TransferHandle,
}

impl ReceivingFile<'_> {
    /// Appends `chunk`.
    ///
    /// # Errors
    ///
    /// Cancelled, over the declared size, or an I/O error. Drop the
    /// `ReceivingFile` and the partial file is gone.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), ErrorInfo> {
        if self.transfer.is_cancelled() {
            return Err(cancelled());
        }
        self.incoming.write(chunk).await.map_err(storage_error)?;
        // An empty chunk moves nothing and reports nothing (see
        // `TransferHandle::add_progress`).
        self.transfer
            .add_progress(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Bytes written so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.incoming.written()
    }

    /// Bytes still expected.
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.incoming
            .declared()
            .saturating_sub(self.incoming.written())
    }

    /// Checks size and digest and places the file.
    ///
    /// # Errors
    ///
    /// As [`Incoming::commit`].
    pub async fn commit(self) -> Result<Saved, ErrorInfo> {
        if self.transfer.is_cancelled() {
            return Err(cancelled());
        }
        self.incoming.commit().await.map_err(storage_error)
    }
}

/// The registry of running transfers.
#[derive(Default)]
pub struct Transfers {
    inner: Mutex<TransfersInner>,
}

#[derive(Default)]
struct TransfersInner {
    next: TransferId,
    active: HashMap<TransferId, CancellationToken>,
}

impl Transfers {
    /// Registers a transfer and tells the UI. `None` when
    /// [`MAX_ACTIVE_TRANSFERS`] are already running.
    pub fn begin(
        &self,
        ctx: &Arc<Ctx>,
        direction: Direction,
        protocol: Protocol,
        peer: &str,
        files: &[OfferFile],
    ) -> Option<TransferHandle> {
        let views: Vec<FileView> = files
            .iter()
            .map(|f| FileView {
                name: f.name.as_str().to_owned(),
                size: f.size,
            })
            .collect();
        let total = files.iter().fold(0u64, |acc, f| acc.saturating_add(f.size));
        self.begin_views(ctx, direction, protocol, peer, views, total)
    }

    /// As [`begin`](Self::begin), for sends, whose files are local.
    pub fn begin_views(
        &self,
        ctx: &Arc<Ctx>,
        direction: Direction,
        protocol: Protocol,
        peer: &str,
        mut files: Vec<FileView>,
        total: u64,
    ) -> Option<TransferHandle> {
        let token = ctx.shutdown.child_token();
        let id = {
            let mut inner = lock(&self.inner);
            if inner.active.len() >= MAX_ACTIVE_TRANSFERS {
                return None;
            }
            inner.next = inner.next.checked_add(1).unwrap_or(1);
            let id = inner.next;
            inner.active.insert(id, token.clone());
            id
        };
        let file_count = files.len();
        tracing::debug!(
            transfer = id,
            ?direction,
            ?protocol,
            files = file_count,
            bytes = total,
            "transfer started"
        );
        files.truncate(MAX_LISTED_FILES);
        ctx.emit(Event::TransferStarted {
            transfer: TransferView {
                id,
                direction,
                protocol,
                peer: peer.to_owned(),
                files,
                file_count,
                total_bytes: total,
            },
        });
        Some(TransferHandle {
            id,
            total,
            bytes: AtomicU64::new(0),
            last_report: Mutex::new(Report::default()),
            token,
            ctx: Arc::downgrade(ctx),
            finished: false,
        })
    }

    /// Cancels one transfer. False if it is not running.
    pub fn cancel(&self, id: TransferId) -> bool {
        match lock(&self.inner).active.get(&id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// How many are running.
    #[must_use]
    pub fn active(&self) -> usize {
        lock(&self.inner).active.len()
    }

    fn cancel_all(&self) {
        for token in lock(&self.inner).active.values() {
            token.cancel();
        }
    }

    fn remove(&self, id: TransferId) {
        lock(&self.inner).active.remove(&id);
    }
}

/// One running transfer. Finish it with [`finish`](Self::finish); dropping
/// it unfinished reports it as failed, so the UI never shows a transfer
/// that will not end.
#[derive(Debug)]
pub struct TransferHandle {
    id: TransferId,
    total: u64,
    bytes: AtomicU64,
    last_report: Mutex<Report>,
    token: CancellationToken,
    ctx: std::sync::Weak<Ctx>,
    finished: bool,
}

impl TransferHandle {
    /// The id the UI knows it by.
    #[must_use]
    pub fn id(&self) -> TransferId {
        self.id
    }

    /// Cancelled by the user, by the peer, or by the engine stopping.
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Whether it has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Counts `n` more bytes and tells the UI, at most every 250 ms, and
    /// once more when the count reaches the total. Each value is reported
    /// once, and `n == 0` reports nothing: the report on reaching the total
    /// used to skip the throttle every time, so once a transfer was
    /// complete, or for a file of zero bytes, a peer sending empty chunks as
    /// fast as the LAN allows got one event per chunk ("Progress throttle is
    /// skipped once bytes >= total").
    pub fn add_progress(&self, n: u64) {
        if n == 0 {
            return;
        }
        let bytes = self.bytes.fetch_add(n, Ordering::Relaxed).saturating_add(n);
        let shown = bytes.min(self.total);
        let now = Instant::now();
        let due = {
            let mut last = lock(&self.last_report);
            let due = last.shown != Some(shown)
                && (shown >= self.total
                    || last
                        .at
                        .is_none_or(|t| now.saturating_duration_since(t) >= PROGRESS_INTERVAL));
            if due {
                *last = Report {
                    at: Some(now),
                    shown: Some(shown),
                };
            }
            due
        };
        if due && let Some(ctx) = self.ctx.upgrade() {
            ctx.emit(Event::TransferProgress {
                transfer: self.id,
                bytes: shown,
                total: self.total,
            });
        }
    }

    /// Ends the transfer and tells the UI.
    pub fn finish(mut self, outcome: Outcome, saved: Vec<String>) {
        self.finished = true;
        self.report(outcome, saved);
    }

    /// Ends it as [`Outcome::Done`], or as cancelled/failed from `result`.
    pub fn finish_with(self, result: Result<Vec<String>, ErrorInfo>) {
        match result {
            Ok(saved) => self.finish(Outcome::Done, saved),
            Err(e) if e.code == ErrorCode::Refused && self.is_cancelled() => {
                self.finish(Outcome::Cancelled, Vec::new())
            }
            Err(e) => {
                let outcome = if self.is_cancelled() {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed { error: e }
                };
                self.finish(outcome, Vec::new());
            }
        }
    }

    fn report(&self, outcome: Outcome, saved: Vec<String>) {
        // S9: the outcome's code, never its message (a storage error's
        // names a directory) or the saved names.
        let (result, code) = match &outcome {
            Outcome::Done => ("done", None),
            Outcome::Cancelled => ("cancelled", None),
            Outcome::Failed { error } => ("failed", Some(error.code)),
        };
        tracing::debug!(
            transfer = self.id,
            result,
            ?code,
            bytes = self.bytes.load(Ordering::Relaxed),
            saved = saved.len(),
            "transfer finished"
        );
        if let Some(ctx) = self.ctx.upgrade() {
            ctx.transfers.remove(self.id);
            ctx.emit(Event::TransferFinished {
                transfer: self.id,
                outcome,
                saved,
            });
        }
    }
}

/// The last progress report of a transfer.
#[derive(Debug, Default)]
struct Report {
    /// When.
    at: Option<Instant>,
    /// The count it showed.
    shown: Option<u64>,
}

impl Drop for TransferHandle {
    fn drop(&mut self) {
        if !self.finished {
            self.token.cancel();
            let outcome = Outcome::Failed {
                error: ErrorInfo::new(ErrorCode::Internal, "transfer abandoned"),
            };
            self.report(outcome, Vec::new());
        }
    }
}

/// The error a cancelled transfer reports.
#[must_use]
pub fn cancelled() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Refused, "cancelled")
}

/// Maps an inbox failure to what the UI shows.
#[must_use]
pub fn storage_error(e: InboxError) -> ErrorInfo {
    match e {
        InboxError::TooLarge | InboxError::Overflow => {
            ErrorInfo::new(ErrorCode::TooLarge, e.to_string())
        }
        InboxError::Truncated { .. } | InboxError::DigestMismatch => {
            ErrorInfo::new(ErrorCode::Network, e.to_string())
        }
        InboxError::NoSpace
        | InboxError::NoFreeName
        | InboxError::NotADirectory(_)
        | InboxError::Io(_) => ErrorInfo::new(ErrorCode::Storage, e.to_string()),
    }
}

/// Runs `fut`, failing with [`ErrorCode::Network`] if it takes longer than
/// [`NETWORK_IDLE_TIMEOUT`] (S6). Wrap every network read in this, or in a
/// shorter timeout.
///
/// # Errors
///
/// The timeout.
pub async fn idle_timeout<T>(fut: impl std::future::Future<Output = T>) -> Result<T, ErrorInfo> {
    tokio::time::timeout(NETWORK_IDLE_TIMEOUT, fut)
        .await
        .map_err(|_| ErrorInfo::new(ErrorCode::Network, "the peer stopped responding"))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sukkula_core::consent::ConsentEvent;
    use sukkula_core::offer::RawFile;

    fn ctx(dir: &std::path::Path) -> (Arc<Ctx>, Arc<Mutex<Vec<ConsentEvent>>>) {
        let (ctx, seen, _) = ctx_with_events(dir);
        (ctx, seen)
    }

    type Events = Arc<Mutex<Vec<Event>>>;

    fn ctx_with_events(dir: &std::path::Path) -> (Arc<Ctx>, Arc<Mutex<Vec<ConsentEvent>>>, Events) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let consent = ConsentBroker::new(Arc::new(move |e| s.lock().unwrap().push(e)));
        let events: Events = Arc::default();
        let e = events.clone();
        let ctx = Ctx::new(
            Settings::default(),
            "Test Phone".into(),
            Store::open(&dir.join("data")).unwrap(),
            Inbox::open(&dir.join("dl")).unwrap(),
            consent,
            ReachPolicy {
                allow_loopback: true,
            },
            Arc::new(move |event| e.lock().unwrap().push(event)),
        );
        (Arc::new(ctx), seen, events)
    }

    fn progress_reports(events: &Events) -> Vec<u64> {
        events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::TransferProgress { bytes, .. } => Some(*bytes),
                _ => None,
            })
            .collect()
    }

    fn one_file(protocol: Protocol, size: u64) -> RawOffer {
        let mut raw = RawOffer::new(protocol, "Eve");
        raw.files.push(RawFile {
            name: "a".into(),
            size: size.into(),
            ..RawFile::default()
        });
        raw
    }

    /// Answers the first offer shown after the first `before` consent
    /// events.
    async fn answer_next(
        ctx: &Arc<Ctx>,
        seen: &Arc<Mutex<Vec<ConsentEvent>>>,
        before: usize,
        accept: bool,
    ) {
        loop {
            let id = seen.lock().unwrap()[before..].iter().find_map(|e| match e {
                ConsentEvent::Pending { id, .. } => Some(*id),
                ConsentEvent::Closed { .. } => None,
            });
            if let Some(id) = id {
                let decision = if accept {
                    sukkula_core::consent::Decision::Accept
                } else {
                    sukkula_core::consent::Decision::Decline
                };
                assert!(ctx.consent().answer(id, decision));
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// A peer that sends empty chunks after the last byte (or for a file of
    /// zero bytes) gets nothing onto the event path: each value is reported
    /// once, and an empty chunk reports nothing.
    #[tokio::test]
    async fn empty_chunks_cost_nothing_and_each_value_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, seen, events) = ctx_with_events(dir.path());
        for size in [3u64, 0] {
            events.lock().unwrap().clear();
            let before = seen.lock().unwrap().len();
            let (accepted, ()) = tokio::join!(
                ctx.offer(one_file(Protocol::QuickShare, size)),
                answer_next(&ctx, &seen, before, true)
            );
            let Accepted {
                offer, transfer, ..
            } = accepted.unwrap();
            let mut file = ctx.begin_file(&transfer, &offer.files[0]).await.unwrap();
            if size > 0 {
                file.write(b"abc").await.unwrap();
            }
            for _ in 0..10_000 {
                file.write(b"").await.unwrap();
            }
            let expected: Vec<u64> = if size > 0 { vec![3] } else { vec![] };
            assert_eq!(progress_reports(&events), expected, "size {size}");
            file.commit().await.unwrap();
            transfer.finish(Outcome::Done, Vec::new());
        }
        // Past the total (no adapter does this; the handle holds anyway).
        let t = ctx
            .transfers()
            .begin_views(
                &ctx,
                Direction::Outgoing,
                Protocol::LocalSend,
                "p",
                vec![],
                5,
            )
            .unwrap();
        events.lock().unwrap().clear();
        for _ in 0..1000 {
            t.add_progress(5);
            t.add_progress(0);
        }
        assert_eq!(progress_reports(&events), vec![5]);
        t.finish(Outcome::Done, Vec::new());
    }

    #[test]
    fn progress_is_throttled_below_the_total_and_reaches_it() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _, events) = ctx_with_events(dir.path());
        let t = ctx
            .transfers()
            .begin_views(
                &ctx,
                Direction::Outgoing,
                Protocol::LocalSend,
                "p",
                vec![],
                10_000,
            )
            .unwrap();
        let started = Instant::now();
        for _ in 0..10_000 {
            t.add_progress(1);
        }
        let reports = progress_reports(&events);
        // The first at once, then at most one per 250 ms, then the total.
        let allowed = 2 + started.elapsed().as_millis() / 250;
        assert!(
            u128::try_from(reports.len()).unwrap() <= allowed,
            "{reports:?}"
        );
        assert_eq!(reports.first(), Some(&1));
        assert_eq!(reports.last(), Some(&10_000));
        assert!(reports.windows(2).all(|w| w[0] < w[1]), "{reports:?}");
        t.finish(Outcome::Done, Vec::new());
    }

    /// The limit on offers from all peers together is for offers nobody
    /// asked for. A code the user typed neither spends it nor is refused by
    /// it: LAN offers that used it up cannot make a wormhole receive fail as
    /// busy.
    #[tokio::test(start_paused = true)]
    async fn offers_the_user_asked_for_are_outside_the_global_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, seen) = ctx(dir.path());
        let pending = || {
            seen.lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e, ConsentEvent::Pending { .. }))
                .count()
        };
        // Asked-for offers first, more than the allowance: all reach the
        // user (nobody answers; each times out) and spend nothing.
        for _ in 0..(GLOBAL_OFFER_BURST + 5) {
            assert_eq!(
                ctx.offer_requested(one_file(Protocol::Wormhole, 1))
                    .await
                    .unwrap_err(),
                Declined::Refused(Refusal::TimedOut)
            );
        }
        assert_eq!(pending(), usize::try_from(GLOBAL_OFFER_BURST + 5).unwrap());
        // The LAN still has its whole allowance...
        for _ in 0..GLOBAL_OFFER_BURST {
            assert_eq!(
                ctx.offer(one_file(Protocol::LocalSend, 1))
                    .await
                    .unwrap_err(),
                Declined::Refused(Refusal::TimedOut)
            );
        }
        // ...and then none.
        assert_eq!(
            ctx.offer(one_file(Protocol::LocalSend, 1))
                .await
                .unwrap_err(),
            Declined::Busy
        );
        // A code typed now still reaches the user, and is accepted.
        let before = seen.lock().unwrap().len();
        let (accepted, ()) = tokio::join!(
            ctx.offer_requested(one_file(Protocol::Wormhole, 1)),
            answer_next(&ctx, &seen, before, true)
        );
        accepted
            .unwrap()
            .transfer
            .finish(Outcome::Cancelled, Vec::new());
    }

    #[tokio::test]
    async fn a_busy_engine_declines_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, seen) = ctx(dir.path());
        let running: Vec<TransferHandle> = (0..MAX_ACTIVE_TRANSFERS)
            .map(|_| {
                ctx.transfers()
                    .begin_views(
                        &ctx,
                        Direction::Outgoing,
                        Protocol::LocalSend,
                        "p",
                        vec![],
                        0,
                    )
                    .unwrap()
            })
            .collect();
        let mut raw = RawOffer::new(Protocol::LocalSend, "Alice");
        raw.files.push(RawFile {
            name: "a.txt".into(),
            size: 3,
            ..RawFile::default()
        });
        assert_eq!(ctx.offer(raw).await.unwrap_err(), Declined::Busy);
        assert!(seen.lock().unwrap().is_empty(), "the user was asked");
        for t in running {
            t.finish(Outcome::Cancelled, Vec::new());
        }
        assert_eq!(ctx.transfers().active(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn offers_are_limited_per_peer_and_in_total() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, seen) = ctx(dir.path());
        let peer = |i: u16| IpAddr::V6(std::net::Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, i));
        // One peer gets its own allowance and no more.
        let own = (0..20).filter(|_| ctx.allow_offer(peer(1))).count();
        assert_eq!(own, usize::try_from(OFFER_BURST).unwrap());
        assert!(
            !ctx.allow_offer("8.8.8.8".parse().unwrap()),
            "a public address"
        );
        // Malformed offers do not touch the shared allowance...
        for _ in 0..50 {
            let raw = RawOffer::new(Protocol::LocalSend, "Eve");
            assert!(matches!(ctx.offer(raw).await, Err(Declined::Invalid(_))));
        }
        // ...well-formed ones do, whichever address they came from.
        let mut asked = 0;
        for _ in 0..(GLOBAL_OFFER_BURST + 5) {
            let mut raw = RawOffer::new(Protocol::LocalSend, "Eve");
            raw.files.push(RawFile {
                name: "a".into(),
                size: 1,
                ..RawFile::default()
            });
            // Nobody answers: each times out, freeing its consent slot.
            if !matches!(ctx.offer(raw).await, Err(Declined::Busy)) {
                asked += 1;
            }
        }
        assert_eq!(asked, GLOBAL_OFFER_BURST);
        let pending = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, ConsentEvent::Pending { .. }))
            .count();
        assert_eq!(pending, usize::try_from(GLOBAL_OFFER_BURST).unwrap());
    }
}
