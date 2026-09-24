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

use std::collections::HashMap;
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
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut state = self.lock();
            if state.shut_down {
                return Err(Refusal::Shutdown);
            }
            if state.pending.len() >= self.inner.max_pending {
                return Err(Refusal::Busy);
            }
            let id = state.next_id;
            state.next_id = state.next_id.checked_add(1).unwrap_or(1);
            state.pending.insert(id, tx);
            id
        };
        let mut guard = Waiting {
            broker: self,
            id,
            settled: false,
        };
        (self.inner.observer)(ConsentEvent::Pending {
            id,
            offer: Arc::new(offer.clone()),
        });

        let outcome = tokio::time::timeout(self.inner.timeout, rx).await;
        guard.settled = true;
        let (result, reason) = match outcome {
            Ok(Ok(Decision::Accept)) => (Ok(id), Closed::Accepted),
            Ok(Ok(Decision::Decline)) => (Err(Refusal::Declined), Closed::Declined),
            // The sender half is dropped only by `shutdown`.
            Ok(Err(_)) => (Err(Refusal::Shutdown), Closed::Shutdown),
            Err(_) => {
                self.lock().pending.remove(&id);
                (Err(Refusal::TimedOut), Closed::TimedOut)
            }
        };
        (self.inner.observer)(ConsentEvent::Closed { id, reason });
        result
    }

    /// Delivers the user's answer. False when `id` is not waiting any more
    /// -- answered already, timed out, withdrawn, or never issued.
    pub fn answer(&self, id: OfferId, decision: Decision) -> bool {
        let tx = self.lock().pending.remove(&id);
        match tx {
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
            (self.broker.inner.observer)(ConsentEvent::Closed {
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
}
