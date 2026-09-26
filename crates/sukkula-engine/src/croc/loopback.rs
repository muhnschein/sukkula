//! Our sender and our receiver, through a croc relay of our own
//! (`relay::testing`): files, a text, declining, a wrong code, and a
//! transfer cancelled half-way. What croc's Go binary makes of us is
//! `tests/croc_interop.rs`'s to say.

#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)] // Test scenes.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sukkula_core::config::Settings;
use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision};
use sukkula_core::inbox::Inbox;
use sukkula_core::name;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use tokio::sync::mpsc;

use super::relay::testing::{self, TestRelay};
use super::{Tuning, adapter_with};
use crate::adapter::{Adapter, Outgoing, OutgoingFile};
use crate::api::{ErrorCode, ErrorInfo, Event, Outcome, SendTarget, TransferId};
use crate::ctx::Ctx;

type Events = Arc<Mutex<Vec<Event>>>;

/// One side: its context, adapter, events, and the offers it is asked.
struct Side {
    ctx: Arc<Ctx>,
    adapter: Arc<dyn Adapter>,
    events: Events,
    offers: mpsc::UnboundedReceiver<u64>,
    dir: tempfile::TempDir,
}

fn tuning() -> Tuning {
    Tuning {
        handshake: Duration::from_secs(10),
        idle: Duration::from_secs(10),
        peer_wait: Duration::from_secs(20),
        answer_wait: Duration::from_secs(20),
    }
}

fn side(relay: &TestRelay) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let (tx, offers) = mpsc::unbounded_channel();
    let consent = ConsentBroker::new(Arc::new(move |e| {
        if let ConsentEvent::Pending { id, .. } = e {
            let _ = tx.send(id);
        }
    }));
    let events: Events = Arc::default();
    let e = events.clone();
    let mut settings = Settings::default();
    settings.croc.relay = Some(format!("127.0.0.1:{}", relay.port));
    settings.croc.password = Some(relay.password.clone());
    let ctx = Arc::new(Ctx::new(
        settings,
        "Test Phone".into(),
        Store::open(&dir.path().join("data")).unwrap(),
        Inbox::open(&dir.path().join("dl")).unwrap(),
        consent,
        ReachPolicy {
            allow_loopback: true,
        },
        Arc::new(move |event| e.lock().unwrap().push(event)),
    ));
    let adapter = adapter_with(ctx.clone(), tuning());
    Side {
        ctx,
        adapter,
        events,
        offers,
        dir,
    }
}

