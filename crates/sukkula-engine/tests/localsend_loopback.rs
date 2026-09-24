//! LocalSend, Sukkula to Sukkula and against the protocol authors' own
//! client and server, on loopback (spec §7 "Adapters").
//!
//! Every test uses ephemeral ports, no multicast, and pre-made identities
//! (see `localsend_support`).

#![cfg(feature = "localsend")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::as_conversions
)]

mod localsend_support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use localsend_support::*;
use sukkula_core::consent::{Closed, ConsentEvent};
use sukkula_engine::adapter::{Adapter, Outgoing};
use sukkula_engine::api::{ErrorCode, Event, Outcome, SendTarget};

fn target(peer: &str) -> SendTarget {
    SendTarget::LocalSend {
        peer: peer.to_owned(),
    }
}

fn failed_with(outcome: &Outcome) -> ErrorCode {
    match outcome {
        Outcome::Failed { error } => error.code,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_arrive_after_consent_and_match() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let peer = a.find(&b).await;
    assert_eq!(peer, format!("ls:{}", identity(1).fingerprint));
    a.wait_event("Bob listed", |e| match e {
        Event::PeerFound { peer } if peer.name == "Bob" => Some(()),
        _ => None,
    })
    .await;

    let src = tempfile::tempdir().unwrap();
    let big = make_file(src.path(), "photo.jpg", 3 * 1024 * 1024 + 7);
    let empty = make_file(src.path(), "empty.bin", 0);
    let small = make_file(src.path(), "notes.txt", 100);
    let id =
        a.ls.send(
            target(&peer),
            vec![outgoing(&big), outgoing(&empty), outgoing(&small)],
        )
        .await
        .unwrap();

    let (offer_id, offer) = b.offer().await;
    assert_eq!(offer.sender, "Alice");
    assert_eq!(offer.model.as_deref(), Some("Test Model"));
    assert_eq!(offer.files.len(), 3);
    assert_eq!(offer.total_bytes, 3 * 1024 * 1024 + 7 + 100);
    // S5: nothing is written before the answer.
    assert!(b.saved().is_empty());
    assert_eq!(b.partials(), 0);
    b.answer(offer_id, true);

    assert_eq!(a.finished(id).await.0, Outcome::Done);
    let incoming = b.incoming().await;
    let (outcome, mut saved) = b.finished(incoming).await;
    assert_eq!(outcome, Outcome::Done);
    saved.sort();
    assert_eq!(saved, vec!["empty.bin", "notes.txt", "photo.jpg"]);
    assert_eq!(b.saved(), saved);
    for name in &saved {
        assert_eq!(
            std::fs::read(b.downloads().join(name)).unwrap(),
            std::fs::read(src.path().join(name)).unwrap(),
            "{name}"
        );
    }
    assert_eq!(b.partials(), 0);

    // Progress reached the total on both sides.
    for node in [&a, &b] {
        let reached = node.events.lock().unwrap().iter().any(|e| {
            matches!(e, Event::TransferProgress { bytes, total, .. } if bytes == total && *total > 0)
        });
        assert!(reached);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_arrives_as_text_and_nothing_is_saved() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let peer = a.find(&b).await;
    let text = "hello \u{202E}world\u{200B} https://example.com".to_owned();
    let id =
        a.ls.send(target(&peer), vec![Outgoing::Text(text)])
            .await
            .unwrap();
    let (offer_id, offer) = b.offer().await;
    assert!(offer.text.is_some());
    assert!(offer.files.is_empty());
    b.answer(offer_id, true);
    assert_eq!(a.finished(id).await.0, Outcome::Done);
    let got = b
        .wait_event("the text", |e| match e {
            Event::TextReceived { from, text, .. } => Some((from.clone(), text.clone())),
            _ => None,
        })
        .await;
    // S2: shown without the bidi override and the zero-width space.
    assert_eq!(
        got,
        (
            "Alice".to_owned(),
            "hello world https://example.com".to_owned()
        )
    );
    assert!(b.saved().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_with_files_arrives_as_a_file() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let peer = a.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "a.bin", 10);
    let id =
        a.ls.send(
            target(&peer),
            vec![outgoing(&f), Outgoing::Text("note".into())],
        )
        .await
        .unwrap();
    let (offer_id, _) = b.offer().await;
    b.answer(offer_id, true);
    assert_eq!(a.finished(id).await.0, Outcome::Done);
    let (outcome, mut saved) = b.finished(b.incoming().await).await;
    assert_eq!(outcome, Outcome::Done);
    saved.sort();
    assert_eq!(saved, vec!["a.bin", "message-1.txt"]);
    assert_eq!(
        std::fs::read(b.downloads().join("message-1.txt")).unwrap(),
        b"note"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declined_offer_writes_nothing() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let peer = a.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "a.bin", 1000);
    let id = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    let (offer_id, _) = b.offer().await;
    b.answer(offer_id, false);
    assert_eq!(failed_with(&a.finished(id).await.0), ErrorCode::Refused);
    assert!(b.saved().is_empty());
    assert_eq!(b.partials(), 0);
    // Declined offers never became transfers on the receiving side.
    assert!(!b.events_json().contains("\"direction\":\"incoming\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_offer_times_out() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let mut cfg = NodeConfig::new(1, "Bob");
    cfg.consent_timeout = Duration::from_millis(500);
    let b = Node::new(&cfg);
    b.receive().await;
    let peer = a.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "a.bin", 10);
    let id = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    assert_eq!(failed_with(&a.finished(id).await.0), ErrorCode::Refused);
    assert!(b.consent.lock().unwrap().iter().any(|e| matches!(
        e,
        ConsentEvent::Closed {
            reason: Closed::TimedOut,
            ..
        }
    )));
    assert!(b.saved().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_sender_is_told_busy_and_the_first_goes_through() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let c = Node::new(&NodeConfig::new(2, "Carol"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let from_a = a.find(&b).await;
    let from_c = c.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "a.bin", 10);
    let first =
        a.ls.send(target(&from_a), vec![outgoing(&f)])
            .await
            .unwrap();
    let (offer_id, offer) = b.offer().await;
    assert_eq!(offer.sender, "Alice");
    let second =
        c.ls.send(target(&from_c), vec![outgoing(&f)])
            .await
            .unwrap();
    assert_eq!(failed_with(&c.finished(second).await.0), ErrorCode::Refused);
    assert_eq!(b.offers_shown(), 1, "the busy offer never reached the user");
    b.answer(offer_id, true);
    assert_eq!(a.finished(first).await.0, Outcome::Done);
    assert_eq!(b.saved(), vec!["a.bin"]);
}

/// A sparse file big enough that a cancel lands mid-transfer.
fn big_sparse(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("big.iso");
    #[allow(clippy::disallowed_methods)] // The scene: a file to send.
    std::fs::File::create(&path)
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receiver_cancel_mid_transfer_leaves_no_partial_file() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    a.receive().await; // so Bob's cancel can reach Alice
    b.receive().await;
    let peer = a.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let f = big_sparse(src.path());
    let id = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    let (offer_id, _) = b.offer().await;
    b.answer(offer_id, true);
    let incoming = b.incoming().await;
    b.wait_event("progress", |e| match e {
        Event::TransferProgress {
            transfer, bytes, ..
        } if *transfer == incoming && *bytes > 0 => Some(()),
        _ => None,
    })
    .await;
    assert!(b.ctx.transfers().cancel(incoming));
    assert_eq!(b.finished(incoming).await.0, Outcome::Cancelled);
    // The sender hears of it: through our POST /cancel to its server.
    assert_eq!(a.finished(id).await.0, Outcome::Cancelled);
    assert!(b.saved().is_empty());
    assert_eq!(b.partials(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sender_cancel_mid_transfer_leaves_no_partial_file() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let peer = a.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let f = big_sparse(src.path());
    let id = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    let (offer_id, _) = b.offer().await;
    b.answer(offer_id, true);
    a.wait_event("progress", |e| match e {
        Event::TransferProgress {
            transfer, bytes, ..
        } if *transfer == id && *bytes > 0 => Some(()),
        _ => None,
    })
    .await;
    assert!(a.ctx.transfers().cancel(id));
    assert_eq!(a.finished(id).await.0, Outcome::Cancelled);
    let (outcome, saved) = b.finished(b.incoming().await).await;
    assert_ne!(outcome, Outcome::Done);
    assert!(saved.is_empty());
    assert!(b.saved().is_empty());
    wait("staging to be empty", || (b.partials() == 0).then_some(())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sender_cancel_while_pending_withdraws_the_offer() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let peer = a.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "a.bin", 10);
    let id = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    let (offer_id, _) = b.offer().await;
    assert!(a.ctx.transfers().cancel(id));
    assert_eq!(a.finished(id).await.0, Outcome::Cancelled);
    wait("the offer to be withdrawn", || {
        b.consent
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, ConsentEvent::Closed { id, reason: Closed::Withdrawn } if *id == offer_id))
            .then_some(())
    })
    .await;
    // The slot is free again: the next offer is shown.
    let again = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    let (second, _) = b.offer_after(1).await;
    b.answer(second, true);
    assert_eq!(a.finished(again).await.0, Outcome::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_certificate_is_refused_before_anything_is_sent() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    let port = b.receive().await;
    let peer = a.find(&b).await;
    b.ls.stop_receiving().await;

    // Someone else takes Bob's address and port: a different certificate.
    let mut cfg = NodeConfig::new(2, "Mallory");
    cfg.port = port;
    let m = Node::new(&cfg);
    assert_eq!(m.receive().await, port);

    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "secret.pdf", 10);
    let id = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    assert_eq!(
        failed_with(&a.finished(id).await.0),
        ErrorCode::PeerMismatch
    );
    // F-LS3: the handshake failed, so not even the offer reached Mallory.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(m.offers_shown(), 0);
    assert!(!m.events_json().contains("secret"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_and_stop_are_idempotent_and_release_the_port() {
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    let port = b.receive().await;
    b.ls.start_receiving().await.unwrap();
    assert_eq!(b.ls.port().await, Some(port));
    b.ls.start_discovery().await.unwrap();
    b.ls.start_discovery().await.unwrap();
    b.ls.stop_discovery().await;
    b.ls.stop_discovery().await;
    b.ls.stop_receiving().await;
    b.ls.stop_receiving().await;
    assert_eq!(b.ls.port().await, None);
    // The listener is gone: the port can be bound again, and nothing answers.
    let rebound = tokio::net::TcpListener::bind(("127.0.0.1", port)).await;
    assert!(rebound.is_ok());
    drop(rebound);
    let again = b.receive().await;
    assert!(raw_tls(again, Some(3), None).await.is_ok());
    b.ls.stop_receiving().await;
    // Engine stop: everything the adapter started ends.
    b.ctx.shut_down();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_lists_registrations_and_forgets_on_stop() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    a.receive().await;
    b.receive().await;
    // Bob registers with Alice (the answer to her announcement); Alice,
    // discovering, lists him from that registration alone.
    a.ls.start_discovery().await.unwrap();
    b.ls.start_discovery().await.unwrap();
    b.ls.discover_at(a.addr().await).await.unwrap();
    let listed = a
        .wait_event("Bob listed by his registration", |e| match e {
            Event::PeerFound { peer } => Some(peer.clone()),
            _ => None,
        })
        .await;
    assert_eq!(listed.name, "Bob");
    assert_eq!(listed.id, format!("ls:{}", identity(1).fingerprint));
    a.ls.stop_discovery().await;
    a.wait_event("Bob forgotten", |e| match e {
        Event::PeerLost { peer } if *peer == listed.id => Some(()),
        _ => None,
    })
    .await;
    let e =
        a.ls.send(target(&listed.id), vec![Outgoing::Text("x".into())])
            .await
            .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_that_changed_after_it_was_chosen_is_not_sent() {
    use sukkula_engine::adapter::OutgoingFile;
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    b.receive().await;
    let peer = a.find(&b).await;
    let src = tempfile::tempdir().unwrap();
    let path = make_file(src.path(), "log.txt", 1000);
    let checked_at = |path: &std::path::Path, size: u64| {
        Outgoing::File(OutgoingFile {
            path: path.to_path_buf(),
            name: sukkula_core::name::sanitize("log.txt"),
            size,
            mime: None,
        })
    };
    let fifo = src.path().join("fifo");
    #[allow(clippy::disallowed_methods)] // The scene: a FIFO where a file was.
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .unwrap();

    // It grew, it shrank, it became a FIFO (whose open would wait for a
    // writer forever): the handle read from is not what was checked.
    let changed = [
        checked_at(&path, 600),
        checked_at(&path, 2000),
        checked_at(&fifo, 0),
    ];
    for (n, item) in changed.into_iter().enumerate() {
        let id = a.ls.send(target(&peer), vec![item]).await.unwrap();
        let (offer, _) = b.offer_after(n).await;
        b.answer(offer, true);
        assert_eq!(
            failed_with(&a.finished(id).await.0),
            ErrorCode::BadFile,
            "{n}"
        );
        // The receiver is told at once, not left to its idle timeout.
        let (outcome, saved) = b.finished(b.nth_incoming(n).await).await;
        assert_eq!(outcome, Outcome::Cancelled, "{n}");
        assert!(saved.is_empty());
    }
    // (Three offers is this address's budget for the minute, so the file
    // as checked is sent in the other tests.)
    assert!(b.saved().is_empty());
    wait("staging to be empty", || (b.partials() == 0).then_some(())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_task_ends_when_the_engine_stops() {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let b = Node::new(&NodeConfig::new(1, "Bob"));
    a.receive().await;
    b.receive().await;
    let peer = a.find(&b).await;
    b.ls.start_discovery().await.unwrap();
    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "a.bin", 10);
    // One transfer done, one left waiting for Bob's answer.
    let done = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    let (offer, _) = b.offer().await;
    b.answer(offer, true);
    assert_eq!(a.finished(done).await.0, Outcome::Done);
    let waiting = a.ls.send(target(&peer), vec![outgoing(&f)]).await.unwrap();
    b.offer_after(1).await;
    assert!(a.ls.tasks_running() > 0 && b.ls.tasks_running() > 0);

    // What `Engine::stop` does: stop the adapters, then shut the context down.
    for node in [&a, &b] {
        node.ls.stop_discovery().await;
        node.ls.stop_receiving().await;
        node.ctx.shut_down();
    }
    assert_ne!(a.finished(waiting).await.0, Outcome::Done);
    wait("every task to end", || {
        (a.ls.tasks_running() == 0 && b.ls.tasks_running() == 0).then_some(())
    })
    .await;
    assert!(b.saved() == ["a.bin"]);
    assert_eq!(b.partials(), 0);
}

// ------------------------------------------------ the reference implementation

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_client_sends_to_sukkula() {
    use localsend::http::client::LsHttpClientV2;
    use localsend::http::dto_v2::{PrepareUploadRequestDtoV2, RegisterDtoV2};
    use localsend::model::discovery::ProtocolType;
    use localsend::model::transfer::FileDto;

    let b = Node::new(&NodeConfig::new(1, "Bob"));
    let port = b.receive().await;
    let me = identity(3);
    let client = LsHttpClientV2::try_new(
        &me.private_key_pem,
        &me.certificate_pem,
        Some(identity(1).fingerprint.clone()),
        None,
    )
    .unwrap();
    let info = RegisterDtoV2 {
        alias: "Reference".into(),
        version: "2.2".into(),
        device_model: Some("Rust".into()),
        device_type: None,
        fingerprint: me.fingerprint.clone(),
        port: 1,
        protocol: ProtocolType::Https,
        download: false,
    };
    let reg = client
        .register(ProtocolType::Https, "127.0.0.1", port, info.clone())
        .await
        .unwrap();
    assert_eq!(reg.body.alias, "Bob");
    assert_eq!(
        reg.cert_fingerprint.as_deref(),
        Some(identity(1).fingerprint.as_str())
    );

    let src = tempfile::tempdir().unwrap();
    let path = make_file(src.path(), "ref.bin", 200_000);
    let files = HashMap::from([(
        "f1".to_owned(),
        FileDto {
            id: "f1".into(),
            file_name: "ref.bin".into(),
            size: 200_000,
            file_type: "application/octet-stream".into(),
            sha256: Some(localsend::crypto::hash::sha256_hex(
                &std::fs::read(&path).unwrap(),
            )),
            preview: None,
            metadata: None,
        },
    )]);
    let prepare = client.prepare_upload(
        ProtocolType::Https,
        "127.0.0.1",
        port,
        None,
        PrepareUploadRequestDtoV2 { info, files },
        None,
        tokio_util::sync::CancellationToken::new(),
    );
    let answer = async {
        let (id, offer) = b.offer().await;
        assert_eq!(offer.sender, "Reference");
        b.answer(id, true);
    };
    let (result, ()) = tokio::join!(prepare, answer);
    let response = result.unwrap().response.unwrap();
    client
        .upload(
            ProtocolType::Https,
            "127.0.0.1",
            port,
            None,
            &response.session_id,
            "f1",
            &response.files["f1"],
            localsend::reqwest::Body::from(std::fs::read(&path).unwrap()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let (outcome, saved) = b.finished(b.incoming().await).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved, vec!["ref.bin"]);
    assert_eq!(
        std::fs::read(b.downloads().join("ref.bin")).unwrap(),
        std::fs::read(&path).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sukkula_sends_to_the_upstream_server() {
    use localsend::http::server::common::save::FileUploadTarget;
    use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2};
    use localsend::http::server::web::WebConfig;
    use localsend::http::server::{ServerConfigV2, TlsConfig, start_with_port};
    use localsend::http::state::ClientInfo;
    use tokio::sync::{mpsc, oneshot};

    let id = identity(4);
    let (event_tx, mut event_rx) = mpsc::channel::<ServerEventV2>(16);
    let received: Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
    let offered: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let (rec, off) = (received.clone(), offered.clone());
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                ServerEventV2::PrepareUpload {
                    files,
                    info,
                    decision_tx,
                    ..
                } => {
                    off.lock().unwrap().push(info.alias.clone());
                    let ids: HashSet<String> = files.keys().cloned().collect();
                    let _ = decision_tx.send(PrepareUploadDecisionV2::Accept(ids));
                }
                ServerEventV2::FileUpload {
                    file, target_tx, ..
                } => {
                    let (binary_tx, mut binary_rx) = mpsc::channel(16);
                    let (result_tx, result_rx) = oneshot::channel();
                    let _ = target_tx.send(FileUploadTarget::Stream {
                        binary_tx,
                        result_rx,
                    });
                    let rec = rec.clone();
                    tokio::spawn(async move {
                        let mut bytes = Vec::new();
                        while let Some(chunk) = binary_rx.recv().await {
                            bytes.extend_from_slice(&chunk);
                        }
                        rec.lock().unwrap().insert(file.file_name.clone(), bytes);
                        let _ = result_tx.send(Ok(()));
                    });
                }
                _ => {}
            }
        }
    });
    let (_stop_tx, stop_rx) = oneshot::channel::<()>();
    let server = start_with_port(
        0,
        Some(TlsConfig {
            cert: id.certificate_pem.clone(),
            private_key: id.private_key_pem.clone(),
        }),
        ClientInfo {
            alias: "Reference Server".into(),
            version: "2.2".into(),
            device_model: None,
            device_type: None,
            token: id.fingerprint.clone(),
        },
        None,
        Some(ServerConfigV2 {
            pin: None,
            verify_checksums: true,
            event_tx,
        }),
        WebConfig::default(),
        stop_rx,
    )
    .await
    .unwrap();

    let a = Node::new(&NodeConfig::new(0, "Alice"));
    a.ls.start_discovery().await.unwrap();
    let peer =
        a.ls.discover_at(std::net::SocketAddr::new(
            std::net::Ipv4Addr::LOCALHOST.into(),
            server.port(),
        ))
        .await
        .unwrap();
    assert_eq!(peer, format!("ls:{}", id.fingerprint));
    let src = tempfile::tempdir().unwrap();
    let f = make_file(src.path(), "to-reference.bin", 500_000);
    let t =
        a.ls.send(
            target(&peer),
            vec![outgoing(&f), Outgoing::Text("hi".into())],
        )
        .await
        .unwrap();
    assert_eq!(a.finished(t).await.0, Outcome::Done);
    assert_eq!(offered.lock().unwrap().as_slice(), ["Alice"]);
    let received = received.lock().unwrap();
    assert_eq!(received["to-reference.bin"], std::fs::read(&f).unwrap());
    assert_eq!(received["message-1.txt"], b"hi");
}
