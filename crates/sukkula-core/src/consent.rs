//! S5 and F-C2/F-C3: nothing is received until the user says so.
//!
//! An adapter hands a validated [`Offer`] to [`ConsentBroker::ask`] and waits.
//! The broker shows it to the user through the observer, and the answer comes
//! back from [`ConsentBroker::answer`]. There is no auto-accept, for anyone.
//!
//! - At most `max_pending` offers wait at once. The next is refused as
//!   [`Refusal::Busy`] without the user ever seeing it.
//! - An offer nobody answers is declined after `timeout`.
//! - An adapter that gives up waiting -- the peer hung up, the transfer was
//!   cancelled -- drops the future, and the offer disappears from the UI.
//!
//! Every offer leaves the queue exactly once, and the one who takes it out
//! decides how it ends: [`ConsentBroker::answer`] takes it out and hands
//! over the decision under the same lock, and a timeout, a withdrawal or a
//! shutdown takes it out under that lock too. So an answer that
//! [`ConsentBroker::answer`] reports as delivered is the answer the adapter
//! gets, even when it arrives in the same instant as the timeout, and one
//! that it reports as not delivered changed nothing.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::limits::{MAX_PENDING_OFFERS, OFFER_TIMEOUT};
use crate::offer::Offer;

/// Identifies one offer while it waits.
pub type OfferId = u64;

/// The user's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Receive it.
    Accept,
    /// Do not.
    Decline,
}

/// How a pending offer left the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Closed {
    /// The user accepted it.
    Accepted,
    /// The user declined it.
    Declined,
    /// Nobody answered in time.
    TimedOut,
    /// The sender went away, or the adapter stopped waiting.
    Withdrawn,
    /// The engine is stopping.
    Shutdown,
}

/// What the observer is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsentEvent {
    /// Show this offer to the user.
    Pending {
        /// Answer with this.
        id: OfferId,
        /// What to show.
        offer: Arc<Offer>,
    },
    /// Take it off the screen.
    Closed {
        /// The offer.
        id: OfferId,
        /// Why.
        reason: Closed,
    },
}

/// Why [`ConsentBroker::ask`] did not come back with a yes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The queue was full; the user was not asked.
    Busy,
    /// The user said no.
    Declined,
    /// The user did not answer in time.
    TimedOut,
    /// The engine is stopping.
    Shutdown,
}

/// The observer: shows offers to the user and takes them away again. Called
/// from whatever task asked, never with the broker's lock held.
pub type Observer = Arc<dyn Fn(ConsentEvent) + Send + Sync>;

/// The consent queue. Cheap to clone; clones share the queue.
#[derive(Clone)]
pub struct ConsentBroker {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    observer: Observer,
    timeout: Duration,
    max_pending: usize,
}

#[derive(Default)]
struct State {
    next_id: OfferId,
    pending: HashMap<OfferId, oneshot::Sender<Decision>>,
    shut_down: bool,
}

impl ConsentBroker {
    /// A broker with the spec's limits: [`OFFER_TIMEOUT`] and
    /// [`MAX_PENDING_OFFERS`].
    #[must_use]
    pub fn new(observer: Observer) -> Self {
        Self::with_limits(observer, OFFER_TIMEOUT, MAX_PENDING_OFFERS)
    }

