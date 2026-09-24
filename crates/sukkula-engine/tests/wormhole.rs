//! The wormhole adapter end to end (F-MW1..F-MW4), offline: Sukkula to
//! Sukkula and to magic-wormhole's own v1 implementation (the one the Rust
//! CLI uses), through an in-process mailbox and relay
//! (`wormhole_support`). Hostile peers and servers are in
//! `wormhole_hostile.rs`; the Python reference client in
//! `wormhole_interop.rs`.

#![cfg(feature = "wormhole")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unreachable,
    clippy::too_many_lines
)]

mod wormhole_support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use magic_wormhole::Wormhole;
use magic_wormhole::transfer;
use magic_wormhole::transit::Abilities;
use serde_json::json;
use sukkula_core::name;
use sukkula_engine::adapter::{Outgoing, OutgoingFile};
use sukkula_engine::api::{
    API_VERSION, Direction, ErrorCode, Event, Outcome, SendTarget, StartConfig,
};
use sukkula_engine::ctx::EventSink;
use sukkula_engine::{Engine, wormhole};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use wormhole_support::*;

const CONSENT: Duration = Duration::from_secs(10);

fn file_item(side: &Side, name: &str, data: &[u8]) -> Outgoing {
    let path = write_file(side.dir.path(), name, data);
    Outgoing::File(OutgoingFile {
        path,
        name: name::sanitize(name),
        size: u64::try_from(data.len()).unwrap(),
        mime: None,
    })
}

