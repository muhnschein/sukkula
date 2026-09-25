//! Hostile wormhole peers and servers (spec §5, §7 "Hostile input").
//!
//! The contract, as in clove's `evil_peer.rs`: no panic, no hang, no
//! unbounded allocation, no byte written outside the inbox, nothing
//! received without consent, and bad data never becomes a file. Each
//! scenario that fails a transfer ends by checking that nothing is left,
//! and several put an honest transfer through the same side afterwards.
//!
//! The peers here use magic-wormhole directly: its v1 sender for the lies
//! it can tell (a name, a size, a stream longer or shorter than the size),
//! hand-written JSON for the ones it cannot.

#![cfg(feature = "wormhole")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unreachable
)]

mod wormhole_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use magic_wormhole::Wormhole;
use magic_wormhole::transfer;
use magic_wormhole::transit::Abilities;
use serde_json::{Value, json};
use sukkula_core::limits::{MAX_FILE_BYTES, MAX_MESSAGE_BYTES};
use sukkula_engine::adapter::Outgoing;
use sukkula_engine::api::{ErrorCode, ErrorInfo, Event, Outcome, SendTarget};
use tokio_util::compat::TokioAsyncReadCompatExt;
use wormhole_support::*;

const CONSENT: Duration = Duration::from_secs(10);

struct World {
    mb: FakeMailbox,
    rl: FakeRelay,
    side: Side,
}

async fn world(behaviour: Behaviour, relay_mode: RelayMode) -> World {
    let mb = mailbox(behaviour).await;
    let rl = relay(relay_mode).await;
    let side = Side::new(&mb.url, &rl.url, CONSENT);
    World { mb, rl, side }
}

fn receive(side: &Side, code: String) -> tokio::task::JoinHandle<Result<u64, ErrorInfo>> {
    let adapter = side.adapter.clone();
    tokio::spawn(async move { adapter.receive_code(code).await })
}