    /// A broker with other limits, for tests.
    #[must_use]
    pub fn with_limits(observer: Observer, timeout: Duration, max_pending: usize) -> Self {
        ConsentBroker {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    next_id: 1,
                    ..State::default()
                }),
                observer,
                timeout,
                max_pending,
            }),
        }
    }

    /// Asks the user about `offer` and waits for the answer.
    ///
    /// # Errors
    ///
    /// Every outcome but an explicit yes, as a [`Refusal`]. Dropping the
    /// returned future withdraws the offer.
    pub async fn ask(&self, offer: &Offer) -> Result<OfferId, Refusal> {
        let (tx, mut rx) = oneshot::channel();
        let id = {
            let mut state = self.lock();
            if state.shut_down {
                return Err(Refusal::Shutdown);
            }
            if state.pending.len() >= self.inner.max_pending {
                return Err(Refusal::Busy);
            }
            let id = state.fresh_id();
            state.pending.insert(id, tx);
            id
        };
        let mut guard = Waiting {
            broker: self,
            id,
            settled: false,
        };
        self.notify(ConsentEvent::Pending {
            id,
            offer: Arc::new(offer.clone()),
        });

        let outcome = tokio::time::timeout(self.inner.timeout, &mut rx).await;
        guard.settled = true;
        let decision = match outcome {
            // An answer, or `None` when `shutdown` dropped the sender: that
            // is the only other way the sender goes while we wait.
            Ok(received) => received.ok(),
            Err(_elapsed) => {
                // The deadline and an answer can land together. Whoever takes
                // the entry out of the queue decides: if it is still there,
                // nobody answered in time; if `answer` took it, it sent its
                // decision before letting go of the lock, so the decision is
                // in the channel now.
                let taken = self.lock().pending.remove(&id);
                if taken.is_some() {
                    self.notify(ConsentEvent::Closed {
                        id,
                        reason: Closed::TimedOut,
                    });
                    return Err(Refusal::TimedOut);
                }
                rx.try_recv().ok()
            }
        };
        let (result, reason) = match decision {
            Some(Decision::Accept) => (Ok(id), Closed::Accepted),
            Some(Decision::Decline) => (Err(Refusal::Declined), Closed::Declined),
            None => (Err(Refusal::Shutdown), Closed::Shutdown),
        };
        self.notify(ConsentEvent::Closed { id, reason });
        result
    }

    /// Delivers the user's answer. True when the waiting adapter gets it;
    /// false when `id` is not waiting any more -- answered already, timed
    /// out, withdrawn, or never issued -- and then nothing changed.
    pub fn answer(&self, id: OfferId, decision: Decision) -> bool {
        let mut state = self.lock();
        match state.pending.remove(&id) {
            // Sent under the lock; see `ask` for why. A oneshot send only
            // stores the value and wakes the waiter, it never runs it.
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// How many offers are waiting.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.lock().pending.len()
    }

    /// Declines everything waiting and every later [`ask`](Self::ask).
    pub fn shutdown(&self) {
        let mut state = self.lock();
        state.shut_down = true;
        // Dropping the senders wakes every waiter with `Shutdown`.
        state.pending.clear();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Tells the observer, which is never called with the lock held. A
    /// panicking observer is contained here: the broker's bookkeeping is
    /// already done by then, and a panic escaping from the withdrawal in
    /// [`Waiting::drop`] while another panic unwinds would abort the app.
    fn notify(&self, event: ConsentEvent) {
        let observer = &self.inner.observer;
        if catch_unwind(AssertUnwindSafe(|| observer(event))).is_err() {
            tracing::error!("the consent observer panicked");
        }
    }
}

impl State {
    /// The next id not in use. Ids are `u64` and never wrap in practice; if
    /// they ever did, one still waiting is skipped, so an answer can never
    /// reach an offer it was not meant for. At most `pending.len() + 1`
    /// steps, and 0 is never issued.
    fn fresh_id(&mut self) -> OfferId {
        let mut id = self.next_id;
        while id == 0 || self.pending.contains_key(&id) {
            id = id.checked_add(1).unwrap_or(1);
        }
        self.next_id = id.checked_add(1).unwrap_or(1);
        id
    }
}

/// Withdraws the offer if the future asking about it is dropped.
struct Waiting<'a> {
    broker: &'a ConsentBroker,
    id: OfferId,
    settled: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.broker.lock().pending.remove(&self.id);
            self.broker.notify(ConsentEvent::Closed {
                id: self.id,
                reason: Closed::Withdrawn,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offer::{Protocol, RawFile, RawOffer};

    fn offer() -> Offer {
        let mut raw = RawOffer::new(Protocol::LocalSend, "Alice");
        raw.files.push(RawFile {
            name: "a.txt".into(),
            size: 3,
            ..RawFile::default()
        });
        Offer::validate(raw).unwrap()
    }

    fn recording() -> (Observer, Arc<Mutex<Vec<ConsentEvent>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        (Arc::new(move |e| l.lock().unwrap().push(e)), log)
    }

    #[tokio::test(start_paused = true)]
    async fn an_accepted_offer_returns_its_id() {
        let (obs, log) = recording();
        let broker = ConsentBroker::new(obs);
        let b = broker.clone();
        let asking = tokio::spawn(async move { b.ask(&offer()).await });
        tokio::task::yield_now().await;
        assert_eq!(broker.pending(), 1);
        assert!(broker.answer(1, Decision::Accept));
        assert_eq!(asking.await.unwrap(), Ok(1));
        assert!(!broker.answer(1, Decision::Accept), "answered twice");
        let log = log.lock().unwrap();
        assert!(matches!(log[0], ConsentEvent::Pending { id: 1, .. }));
        assert_eq!(
            log[1],
            ConsentEvent::Closed {
                id: 1,
                reason: Closed::Accepted
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unanswered_offers_time_out() {
        let (obs, log) = recording();
        let broker = ConsentBroker::new(obs);
        let started = tokio::time::Instant::now();
        assert_eq!(broker.ask(&offer()).await, Err(Refusal::TimedOut));
        assert!(started.elapsed() >= OFFER_TIMEOUT);
        assert_eq!(broker.pending(), 0);
        assert_eq!(
            log.lock().unwrap()[1],
            ConsentEvent::Closed {
                id: 1,
                reason: Closed::TimedOut
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_refuses_without_asking() {
        let (obs, log) = recording();
        let broker = ConsentBroker::new(obs);
        let mut waiting = Vec::new();
        for _ in 0..MAX_PENDING_OFFERS {
            let b = broker.clone();
            waiting.push(tokio::spawn(async move { b.ask(&offer()).await }));
        }
        tokio::task::yield_now().await;
        assert_eq!(broker.pending(), MAX_PENDING_OFFERS);
        assert_eq!(broker.ask(&offer()).await, Err(Refusal::Busy));
        assert_eq!(
            log.lock().unwrap().len(),
            MAX_PENDING_OFFERS,
            "the third offer was shown"
        );
        broker.shutdown();
        for w in waiting {
            assert_eq!(w.await.unwrap(), Err(Refusal::Shutdown));
        }
        assert_eq!(broker.ask(&offer()).await, Err(Refusal::Shutdown));
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_ask_withdraws_the_offer() {
        let (obs, log) = recording();
        let broker = ConsentBroker::new(obs);
        let b = broker.clone();
        let asking = tokio::spawn(async move { b.ask(&offer()).await });
        tokio::task::yield_now().await;
        asking.abort();
        let _ = asking.await;
        assert_eq!(broker.pending(), 0);
        assert!(!broker.answer(1, Decision::Accept));
        assert_eq!(
            log.lock().unwrap()[1],
            ConsentEvent::Closed {
                id: 1,
                reason: Closed::Withdrawn
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_decline_is_a_refusal() {
        let (obs, _) = recording();
        let broker = ConsentBroker::new(obs);
        let b = broker.clone();
        let asking = tokio::spawn(async move { b.ask(&offer()).await });
        tokio::task::yield_now().await;
        assert!(broker.answer(1, Decision::Decline));
        assert_eq!(asking.await.unwrap(), Err(Refusal::Declined));
        assert!(!broker.answer(99, Decision::Accept), "an id never issued");
    }

    /// The race between an answer and the deadline, run for real on two
    /// threads with answers landing on both sides of it. The rule it holds:
    /// `answer` returning true means the adapter got that answer. Before the
    /// answer was handed over under the lock, one landing between the
    /// deadline and the timeout's cleanup was reported as delivered while the
    /// offer ended as timed out. That window is nanoseconds wide, so this is
    /// a guard against the rule breaking wholesale rather than a reliable
    /// reproduction; the fix is by construction (see `ask`).
    ///
    /// Tokio's timer ticks once a millisecond, so a deadline shorter than a
    /// tick is really one to two ticks: the answers run from well before a
    /// 2 ms deadline to well after its latest tick, or a quiet machine
    /// delivers every one and the timed-out side is never reached.
    #[test]
    fn an_answer_reported_delivered_is_the_answer_the_adapter_gets() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let broker = ConsentBroker::with_limits(Arc::new(|_| {}), Duration::from_millis(2), 1);
            let mut delivered = 0;
            let mut missed = 0;
            for round in 0..400u64 {
                let b = broker.clone();
                let asking = tokio::spawn(async move { b.ask(&offer()).await });
                while broker.pending() == 0 && !asking.is_finished() {
                    tokio::task::yield_now().await;
                }
                let b = broker.clone();
                let id = round + 1;
                let answered = tokio::task::spawn_blocking(move || {
                    // From 0 to 4 ms, in 0.5 ms steps: each side of the
                    // deadline, and the two ticks it can fire on.
                    std::thread::sleep(Duration::from_micros((round % 9) * 500));
                    b.answer(id, Decision::Accept)
                })
                .await
                .unwrap();
                let outcome = asking.await.unwrap();
                if answered {
                    delivered += 1;
                    assert_eq!(outcome, Ok(id), "round {round}");
                } else {
                    missed += 1;
                    assert_eq!(outcome, Err(Refusal::TimedOut), "round {round}");
                }
                assert_eq!(broker.pending(), 0);
            }
            // Both sides of the race were reached.
            assert!(delivered > 0 && missed > 0, "{delivered} / {missed}");
        });
    }

    #[test]
    fn ids_are_never_reused_while_waiting() {
        let mut state = State {
            next_id: u64::MAX,
            ..State::default()
        };
        let (tx1, _rx1) = oneshot::channel();
        let (tx2, _rx2) = oneshot::channel();
        state.pending.insert(1, tx1);
        state.pending.insert(2, tx2);
        assert_eq!(state.fresh_id(), u64::MAX);
        // Wraps past 0, and past the two still waiting.
        assert_eq!(state.fresh_id(), 3);
        assert_eq!(state.fresh_id(), 4);
        let mut state = State::default();
        assert_eq!(state.fresh_id(), 1, "0 is never issued");
    }

    #[tokio::test(start_paused = true)]
    async fn a_panicking_observer_does_not_leak_a_slot() {
        let observer: Observer = Arc::new(|e| {
            if matches!(e, ConsentEvent::Pending { .. }) {
                panic!("observer bug");
            }
        });
        let broker = ConsentBroker::with_limits(observer, OFFER_TIMEOUT, 1);
        // Nobody saw it, so nobody answers: it times out, and the slot is
        // free again.
        assert_eq!(broker.ask(&offer()).await, Err(Refusal::TimedOut));
        assert_eq!(broker.pending(), 0);
        assert_eq!(broker.ask(&offer()).await, Err(Refusal::TimedOut));
    }

    /// An adapter task that panics while an offer waits drops the `ask`
    /// future during unwinding, and the withdrawal calls the observer then.
    /// An observer that panics too must not turn that into an abort; if it
    /// did, this test would take the whole test binary down.
    #[tokio::test(start_paused = true)]
    async fn a_withdrawal_during_a_panic_survives_a_panicking_observer() {
        let observer: Observer = Arc::new(|_| panic!("observer bug"));
        let broker = ConsentBroker::new(observer);
        let b = broker.clone();
        let task = tokio::spawn(async move {
            let o = offer();
            let ask = b.ask(&o);
            tokio::pin!(ask);
            std::future::poll_fn(|cx| {
                assert!(ask.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            assert_eq!(b.pending(), 1);
            panic!("adapter bug");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert_eq!(broker.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_asked_with_no_room_at_all() {
        let (obs, log) = recording();
        let broker = ConsentBroker::with_limits(obs, OFFER_TIMEOUT, 0);
        assert_eq!(broker.ask(&offer()).await, Err(Refusal::Busy));
        assert!(log.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn every_offer_closes_exactly_once() {
        let (obs, log) = recording();
        let broker = ConsentBroker::with_limits(obs, OFFER_TIMEOUT, 2);
        // One answered, one withdrawn, then one timed out.
        let b = broker.clone();
        let a = tokio::spawn(async move { b.ask(&offer()).await });
        let b = broker.clone();
        let w = tokio::spawn(async move { b.ask(&offer()).await });
        tokio::task::yield_now().await;
        assert!(broker.answer(1, Decision::Accept));
        w.abort();
        let _ = w.await;
        assert_eq!(a.await.unwrap(), Ok(1));
        assert_eq!(broker.ask(&offer()).await, Err(Refusal::TimedOut));
        let log = log.lock().unwrap();
        for id in 1..=3 {
            let pending = log
                .iter()
                .filter(|e| matches!(e, ConsentEvent::Pending { id: i, .. } if *i == id))
                .count();
            let closed = log
                .iter()
                .filter(|e| matches!(e, ConsentEvent::Closed { id: i, .. } if *i == id))
                .count();
            assert_eq!((pending, closed), (1, 1), "offer {id}");
        }
    }
}