/// Starts a receive on `side` in the background: it returns only once the
/// offer is answered.
fn receive(
    side: &Side,
    code: String,
) -> tokio::task::JoinHandle<Result<u64, sukkula_engine::api::ErrorInfo>> {
    let adapter = side.adapter.clone();
    tokio::spawn(async move { adapter.receive_code(code).await })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_goes_from_sukkula_to_sukkula() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let data = content(300_000);
    let sent = a
        .adapter
        .send(
            SendTarget::Wormhole,
            vec![file_item(&a, "photo.jpg", &data)],
        )
        .await
        .unwrap();

    // F-MW1: a two-word code and its QR code, for this transfer.
    let (code, qr) = match a.event(|e| matches!(e, Event::WormholeCode { .. })).await {
        Event::WormholeCode { transfer, code, qr } => {
            assert_eq!(transfer, sent);
            (code, qr)
        }
        _ => unreachable!(),
    };
    let parts: Vec<&str> = code.split('-').collect();
    assert_eq!(parts.len(), 3, "{code}");
    assert!(parts[0].bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(qr.rows.len(), usize::try_from(qr.size).unwrap());

    let rx = receive(&b, code);
    let (offer_id, offer) = b.pending().await;
    assert_eq!(offer.files.len(), 1);
    assert_eq!(offer.files[0].name.as_str(), "photo.jpg");
    assert_eq!(offer.files[0].size, 300_000);
    assert_eq!(offer.sender, wormhole::PEER_LABEL);
    // S5: nothing flowed before the yes.
    assert_eq!(rl.connections.load(Ordering::SeqCst), 0);
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();

    let (outcome, saved) = b.finished(got).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved, vec!["photo.jpg".to_owned()]);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    assert_eq!(b.received(), vec![("photo.jpg".to_owned(), data)]);

    let started = b
        .event(|e| matches!(e, Event::TransferStarted { transfer } if transfer.id == got))
        .await;
    let Event::TransferStarted { transfer } = started else {
        unreachable!()
    };
    assert_eq!(transfer.direction, Direction::Incoming);
    assert_eq!(transfer.total_bytes, 300_000);
    b.event(|e| {
        matches!(e, Event::TransferProgress { transfer, bytes, total }
            if *transfer == got && bytes == total)
    })
    .await;
    a.event(|e| {
        matches!(e, Event::TransferProgress { transfer, bytes, total }
            if *transfer == sent && bytes == total)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_text_goes_from_sukkula_to_sukkula_as_plain_text() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let text = "hello \u{202E}there https://example.com/";
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text(text.into())])
        .await
        .unwrap();
    let rx = receive(&b, a.code().await);
    let (offer_id, offer) = b.pending().await;
    assert!(offer.files.is_empty());
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    // F-C4 and S2: the text arrives sanitised, and only after the yes.
    match b.event(|e| matches!(e, Event::TextReceived { .. })).await {
        Event::TextReceived {
            transfer,
            text,
            from,
        } => {
            assert_eq!(transfer, got);
            assert_eq!(text, "hello there https://example.com/");
            assert_eq!(from, wormhole::PEER_LABEL);
        }
        _ => unreachable!(),
    }
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    assert!(b.received().is_empty());
    assert_eq!(
        rl.connections.load(Ordering::SeqCst),
        0,
        "a text needs no transit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_largest_text_fits_through_the_guards_at_its_worst() {
    // 64 KiB of the characters JSON escapes into six bytes each: the
    // largest message either guard or the peer-message cap has to carry.
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let mut text = "\u{1}".repeat(sukkula_core::limits::MAX_MESSAGE_BYTES - 1);
    text.push('x');
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text(text)])
        .await
        .unwrap();
    let rx = receive(&b, a.code().await);
    let (offer_id, _) = b.pending().await;
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    match b.event(|e| matches!(e, Event::TextReceived { .. })).await {
        // S2: the control characters are gone.
        Event::TextReceived { text, .. } => assert_eq!(text, "x"),
        _ => unreachable!(),
    }
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declined_offer_moves_no_data_and_leaves_no_file() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let sent = a
        .adapter
        .send(
            SendTarget::Wormhole,
            vec![file_item(&a, "x.bin", &content(5000))],
        )
        .await
        .unwrap();
    let rx = receive(&b, a.code().await);
    let (offer_id, _) = b.pending().await;
    b.answer(offer_id, false);
    let e = rx.await.unwrap().unwrap_err();
    assert_eq!(e.code, ErrorCode::Refused);
    match a.finished(sent).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Refused),
        other => panic!("{other:?}"),
    }
    assert!(b.received().is_empty());
    assert_eq!(
        rl.connections.load(Ordering::SeqCst),
        0,
        "no transit, not even a connection"
    );
    assert!(
        !b.log
            .lock()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e, Event::TransferStarted { .. })),
        "a declined offer is no transfer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unanswered_offer_times_out() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, Duration::from_millis(500));
    let sent = a
        .adapter
        .send(
            SendTarget::Wormhole,
            vec![file_item(&a, "x.bin", &content(10))],
        )
        .await
        .unwrap();
    let rx = receive(&b, a.code().await);
    let e = rx.await.unwrap().unwrap_err();
    assert_eq!(e.code, ErrorCode::Refused);
    assert!(b.was_asked());
    assert!(matches!(a.finished(sent).await.0, Outcome::Failed { .. }));
    assert!(b.received().is_empty());
    assert_eq!(rl.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_mid_transfer_leaves_no_partial_file() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let data = content(64 * 1024 * 1024);
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![file_item(&a, "big.bin", &data)])
        .await
        .unwrap();
    let rx = receive(&b, a.code().await);
    let (offer_id, _) = b.pending().await;
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    b.event(|e| matches!(e, Event::TransferProgress { transfer, bytes, .. } if *transfer == got && *bytes > 0))
        .await;
    assert!(b.ctx.transfers().cancel(got));
    assert_eq!(b.finished(got).await.0, Outcome::Cancelled);
    assert!(matches!(a.finished(sent).await.0, Outcome::Failed { .. }));
    assert!(b.received().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_nobody_receives_can_be_cancelled() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("hi".into())])
        .await
        .unwrap();
    a.code().await;
    assert!(a.ctx.transfers().cancel(sent));
    let start = tokio::time::Instant::now();
    assert_eq!(a.finished(sent).await.0, Outcome::Cancelled);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_carries_one_item() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let e = a
        .adapter
        .send(
            SendTarget::Wormhole,
            vec![Outgoing::Text("a".into()), Outgoing::Text("b".into())],
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::TooLarge);
    let e = a
        .adapter
        .send(SendTarget::Wormhole, vec![])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadCommand);
    assert_eq!(mb.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_that_changed_or_became_a_fifo_is_not_sent() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    // The hub measured 100 bytes; there are 50 now.
    let Outgoing::File(mut changed) = file_item(&a, "c.bin", &content(50)) else {
        unreachable!()
    };
    changed.size = 100;
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::File(changed)])
        .await
        .unwrap();
    match a.finished(sent).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::BadFile),
        other => panic!("{other:?}"),
    }
    // A FIFO swapped in after the check: refused, not waited on.
    let fifo = a.dir.path().join("f.bin");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    let item = Outgoing::File(OutgoingFile {
        path: fifo,
        name: name::sanitize("f.bin"),
        size: 10,
        mime: None,
    });
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![item])
        .await
        .unwrap();
    let start = tokio::time::Instant::now();
    match a.finished(sent).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::BadFile),
        other => panic!("{other:?}"),
    }
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(
        mb.connections.load(Ordering::SeqCst),
        0,
        "no code for a bad file"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codes_are_checked_before_the_network() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    for code in [
        "",
        "7",
        "seven-guitarist-revenge",
        "07-guitarist-revenge",
        "7-guitarist revenge",
        "7-guitarist-revenge; rm -rf /",
        "7-../../etc",
        "7-\u{202E}guitarist-revenge",
        &format!("7-{}", "a".repeat(300)),
    ] {
        let e = b.adapter.receive_code(code.to_owned()).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::BadCode, "{code:?}");
    }
    assert_eq!(mb.connections.load(Ordering::SeqCst), 0);

    // Well formed, but nobody holds the nameplate: a wrong code, not a new
    // mailbox.
    let e = b
        .adapter
        .receive_code("41-guitarist-revenge".into())
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadCode);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mistyped_code_fails_on_both_sides() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("hi".into())])
        .await
        .unwrap();
    let code = a.code().await;
    let nameplate = code.split('-').next().unwrap();
    let wrong = format!("{nameplate}-aardvark-adroitness");
    assert_ne!(wrong, code);
    let e = b.adapter.receive_code(wrong).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::BadCode);
    match a.finished(sent).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::BadCode),
        other => panic!("{other:?}"),
    }
    assert!(!b.was_asked());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unusable_server_urls_are_refused_at_use() {
    // Settings validation checks the scheme; the adapter checks the rest.
    let mut s = settings("ws://127.0.0.1:9/v1", "tcp://127.0.0.1:9");
    s.wormhole.relay_url = Some("tcp://127.0.0.1:9/path".into());
    let b = Side::with_settings(s.clone(), CONSENT);
    let e = b
        .adapter
        .receive_code("7-guitarist-revenge".into())
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadSettings);
    let e = b
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("x".into())])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadSettings);
    s.wormhole.relay_url = None;
    s.wormhole.mailbox_url = Some("ws://user:secret@127.0.0.1:9/v1".into());
    let b = Side::with_settings(s, CONSENT);
    let e = b
        .adapter
        .receive_code("7-guitarist-revenge".into())
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadSettings);
}