/// A peer that allocates a code and then runs `script` on the wormhole.
async fn hostile<F, Fut, T>(w: &World, script: F) -> (String, tokio::task::JoinHandle<T>)
where
    F: FnOnce(Wormhole, String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T> + Send,
    T: Send + 'static,
{
    let (mc, code) = peer_allocate(&w.mb.url).await;
    let relay = w.rl.url.clone();
    let task = tokio::spawn(async move {
        let wh = Wormhole::connect(mc).await.unwrap();
        script(wh, relay).await
    });
    (code, task)
}

/// The library's own v1 sender, telling whatever lie it is given about the
/// name and the size of `data`.
async fn library_sender(
    w: &World,
    name: &'static str,
    declared: u64,
    data: Vec<u8>,
) -> (
    String,
    tokio::task::JoinHandle<Result<(), transfer::TransferError>>,
) {
    hostile(w, move |wh, relay| async move {
        let mut reader = std::io::Cursor::new(data).compat();
        transfer::send_file(
            wh,
            vec![relay_hint(&relay)],
            &mut reader,
            name,
            declared,
            Abilities::FORCE_RELAY,
            |_| {},
            |_, _| {},
            std::future::pending(),
        )
        .await
    })
    .await
}

/// Accepts the offer `side` is asked about and returns how the transfer
/// ended.
async fn accept_and_finish(
    side: &Side,
    rx: tokio::task::JoinHandle<Result<u64, ErrorInfo>>,
) -> Outcome {
    let (id, _) = side.pending().await;
    side.answer(id, true);
    let transfer = rx.await.unwrap().unwrap();
    side.finished(transfer).await.0
}

/// An honest transfer through the same side, after whatever went before.
async fn honest_transfer_still_works(w: &World) {
    let data = content(4321);
    let (code, peer) = library_sender(w, "honest.bin", 4321, data.clone()).await;
    let rx = receive(&w.side, code);
    let (id, _) = loop {
        let pending = w.side.log.lock().unwrap().pending.last().cloned();
        if let Some(p) = pending
            && p.1
                .files
                .first()
                .is_some_and(|f| f.name.as_str() == "honest.bin")
        {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    w.side.answer(id, true);
    let t = rx.await.unwrap().unwrap();
    assert_eq!(w.side.finished(t).await.0, Outcome::Done);
    peer.await.unwrap().unwrap();
    assert!(
        w.side
            .received()
            .iter()
            .any(|(n, d)| n == "honest.bin" && *d == data)
    );
}

fn failed(o: &Outcome) -> bool {
    matches!(o, Outcome::Failed { .. })
}

// --------------------------------------------------------- lying senders

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn traversal_names_land_inside_the_inbox_under_safe_names() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    for (evil, safe) in [
        ("../../../../etc/passwd", "passwd"),
        ("/etc/shadow", "shadow"),
        ("..", "received-file"),
        ("a\\..\\..\\b.txt", "b.txt"),
        (".bashrc", "bashrc"),
        ("photo\u{202E}gpj.exe", "photogpj.exe"),
        ("", "received-file"),
    ] {
        let (code, peer) = library_sender(&w, evil, 3, b"abc".to_vec()).await;
        let rx = receive(&w.side, code);
        let (id, offer) = loop {
            let p = w.side.log.lock().unwrap().pending.last().cloned();
            if let Some(p) = p
                && !w.side.log.lock().unwrap().closed.contains(&p.0)
            {
                break p;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(offer.files[0].name.as_str(), safe, "{evil:?}");
        w.side.answer(id, true);
        let t = rx.await.unwrap().unwrap();
        let (outcome, saved) = w.side.finished(t).await;
        assert_eq!(outcome, Outcome::Done, "{evil:?}");
        assert!(
            saved[0].starts_with(safe.split('.').next().unwrap()),
            "{saved:?}"
        );
        peer.await.unwrap().unwrap();
    }
    // Everything is a plain file directly in the download directory.
    assert_eq!(w.side.received().len(), 7);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn negative_and_oversized_offers_never_reach_the_user() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let too_big = MAX_FILE_BYTES + 1;
    for (size, code) in [
        (json!(-1), ErrorCode::Refused),
        (json!(i64::MIN), ErrorCode::Refused),
        (json!(-1e30), ErrorCode::Refused),
        (json!(u64::MAX), ErrorCode::TooLarge),
        (json!(1e30), ErrorCode::TooLarge),
        (json!(too_big), ErrorCode::TooLarge),
        (json!(5.5), ErrorCode::Network),
        (json!("12"), ErrorCode::Network),
    ] {
        let offer = json!({"offer": {"file": {"filename": "x", "filesize": size}}});
        let (c, peer) = hostile(&w, move |wh, _| scripted_offer(wh, offer)).await;
        let e = w.side.adapter.receive_code(c).await.unwrap_err();
        assert_eq!(e.code, code, "{size}");
        // The peer is told no, in the v1 way.
        let answer = peer.await.unwrap();
        assert!(answer.get("error").is_some(), "{answer}");
    }
    let text = "x".repeat(MAX_MESSAGE_BYTES + 1);
    let offer = json!({"offer": {"message": text}});
    let (c, peer) = hostile(&w, move |wh, _| scripted_offer(wh, offer)).await;
    let e = w.side.adapter.receive_code(c).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::TooLarge);
    assert!(peer.await.unwrap().get("error").is_some());
    assert!(!w.side.was_asked(), "S4: refused before anyone was asked");
    assert_eq!(w.rl.connections.load(Ordering::SeqCst), 0);
    assert!(w.side.received().is_empty());
    honest_transfer_still_works(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_and_unknown_offers_are_declined() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    for (offer, reply) in [
        (json!({"offer": {"hologram": {"size": 1}}}), true),
        (json!({"offer": {"file": {"filesize": 1}}}), false),
        (json!({"offer": {"message": "a", "file": {}}}), false),
        (json!({"offer": "x"}), false),
        (json!({"answer": {"file_ack": "ok"}}), false),
        (json!({"offer": {"message": "a"}, "error": "b"}), false),
        (json!({"error": "never mind"}), false),
    ] {
        let o = offer.clone();
        let (c, peer) = hostile(&w, move |mut wh, _| async move {
            wh.send_json(&o).await.unwrap();
            // Wait for an answer if one comes; otherwise the session ends.
            tokio::time::timeout(Duration::from_secs(5), wh.receive_json::<Value>())
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(Result::ok)
        })
        .await;
        let e = w.side.adapter.receive_code(c).await.unwrap_err();
        assert_ne!(e.code, ErrorCode::Internal, "{offer}: {e:?}");
        let answer = peer.await.unwrap();
        if reply {
            assert_eq!(answer, Some(json!({"error": "unsupported offer"})));
        }
    }
    assert!(!w.side.was_asked());
    honest_transfer_still_works(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn more_data_than_declared_is_refused_and_leaves_nothing() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let (code, peer) = library_sender(&w, "short.bin", 5, content(40_000)).await;
    let rx = receive(&w.side, code);
    let outcome = accept_and_finish(&w.side, rx).await;
    match outcome {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Network),
        other => panic!("{other:?}"),
    }
    let _ = peer.await.unwrap();
    assert!(w.side.received().is_empty());
    honest_transfer_still_works(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn less_data_than_declared_is_refused_and_leaves_nothing() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let (code, peer) = library_sender(&w, "long.bin", 90_000, content(30_000)).await;
    let rx = receive(&w.side, code);
    assert!(failed(&accept_and_finish(&w.side, rx).await));
    assert!(peer.await.unwrap().is_err());
    assert!(w.side.received().is_empty());
    honest_transfer_still_works(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sender_that_stalls_after_the_yes_times_out() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let (code, _peer) = hostile(&w, |wh, relay| async move {
        scripted_send(
            wh,
            &relay,
            json!({"offer": {"file": {"filename": "slow.bin", "filesize": 1000}}}),
            Payload::Stall,
        )
        .await
    })
    .await;
    let rx = receive(&w.side, code);
    let start = tokio::time::Instant::now();
    match accept_and_finish(&w.side, rx).await {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Network),
        other => panic!("{other:?}"),
    }
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "bounded by the idle timeout"
    );
    assert!(w.side.received().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sender_that_never_offers_times_out() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let (code, _peer) = hostile(&w, |wh, _| async move {
        tokio::time::sleep(DEADLINE).await;
        drop(wh);
    })
    .await;
    let start = tokio::time::Instant::now();
    let e = w.side.adapter.receive_code(code).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Network);
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flood_of_messages_is_cut_off() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let (code, _peer) = hostile(&w, |mut wh, _| async move {
        for i in 0..100 {
            if wh.send_json(&json!({ "chatter": i })).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(DEADLINE).await;
    })
    .await;
    let e = w.side.adapter.receive_code(code).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Network);
    assert!(!w.side.was_asked());
}

/// A raw transit sender offering `size` bytes as `name`, then `wire`;
/// returns how the receive ended.
async fn raw_transfer(w: &World, name: &'static str, size: u64, wire: Vec<Wire>) -> Outcome {
    let (code, peer) = hostile(w, move |wh, relay| async move {
        raw_send(
            wh,
            &relay,
            json!({"offer": {"file": {"filename": name, "filesize": size}}}),
            wire,
        )
        .await
    })
    .await;
    let rx = receive(&w.side, code);
    let outcome = accept_and_finish(&w.side, rx).await;
    assert_eq!(peer.await.unwrap(), json!({"answer": {"file_ack": "ok"}}));
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_raw_transit_peer_is_a_faithful_sender() {
    // The baseline for the tests below: sealed by hand, received intact.
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let data = content(40_000);
    let wire = data
        .chunks(10_000)
        .map(|c| Wire::Record(c.to_vec()))
        .collect();
    assert_eq!(
        raw_transfer(&w, "raw.bin", 40_000, wire).await,
        Outcome::Done
    );
    assert_eq!(w.side.received(), vec![("raw.bin".to_owned(), data)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_records_do_not_count_as_data() {
    // magic-wormhole will not send an empty record; a hostile peer will,
    // as many as it likes.
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let wire = (0..1000).map(|_| Wire::Record(Vec::new())).collect();
    match raw_transfer(&w, "e.bin", 10, wire).await {
        Outcome::Failed { error } => assert_eq!(error.message, "the sender sent empty records"),
        other => panic!("{other:?}"),
    }
    assert!(w.side.received().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oversized_record_is_refused_at_the_guard() {
    // Validly sealed, and within the declared size, but larger than any
    // record a reference client sends: the guard cuts it before the library
    // allocates for it.
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let big = 5 * 1024 * 1024;
    let wire = vec![Wire::Record(vec![7u8; big])];
    let start = tokio::time::Instant::now();
    assert!(failed(
        &raw_transfer(&w, "big.bin", u64::try_from(big).unwrap(), wire).await
    ));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(w.side.received().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replayed_nonce_fails_the_transfer() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let wire = vec![Wire::Record(content(100)), Wire::WrongNonce(content(100))];
    assert!(failed(&raw_transfer(&w, "n.bin", 200, wire).await));
    assert!(w.side.received().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_loris_record_times_out() {
    // One byte every 100 ms: always progress, never a record.
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let wire = vec![Wire::Trickle(content(1000), Duration::from_millis(100))];
    let start = tokio::time::Instant::now();
    match raw_transfer(&w, "slow.bin", 1000, wire).await {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Network),
        other => panic!("{other:?}"),
    }
    assert!(start.elapsed() < Duration::from_secs(10));
    assert!(w.side.received().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sender_that_gives_up_takes_its_offer_off_the_screen() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let (code, _peer) = hostile(&w, |mut wh, _| async move {
        wh.send_json(&json!({"offer": {"message": "wait for it"}}))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        wh.send_json(&json!({"error": "changed my mind"}))
            .await
            .unwrap();
        tokio::time::sleep(DEADLINE).await;
    })
    .await;
    let rx = receive(&w.side, code);
    let (id, _) = w.side.pending().await;
    let e = rx.await.unwrap().unwrap_err();
    assert_eq!(e.code, ErrorCode::Refused);
    assert!(w.side.log.lock().unwrap().closed.contains(&id));
    assert!(
        !w.side
            .ctx
            .consent()
            .answer(id, sukkula_core::consent::Decision::Accept)
    );
}

// ------------------------------------------------------ lying receivers

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receiver_that_lies_about_the_digest_fails_the_send() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let data = content(20_000);
    let path = write_file(w.side.dir.path(), "d.bin", &data);
    let item = Outgoing::File(sukkula_engine::adapter::OutgoingFile {
        path,
        name: sukkula_core::name::sanitize("d.bin"),
        size: 20_000,
        mime: None,
    });
    let sent = w
        .side
        .adapter
        .send(SendTarget::Wormhole, vec![item])
        .await
        .unwrap();
    let code = w.side.code().await;
    let (mb, rl) = (w.mb.url.clone(), w.rl.url.clone());
    tokio::spawn(async move {
        use magic_wormhole::transit::{self, Hints, TransitRole};
        let mut wh = peer_connect(&mb, &code).await;
        let connector = transit::init(Abilities::FORCE_RELAY, None, vec![relay_hint(&rl)])
            .await
            .unwrap();
        let (mut their_a, mut their_h) = (None, None);
        loop {
            let m = recv_json(&mut wh).await;
            if let Some(t) = m.get("transit") {
                their_a = serde_json::from_value::<Abilities>(t["abilities-v1"].clone()).ok();
                their_h = serde_json::from_value::<Hints>(t["hints-v1"].clone()).ok();
            }
            if m.get("offer").is_some() {
                break;
            }
        }
        wh.send_json(&json!({"transit": {
            "abilities-v1": connector.our_abilities(),
            "hints-v1": &**connector.our_hints(),
        }}))
        .await
        .unwrap();
        wh.send_json(&json!({"answer": {"file_ack": "ok"}}))
            .await
            .unwrap();
        let key = transit_key(&wh);
        let (mut t, _) = connector
            .connect(
                TransitRole::Follower,
                key,
                their_a.unwrap(),
                std::sync::Arc::new(their_h.unwrap()),
            )
            .await
            .unwrap();
        let mut got = 0;
        while got < 20_000 {
            got += t.receive_record().await.unwrap().len();
        }
        let lie = json!({"ack": "ok", "sha256": "00".repeat(32)}).to_string();
        t.send_record(lie.as_bytes()).await.unwrap();
        t.flush().await.unwrap();
        tokio::time::sleep(DEADLINE).await;
    });
    match w.side.finished(sent).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Network),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receiver_that_answers_a_text_with_an_error_fails_the_send() {
    let w = world(Behaviour::default(), RelayMode::Honest).await;
    let sent = w
        .side
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("hi".into())])
        .await
        .unwrap();
    let code = w.side.code().await;
    let mut wh = peer_connect(&w.mb.url, &code).await;
    let _offer = recv_json(&mut wh).await;
    wh.send_json(&json!({"error": "transfer rejected"}))
        .await
        .unwrap();
    match w.side.finished(sent).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Refused),
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------- hostile servers

/// A receive against `behaviour`: it must fail, quickly, and with no offer.
async fn receive_fails_against(behaviour: Behaviour) -> ErrorInfo {
    let w = world(behaviour, RelayMode::Honest).await;
    let start = tokio::time::Instant::now();
    let e = tokio::time::timeout(
        Duration::from_secs(15),
        w.side.adapter.receive_code("7-guitarist-revenge".into()),
    )
    .await
    .expect("a hostile mailbox made the receive hang")
    .unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(12));
    assert!(!w.side.was_asked());
    assert!(w.side.received().is_empty());
    e
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mailbox_demanding_endless_hashcash_is_refused() {
    // The library would mint this in a loop that never yields (W1).
    let welcome = json!({"type": "welcome", "welcome": {"permission-required": {
        "hashcash": {"bits": 200, "resource": "sukkula"}}}});
    let e = receive_fails_against(Behaviour {
        welcome: Some(welcome.clone()),
        nameplates: vec!["7".into()],
        ..Behaviour::default()
    })
    .await;
    assert_eq!(e.code, ErrorCode::Network);
    assert_eq!(e.message, "the mailbox server demands too much work");

    // The send side goes through the same guard.
    let mb = mailbox(Behaviour {
        welcome: Some(welcome),
        ..Behaviour::default()
    })
    .await;
    let a = Side::new(&mb.url, "tcp://127.0.0.1:9", CONSENT);
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("x".into())])
        .await
        .unwrap();
    match a.finished(sent).await.0 {
        Outcome::Failed { error } => {
            assert_eq!(error.message, "the mailbox server demands too much work");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_modest_hashcash_is_minted_and_the_receive_goes_on() {
    // magic-wormhole 0.8.1 cannot parse a `permission-required` without a
    // `none` key (W12), so the fake sends one.
    let welcome = json!({"type": "welcome", "welcome": {"permission-required": {
        "none": null, "hashcash": {"bits": 8, "resource": "sukkula"}}}});
    let e = receive_fails_against(Behaviour {
        welcome: Some(welcome),
        ..Behaviour::default()
    })
    .await;
    // Past the welcome: the nameplate does not exist.
    assert_eq!(e.code, ErrorCode::BadCode, "{e:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_the_library_would_panic_on_are_stopped_at_the_guard() {
    let body = "ab".repeat(40);
    for (frame, why) in [
        (
            json!({"type": "message", "side": "evil", "phase": "dilate-0", "body": body}),
            "todo!() on a non-numeric phase (W2)",
        ),
        (
            json!({"type": "message", "side": "evil", "phase": "4", "body": "abcd"}),
            "split_at on a short body (W3)",
        ),
        (
            json!({"type": "message", "side": "evil", "phase": "4", "body": "zz"}),
            "not hex",
        ),
        (json!({"type": "message"}), "no fields"),
    ] {
        let e = receive_fails_against(Behaviour {
            after_open: vec![frame.to_string()],
            nameplates: vec!["7".into()],
            ..Behaviour::default()
        })
        .await;
        assert_eq!(e.code, ErrorCode::Network, "{why}");
        assert_eq!(
            e.message, "the mailbox server relayed a malformed message",
            "{why}"
        );
    }
}

/// How many panics of the two kinds the mailbox guard exists to prevent --
/// W3's `split_at` ("mid > len") and W2's `todo!()` -- have happened
/// anywhere in this test binary. `session::CatchUnwind` turns either into a
/// failed transfer, so the outcome alone cannot tell a panic caught from
/// one prevented. No test here should ever cause one.
fn library_panics() -> usize {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let what = info.payload_as_str().unwrap_or("");
            if what == "mid > len" || what.starts_with("not yet implemented") {
                COUNT.fetch_add(1, Ordering::SeqCst);
            }
            previous(info);
        }));
    });
    COUNT.load(Ordering::SeqCst)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_messages_out_of_the_order_the_library_reads_them_are_stopped_at_the_guard() {
    let before = library_panics();
    // A PAKE the library's key exchange accepts: symmetric 'S', then the
    // Ed25519 base point.
    let element = format!("5358{}", "66".repeat(31));
    let pake = sukkula_core::hex::encode(format!(r#"{{"pake_v1":"{element}"}}"#).as_bytes());
    let frame = |phase: &str, body: &str| {
        json!({"type": "message", "side": "evil", "phase": phase, "body": body}).to_string()
    };
    for (frames, why) in [
        (
            // Each passes a check of its phase alone: a "version" long
            // enough to be sealed, a "pake" short enough to be a PAKE. The
            // library takes the first as the PAKE and decrypts the second
            // as the version message: split_at(24) on two bytes (W3). No
            // code needed; anyone on the ws:// path can send these.
            vec![frame("version", &pake), frame("pake", "7b7d")],
            "a PAKE under version, then a short pake",
        ),
        (
            // A PAKE under a number: taken as the PAKE all the same, which
            // leaves "pake" unused for a later todo!() (W2).
            vec![frame("7", &pake)],
            "a PAKE under a number",
        ),
    ] {
        let e = receive_fails_against(Behaviour {
            after_open: frames,
            nameplates: vec!["7".into()],
            ..Behaviour::default()
        })
        .await;
        assert_eq!(
            library_panics(),
            before,
            "{why}: the library panicked -- caught, not prevented"
        );
        assert_eq!(e.code, ErrorCode::Network, "{why}: {e:?}");
        assert_eq!(
            e.message, "the mailbox server relayed a message out of order",
            "{why}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_and_endless_server_messages_are_cut_off() {
    let big = json!({"type": "motd", "pad": "a".repeat(2 * 1024 * 1024)}).to_string();
    let e = receive_fails_against(Behaviour {
        after_open: vec![big],
        nameplates: vec!["7".into()],
        ..Behaviour::default()
    })
    .await;
    assert_eq!(e.message, "the mailbox server sent an oversized message");
    // A type the library ignores, so only the guard's budget ends it.
    let flood = vec![json!({"type": "noise", "n": 1}).to_string(); 500];
    let e = receive_fails_against(Behaviour {
        after_open: flood,
        nameplates: vec!["7".into()],
        ..Behaviour::default()
    })
    .await;
    assert_eq!(e.message, "the mailbox server sent too much");
    let e = receive_fails_against(Behaviour {
        after_open: vec!["{not json".into()],
        nameplates: vec!["7".into()],
        ..Behaviour::default()
    })
    .await;
    assert_eq!(e.message, "the mailbox server sent malformed JSON");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receives_that_are_still_connecting_are_bounded() {
    // Nameplates nobody is behind: each receive waits in the key exchange
    // until its timeout, holding a mailbox connection and a guard.
    let n = sukkula_engine::wormhole::MAX_CONNECTING_RECEIVES;
    let w = world(
        Behaviour {
            nameplates: (1..=n + 1).map(|i| i.to_string()).collect(),
            ..Behaviour::default()
        },
        RelayMode::Honest,
    )
    .await;
    let waiting: Vec<_> = (1..=n)
        .map(|i| receive(&w.side, format!("{i}-guitarist-revenge")))
        .collect();
    let start = tokio::time::Instant::now();
    while w.mb.connections.load(Ordering::SeqCst) < n {
        assert!(start.elapsed() < DEADLINE);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let e = w
        .side
        .adapter
        .receive_code(format!("{}-guitarist-revenge", n + 1))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::TooLarge);
    assert_eq!(w.mb.connections.load(Ordering::SeqCst), n);
    for r in waiting {
        assert_eq!(r.await.unwrap().unwrap_err().code, ErrorCode::Network);
    }
    // The slots come back.
    honest_transfer_still_works(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_mailbox_fails_fast() {
    // A port nothing listens on.
    let spare = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1", spare.local_addr().unwrap());
    drop(spare);
    let b = Side::new(&url, "tcp://127.0.0.1:9", CONSENT);
    let e = b
        .adapter
        .receive_code("7-guitarist-revenge".into())
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Network);
}

/// A `wss://` server on loopback with a certificate for "localhost" that
/// nobody vouches for: someone in the middle. Counts the TLS handshakes it
/// completes.
async fn untrusted_tls_mailbox() -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::Mutex<Vec<String>>>,
) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert = ck.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        ck.signing_key.serialize_der(),
    ));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failures = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (c, f) = (completed.clone(), failures.clone());
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            match acceptor.accept(s).await {
                Ok(_) => {
                    c.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => f.lock().unwrap().push(format!("{e:?}")),
            }
        }
    });
    (format!("wss://localhost:{port}/v1"), completed, failures)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wss_mailbox_nobody_vouches_for_is_refused() {
    // F-MW4: wss:// goes through the guard's rustls and the platform
    // verifier, which refuses a certificate no system root signed.
    let (url, completed, failures) = untrusted_tls_mailbox().await;
    let b = Side::new(&url, "tcp://127.0.0.1:9", CONSENT);
    let e = b
        .adapter
        .receive_code("7-guitarist-revenge".into())
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Network);
    assert_eq!(e.message, "the mailbox server's TLS handshake failed");
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    // The client got as far as the certificate, and turned it down.
    let start = tokio::time::Instant::now();
    while failures.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let seen = failures.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|f| f.contains("UnknownCA") || f.contains("BadCertificate")),
        "{seen:?}"
    );
    // A plain server where TLS was asked for fails the same way.
    let plain = mailbox(Behaviour::default()).await;
    let url = plain.url.replace("ws://127.0.0.1", "wss://localhost");
    let b = Side::new(&url, "tcp://127.0.0.1:9", CONSENT);
    let e = b
        .adapter
        .receive_code("7-guitarist-revenge".into())
        .await
        .unwrap_err();
    assert_eq!(e.message, "the mailbox server's TLS handshake failed");
}

// --------------------------------------------------------- hostile relay

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lying_record_length_is_refused_before_anything_is_allocated() {
    // What the library would take as `Vec::with_capacity(4 GiB)` (W6),
    // written by a relay, which sees the unencrypted framing.
    let w = world(
        Behaviour::default(),
        RelayMode::Inject(vec![0xff, 0xff, 0xff, 0xff, 1, 2, 3]),
    )
    .await;
    let (code, _peer) = library_sender(&w, "x.bin", 50_000, content(50_000)).await;
    let rx = receive(&w.side, code);
    match accept_and_finish(&w.side, rx).await {
        // The guard cut the connection: a failed read, not a timeout.
        Outcome::Failed { error } => {
            assert_eq!(error.code, ErrorCode::Network);
            assert_eq!(error.message, "the transit connection failed");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(w.rl.paired.load(Ordering::SeqCst), 1);
    assert!(w.side.received().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_code_from_the_mailbox_never_reaches_the_screen() {
    // The nameplate is the server's choice. A code the user could not have
    // typed -- a bidi override, or a kilobyte of digits -- is not shown.
    for forced in ["7\u{202E}1", &"9".repeat(1000), "x"] {
        let mb = mailbox(Behaviour {
            allocate_as: Some(forced.to_owned()),
            ..Behaviour::default()
        })
        .await;
        let a = Side::new(&mb.url, "tcp://127.0.0.1:9", CONSENT);
        let sent = a
            .adapter
            .send(SendTarget::Wormhole, vec![Outgoing::Text("x".into())])
            .await
            .unwrap();
        match a.finished(sent).await.0 {
            Outcome::Failed { .. } => {}
            other => panic!("{forced:?}: {other:?}"),
        }
        let shown = a
            .log
            .lock()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e, Event::WormholeCode { .. }));
        assert!(!shown, "{forced:?}: a malformed code was shown");
    }
}

/// A mailbox whose welcome asks for hashcash the library takes seconds to
/// mint (18 bits over the longest resource the guard allows: about four
/// seconds on average in a debug build), minted in a loop that never
/// yields (W1).
async fn minting_mailbox() -> FakeMailbox {
    mailbox(Behaviour {
        welcome: Some(
            json!({"type": "welcome", "welcome": {"permission-required": {
            "none": null, "hashcash": {"bits": 18, "resource": "r".repeat(256)}}}}),
        ),
        ..Behaviour::default()
    })
    .await
}

/// Runs `attempt` until one of its mints was still running when it had
/// judged what it came to judge; mint lengths are geometric, and one that
/// happens to end at once proves nothing either way.
async fn with_a_long_mint<F, Fut>(mut attempt: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..6 {
        if attempt().await {
            return;
        }
    }
    panic!("no mint was ever still running when judged");
}

/// The longest the runtime went, by the wall clock, without running a task
/// that asks to run every 10 ms: how long its workers were held.
struct Stalls {
    last: Arc<std::sync::Mutex<(std::time::Instant, Duration)>>,
    task: tokio::task::JoinHandle<()>,
}

impl Stalls {
    fn watch() -> Stalls {
        let last = Arc::new(std::sync::Mutex::new((
            std::time::Instant::now(),
            Duration::ZERO,
        )));
        let l = last.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let mut g = l.lock().unwrap();
                let gap = g.0.elapsed();
                *g = (std::time::Instant::now(), g.1.max(gap));
            }
        });
        Stalls { last, task }
    }

    /// The worst stall so far, counting one still going on.
    fn worst(&self) -> Duration {
        let g = self.last.lock().unwrap();
        g.1.max(g.0.elapsed())
    }
}

impl Drop for Stalls {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// One worker, so a mint on it would hold the whole runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_hashcash_mint_holds_up_neither_the_runtime_nor_an_engine_stop() {
    with_a_long_mint(|| async {
        let mb = minting_mailbox().await;
        let side = Side::new(&mb.url, "tcp://127.0.0.1:9", CONSENT);
        let stalls = Stalls::watch();
        let rx = receive(&side, "7-guitarist-revenge".into());
        while mb.connections.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The welcome is on its way to the library, which mints.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let stopping = std::time::Instant::now();
        side.ctx.shut_down();
        let e = rx.await.unwrap().unwrap_err();
        let stop_took = stopping.elapsed();
        let stalled = stalls.worst();
        assert!(
            stalled < Duration::from_millis(500),
            "the runtime was held for {stalled:?}"
        );
        if mb.has_received("submit-permission") {
            return false;
        }
        assert!(
            stop_took < Duration::from_secs(1),
            "the stop took {stop_took:?}"
        );
        assert_eq!(e.message, "cancelled");
        true
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_hashcash_mint_does_not_hold_up_a_cancel() {
    with_a_long_mint(|| async {
        let mb = minting_mailbox().await;
        let side = Side::new(&mb.url, "tcp://127.0.0.1:9", CONSENT);
        let stalls = Stalls::watch();
        let id = side
            .adapter
            .send(SendTarget::Wormhole, vec![Outgoing::Text("x".into())])
            .await
            .unwrap();
        while mb.connections.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let cancelling = std::time::Instant::now();
        let cancelled = side.ctx.transfers().cancel(id);
        let outcome = side.finished(id).await.0;
        let cancel_took = cancelling.elapsed();
        let stalled = stalls.worst();
        assert!(
            stalled < Duration::from_millis(500),
            "the runtime was held for {stalled:?}"
        );
        if mb.has_received("submit-permission") {
            return false;
        }
        assert!(cancelled, "the send ended before the cancel: {outcome:?}");
        assert!(
            cancel_took < Duration::from_secs(1),
            "the cancel took {cancel_took:?}"
        );
        assert_eq!(outcome, Outcome::Cancelled);
        true
    })
    .await;
}