impl Side {
    /// The code a send of ours shows, once it does.
    async fn code(&self, transfer: TransferId) -> String {
        loop {
            let found = self.events.lock().unwrap().iter().find_map(|e| match e {
                Event::CrocCode { transfer: t, code } if *t == transfer => Some(code.clone()),
                _ => None,
            });
            if let Some(c) = found {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// How transfer `id` ended, and the names it saved.
    async fn outcome(&self, id: TransferId) -> (Outcome, Vec<String>) {
        loop {
            let found = self.events.lock().unwrap().iter().find_map(|e| match e {
                Event::TransferFinished {
                    transfer,
                    outcome,
                    saved,
                } if *transfer == id => Some((outcome.clone(), saved.clone())),
                _ => None,
            });
            if let Some(o) = found {
                return o;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Answers the next offer.
    async fn answer(&mut self, accept: bool) {
        let id = self.offers.recv().await.unwrap();
        let d = if accept {
            Decision::Accept
        } else {
            Decision::Decline
        };
        assert!(self.ctx.consent().answer(id, d));
    }

    fn file(&self, name: &str, bytes: &[u8]) -> Outgoing {
        let path = self.dir.path().join(name);
        write(&path, bytes);
        Outgoing::File(OutgoingFile {
            path,
            name: name::sanitize(name),
            size: u64::try_from(bytes.len()).unwrap(),
            mime: None,
        })
    }

    fn received(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join("dl").join(name)).unwrap()
    }
}

/// `items` sent from `sender`, its code typed at `receiver`, whose user
/// answers `accept`: the send's id, and how the receive went.
async fn exchange(
    sender: &Side,
    receiver: &mut Side,
    items: Vec<Outgoing>,
    accept: bool,
) -> (TransferId, Result<TransferId, ErrorInfo>) {
    let id = sender.adapter.send(SendTarget::Croc, items).await.unwrap();
    let code = sender.code(id).await;
    let receiving = tokio::spawn({
        let adapter = receiver.adapter.clone();
        async move { adapter.receive_code(code).await }
    });
    receiver.answer(accept).await;
    (id, receiving.await.unwrap())
}

#[allow(clippy::disallowed_methods)] // A test's own scratch files.
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap() ^ seed)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_go_through_the_relay() {
    let relay = testing::start("s3cret", 4).await;
    let sender = side(&relay);
    let mut receiver = side(&relay);
    let big = pattern(300_001, 1);
    let items = vec![
        sender.file("big.bin", &big),
        sender.file("empty.txt", b""),
        sender.file("small.txt", b"hello"),
    ];
    let (id, received) = exchange(&sender, &mut receiver, items, true).await;
    let rid = received.unwrap();
    let (outcome, saved) = receiver.outcome(rid).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved, vec!["big.bin", "empty.txt", "small.txt"]);
    assert_eq!(receiver.received("big.bin"), big);
    assert_eq!(receiver.received("empty.txt"), b"");
    assert_eq!(receiver.received("small.txt"), b"hello");
    assert_eq!(sender.outcome(id).await.0, Outcome::Done);
    // Progress reached the whole on both sides.
    for (side, t) in [(&sender, id), (&receiver, rid)] {
        let last = side
            .events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::TransferProgress {
                    transfer,
                    bytes,
                    total,
                } if *transfer == t => Some((*bytes, *total)),
                _ => None,
            });
        assert_eq!(last, Some((300_006, 300_006)));
    }
    // The offer was croc's, with the files by name and size.
    let offered = receiver
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            Event::TransferStarted { transfer } if transfer.id == rid => Some(transfer.clone()),
            _ => None,
        });
    let offered = offered.unwrap();
    assert_eq!(offered.peer, "croc");
    assert_eq!(offered.file_count, 3);
    assert_eq!(offered.total_bytes, 300_006);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_text_arrives_as_a_text() {
    let relay = testing::start("pass123", 2).await;
    let sender = side(&relay);
    let mut receiver = side(&relay);
    let text = "Hello from croc <b>not bold</b>\nsecond line";
    let items = vec![Outgoing::Text(text.into())];
    let (id, received) = exchange(&sender, &mut receiver, items, true).await;
    let rid = received.unwrap();
    let (outcome, saved) = receiver.outcome(rid).await;
    assert_eq!(outcome, Outcome::Done);
    assert!(saved.is_empty(), "a text is not saved as a file");
    let got = receiver
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            Event::TextReceived {
                transfer,
                text,
                from,
            } if *transfer == rid => Some((text.clone(), from.clone())),
            _ => None,
        });
    assert_eq!(got, Some((text.to_owned(), "croc".to_owned())));
    assert_eq!(sender.outcome(id).await.0, Outcome::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declining_is_said_to_the_sender() {
    let relay = testing::start("pass123", 4).await;
    let sender = side(&relay);
    let mut receiver = side(&relay);
    let items = vec![sender.file("a.txt", b"abc")];
    let (id, received) = exchange(&sender, &mut receiver, items, false).await;
    let err = received.unwrap_err();
    assert_eq!(err.code, ErrorCode::Refused);
    match sender.outcome(id).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Refused),
        other => panic!("{other:?}"),
    }
    assert!(
        !receiver.dir.path().join("dl").join("a.txt").exists(),
        "nothing was saved"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_code_is_said_and_nothing_is_offered() {
    let relay = testing::start("pass123", 4).await;
    let sender = side(&relay);
    let mut receiver = side(&relay);
    let id = sender
        .adapter
        .send(SendTarget::Croc, vec![sender.file("a.txt", b"abc")])
        .await
        .unwrap();
    let code = sender.code(id).await;
    // The same room, other words.
    let wrong = format!("{}-wrong-words-here", code.split('-').next().unwrap());
    let err = receiver.adapter.receive_code(wrong).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::BadCode, "{err:?}");
    assert!(
        receiver.offers.try_recv().is_err(),
        "the user was never asked"
    );
    match sender.outcome(id).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::BadCode),
        other => panic!("{other:?}"),
    }
    // A code that is not one at all never reaches the network.
    let err = receiver
        .adapter
        .receive_code("12345".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadCode);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_receive_leaves_nothing() {
    let relay = testing::start("pass123", 4).await;
    let sender = side(&relay);
    let mut receiver = side(&relay);
    // Big enough to still be running when cancelled.
    let big = pattern(8 * 1024 * 1024, 7);
    let items = vec![sender.file("big.bin", &big)];
    let (id, received) = exchange(&sender, &mut receiver, items, true).await;
    let rid = received.unwrap();
    assert!(receiver.ctx.transfers().cancel(rid));
    assert_eq!(receiver.outcome(rid).await.0, Outcome::Cancelled);
    assert!(!matches!(sender.outcome(id).await.0, Outcome::Done));
    assert!(!receiver.dir.path().join("dl").join("big.bin").exists());
}