// ------------------------------------------- magic-wormhole's own v1 peer

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_librarys_v1_sender_reaches_sukkula() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let (mc, code) = peer_allocate(&mb.url).await;
    let data = content(123_457);
    let peer_data = data.clone();
    let relay_url = rl.url.clone();
    let peer = tokio::spawn(async move {
        let w = Wormhole::connect(mc).await.unwrap();
        let mut reader = std::io::Cursor::new(peer_data).compat();
        transfer::send_file(
            w,
            vec![relay_hint(&relay_url)],
            &mut reader,
            "report.pdf",
            123_457,
            Abilities::FORCE_RELAY,
            |_| {},
            |_, _| {},
            std::future::pending(),
        )
        .await
    });
    let rx = receive(&b, code);
    let (offer_id, offer) = b.pending().await;
    assert_eq!(offer.files[0].name.as_str(), "report.pdf");
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    peer.await.unwrap().unwrap();
    assert_eq!(b.received(), vec![("report.pdf".to_owned(), data)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_file_arrives_as_an_empty_file() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![file_item(&a, "empty.txt", b"")])
        .await
        .unwrap();
    let rx = receive(&b, a.code().await);
    let (offer_id, offer) = b.pending().await;
    assert_eq!(offer.total_bytes, 0);
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    assert_eq!(b.received(), vec![("empty.txt".to_owned(), Vec::new())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_engine_ends_every_wormhole_task() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, Duration::from_secs(60));
    // A send waiting for its receiver, on one side; on the other, a
    // receive waiting for the user.
    let lonely = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("anyone?".into())])
        .await
        .unwrap();
    a.code().await;
    let waiting = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("hi".into())])
        .await
        .unwrap();
    let code = loop {
        let c = a.log.lock().unwrap().events.iter().find_map(|e| match e {
            Event::WormholeCode { transfer, code, .. } if *transfer == waiting => {
                Some(code.clone())
            }
            _ => None,
        });
        if let Some(c) = c {
            break c;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let rx = receive(&b, code);
    b.pending().await;
    let start = tokio::time::Instant::now();
    a.ctx.shut_down();
    b.ctx.shut_down();
    assert_eq!(a.finished(lonely).await.0, Outcome::Cancelled);
    assert!(matches!(
        a.finished(waiting).await.0,
        Outcome::Cancelled | Outcome::Failed { .. }
    ));
    assert!(rx.await.unwrap().is_err());
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sukkula_reaches_the_librarys_v1_receiver() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let data = content(70_001);
    let sent = a
        .adapter
        .send(
            SendTarget::Wormhole,
            vec![file_item(&a, "notes.txt", &data)],
        )
        .await
        .unwrap();
    let code = a.code().await;
    let (mb_url, relay_url) = (mb.url.clone(), rl.url.clone());
    let peer = tokio::spawn(async move {
        let w = peer_connect(&mb_url, &code).await;
        let req = transfer::request_file(
            w,
            vec![relay_hint(&relay_url)],
            Abilities::FORCE_RELAY,
            std::future::pending(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(req.file_name(), "notes.txt");
        assert_eq!(req.file_size(), 70_001);
        let mut sink = Vec::new().compat_write();
        req.accept(|_| {}, |_, _| {}, &mut sink, std::future::pending())
            .await
            .unwrap();
        sink.into_inner()
    });
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    assert_eq!(peer.await.unwrap(), data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_from_the_library_arrives_as_one_unopened_tar() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let src = tempfile::tempdir().unwrap();
    let folder = src.path().join("holiday");
    std::fs::create_dir(&folder).unwrap();
    write_file(&folder, "a.jpg", &content(1000));
    write_file(&folder, "b.jpg", &content(2000));
    let (mc, code) = peer_allocate(&mb.url).await;
    let relay_url = rl.url.clone();
    let peer = tokio::spawn(async move {
        let w = Wormhole::connect(mc).await.unwrap();
        transfer::send_folder(
            w,
            vec![relay_hint(&relay_url)],
            folder,
            "holiday",
            Abilities::FORCE_RELAY,
            |_| {},
            |_, _| {},
            std::future::pending(),
        )
        .await
    });
    let rx = receive(&b, code);
    let (offer_id, offer) = b.pending().await;
    assert_eq!(offer.files[0].name.as_str(), "holiday.tar");
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    peer.await.unwrap().unwrap();
    // F-MW3: one file, the archive as sent; nothing unpacked.
    let files = b.received();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "holiday.tar");
    assert!(files[0].1.starts_with(b"holiday"), "a tar header");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zipped_directory_offer_is_saved_as_the_zip() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let (mc, code) = peer_allocate(&mb.url).await;
    let zip = [b"PK\x03\x04".as_slice(), &content(5000)].concat();
    let peer_zip = zip.clone();
    let relay_url = rl.url.clone();
    let peer = tokio::spawn(async move {
        let w = Wormhole::connect(mc).await.unwrap();
        scripted_send(
            w,
            &relay_url,
            json!({"offer": {"directory": {
                "dirname": "photos", "mode": "zipfile/deflated",
                "zipsize": peer_zip.len(), "numbytes": 99999, "numfiles": 3,
            }}}),
            Payload::Records(peer_zip.chunks(2048).map(<[u8]>::to_vec).collect()),
        )
        .await
    });
    let rx = receive(&b, code);
    let (offer_id, offer) = b.pending().await;
    assert_eq!(offer.files[0].name.as_str(), "photos.zip");
    assert_eq!(offer.total_bytes, 5004);
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    let (answer, ack) = peer.await.unwrap();
    assert_eq!(answer, json!({"answer": {"file_ack": "ok"}}));
    assert_eq!(ack.unwrap()["ack"], "ok");
    assert_eq!(b.received(), vec![("photos.zip".to_owned(), zip)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_that_listens_is_reached_directly() {
    // Sukkula never listens, but it connects out to a peer's direct hints
    // (through a transit guard that plays the relay for the library).
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let (mc, code) = peer_allocate(&mb.url).await;
    let data = content(33_333);
    let wire = data
        .chunks(16 * 1024)
        .map(|c| Wire::Record(c.to_vec()))
        .collect();
    let peer = tokio::spawn(async move {
        let w = Wormhole::connect(mc).await.unwrap();
        raw_send_via(
            w,
            Route::Direct,
            json!({"offer": {"file": {"filename": "direct.bin", "filesize": 33_333}}}),
            wire,
        )
        .await
    });
    let rx = receive(&b, code);
    let (offer_id, _) = b.pending().await;
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    assert_eq!(peer.await.unwrap(), json!({"answer": {"file_ack": "ok"}}));
    assert_eq!(b.received(), vec![("direct.bin".to_owned(), data)]);
    assert_eq!(
        rl.paired.load(Ordering::SeqCst),
        0,
        "the relay was not what carried it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_python_style_text_sender_gets_its_ack() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let (mc, code) = peer_allocate(&mb.url).await;
    // `wormhole send --text` sends no transit hints, only the offer.
    let peer = tokio::spawn(async move {
        let w = Wormhole::connect(mc).await.unwrap();
        scripted_offer(w, json!({"offer": {"message": "tervetuloa"}})).await
    });
    let rx = receive(&b, code);
    let (offer_id, _) = b.pending().await;
    b.answer(offer_id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    assert_eq!(
        peer.await.unwrap(),
        json!({"answer": {"message_ack": "ok"}})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_python_style_text_receiver_takes_our_text() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text("moi".into())])
        .await
        .unwrap();
    let code = a.code().await;
    let mut w = peer_connect(&mb.url, &code).await;
    assert_eq!(
        recv_json(&mut w).await,
        json!({"offer": {"message": "moi"}})
    );
    w.send_json(&json!({"answer": {"message_ack": "ok"}}))
        .await
        .unwrap();
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
}

// ------------------------------------------------ through the JSON contract

fn engine(dir: &std::path::Path) -> (Engine, Arc<std::sync::Mutex<Vec<Event>>>) {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l = log.clone();
    let sink: EventSink = Arc::new(move |e| l.lock().unwrap().push(e));
    let cfg = StartConfig {
        v: API_VERSION,
        data_dir: dir.join("data").to_string_lossy().into_owned(),
        download_dir: dir.join("dl").to_string_lossy().into_owned(),
        device_model: Some("Test".into()),
        allow_loopback: true,
    };
    (Engine::start(cfg, sink).unwrap(), log)
}

fn wait_event(log: &Arc<std::sync::Mutex<Vec<Event>>>, f: impl Fn(&Event) -> bool) -> Event {
    let start = std::time::Instant::now();
    loop {
        if let Some(e) = log.lock().unwrap().iter().find(|e| f(e)) {
            return e.clone();
        }
        assert!(start.elapsed() < DEADLINE, "no such event");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Engines own their runtime, so this test is synchronous; the fakes run
/// on a runtime of their own.
#[test]
fn engines_trade_a_text_through_the_json_contract() {
    let fakes = tokio::runtime::Runtime::new().unwrap();
    let mb = fakes.block_on(mailbox(Behaviour::default()));
    let rl = fakes.block_on(relay(RelayMode::Honest));
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let (ea, la) = engine(da.path());
    let (eb, lb) = engine(db.path());
    let set = format!(
        r#"{{"v":1,"id":1,"cmd":{{"type":"set_settings","settings":{{"wormhole":{{"mailbox_url":"{}","relay_url":"{}"}}}}}}}}"#,
        mb.url, rl.url
    );
    ea.command_json(&set);
    eb.command_json(&set);
    for log in [&la, &lb] {
        wait_event(log, |e| {
            matches!(
                e,
                Event::Reply {
                    id: 1,
                    ok: true,
                    ..
                }
            )
        });
    }
    ea.command_json(
        r#"{"v":1,"id":2,"cmd":{"type":"send","target":{"protocol":"wormhole"},"items":[{"kind":"text","text":"hei"}]}}"#,
    );
    let sent = match wait_event(&la, |e| matches!(e, Event::Reply { id: 2, .. })) {
        Event::Reply {
            ok: true,
            transfer: Some(t),
            ..
        } => t,
        other => panic!("{other:?}"),
    };
    let code = match wait_event(&la, |e| matches!(e, Event::WormholeCode { .. })) {
        Event::WormholeCode { code, .. } => code,
        _ => unreachable!(),
    };
    eb.command_json(&format!(
        r#"{{"v":1,"id":3,"cmd":{{"type":"receive_wormhole","code":"{code}"}}}}"#
    ));
    let offer = match wait_event(&lb, |e| matches!(e, Event::OfferPending { .. })) {
        Event::OfferPending { offer } => offer,
        _ => unreachable!(),
    };
    assert!(offer.has_text);
    // The receive's reply waits for the answer.
    assert!(
        !lb.lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::Reply { id: 3, .. }))
    );
    eb.command_json(&format!(
        r#"{{"v":1,"id":4,"cmd":{{"type":"answer","offer":{},"accept":true}}}}"#,
        offer.id
    ));
    let got = match wait_event(&lb, |e| matches!(e, Event::Reply { id: 3, .. })) {
        Event::Reply {
            ok: true,
            transfer: Some(t),
            ..
        } => t,
        other => panic!("{other:?}"),
    };
    match wait_event(&lb, |e| matches!(e, Event::TextReceived { .. })) {
        Event::TextReceived { transfer, text, .. } => {
            assert_eq!(transfer, got);
            assert_eq!(text, "hei");
        }
        _ => unreachable!(),
    }
    wait_event(
        &la,
        |e| matches!(e, Event::TransferFinished { transfer, outcome: Outcome::Done, .. } if *transfer == sent),
    );
    wait_event(
        &lb,
        |e| matches!(e, Event::TransferFinished { transfer, outcome: Outcome::Done, .. } if *transfer == got),
    );
    // Sukkula lists wormhole as send-only: it receives by code.
    let status = wait_event(&la, |e| matches!(e, Event::Receiving { .. }));
    let Event::Receiving { protocols, .. } = status else {
        unreachable!()
    };
    assert!(
        protocols
            .iter()
            .any(|p| p.protocol == sukkula_core::Protocol::Wormhole
                && p.state == sukkula_engine::api::ProtocolState::SendOnly)
    );
    ea.stop();
    eb.stop();
}
