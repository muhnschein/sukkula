//! Interop with the Python reference client (spec §7: "Sukkula to the
//! reference client on the same host"), offline, against the in-process
//! mailbox and relay.
//!
//! Ignored by a plain `cargo test`, which has no Python client. CI's
//! `wormhole-interop` job installs the pinned, hash-checked client from
//! `ci/wormhole-interop-requirements.txt` and runs these with
//! `--include-ignored` (`make wormhole-interop` does the same locally). By
//! hand:
//!
//! ```sh
//! python3 -m venv /tmp/mw && /tmp/mw/bin/pip install --require-hashes \
//!   -r ci/wormhole-interop-requirements.txt
//! SUKKULA_PY_WORMHOLE=/tmp/mw/bin/wormhole \
//!   cargo test -p sukkula-engine --test wormhole_interop -- --ignored
//! ```
//!
//! Checked with magic-wormhole 0.24.0 (Python). The client is pointed at
//! the fakes with `--relay-url` (the mailbox) and `--transit-helper`.

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

use std::io::{BufRead, BufReader};
use std::time::Duration;

use sukkula_core::name;
use sukkula_engine::adapter::{Outgoing, OutgoingFile};
use sukkula_engine::api::{Event, Outcome, SendTarget};
use wormhole_support::*;

const CONSENT: Duration = Duration::from_secs(20);

/// The Python client, from `SUKKULA_PY_WORMHOLE`.
fn python() -> String {
    std::env::var("SUKKULA_PY_WORMHOLE")
        .expect("set SUKKULA_PY_WORMHOLE to the Python `wormhole` executable")
}

/// Runs the reference client with the fakes' URLs and `args`.
///
/// S8 bans spawning processes in Sukkula; this is the test harness running
/// the reference client, never the app, so the ban is lifted here alone.
#[allow(clippy::disallowed_types, clippy::disallowed_methods)]
fn run_python(mailbox: &str, relay: &str, args: &[&str]) -> std::process::Child {
    let helper = relay.replace("tcp://", "tcp:");
    std::process::Command::new(python())
        .args(["--relay-url", mailbox, "--transit-helper", &helper])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the Python client did not start")
}

/// Reads the client's stderr until it prints its code.
fn code_of(child: &mut std::process::Child) -> String {
    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();
    for line in lines.by_ref() {
        let line = line.unwrap();
        if let Some(code) = line.trim().strip_prefix("Wormhole code is: ") {
            let code = code.to_owned();
            // Keep draining so the client never blocks on a full pipe.
            std::thread::spawn(move || for _ in lines {});
            return code;
        }
    }
    panic!("the Python client printed no code");
}

async fn wait_child(mut child: std::process::Child) -> (bool, String) {
    tokio::task::spawn_blocking(move || {
        let status = child.wait().unwrap();
        let mut out = String::new();
        if let Some(mut o) = child.stdout.take() {
            std::io::Read::read_to_string(&mut o, &mut out).unwrap();
        }
        (status.success(), out)
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the Python wormhole client (SUKKULA_PY_WORMHOLE)"]
async fn a_text_from_python_reaches_sukkula() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let mut child = run_python(&mb.url, &rl.url, &["send", "--text", "terve Pythonista"]);
    let code = tokio::task::spawn_blocking(move || {
        let c = code_of(&mut child);
        (c, child)
    })
    .await
    .unwrap();
    let (code, child) = code;
    let adapter = b.adapter.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(code).await });
    let (id, _) = b.pending().await;
    b.answer(id, true);
    let t = rx.await.unwrap().unwrap();
    match b.event(|e| matches!(e, Event::TextReceived { .. })).await {
        Event::TextReceived { text, .. } => assert_eq!(text, "terve Pythonista"),
        _ => unreachable!(),
    }
    assert_eq!(b.finished(t).await.0, Outcome::Done);
    assert!(
        wait_child(child).await.0,
        "the Python sender reported failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the Python wormhole client (SUKKULA_PY_WORMHOLE)"]
async fn a_file_from_python_reaches_sukkula() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let src = tempfile::tempdir().unwrap();
    let data = content(250_000);
    let path = write_file(src.path(), "from-python.bin", &data);
    let p = path.to_string_lossy().into_owned();
    let mut child = run_python(&mb.url, &rl.url, &["send", "--hide-progress", &p]);
    let (code, child) = tokio::task::spawn_blocking(move || {
        let c = code_of(&mut child);
        (c, child)
    })
    .await
    .unwrap();
    let adapter = b.adapter.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(code).await });
    let (id, offer) = b.pending().await;
    assert_eq!(offer.files[0].name.as_str(), "from-python.bin");
    b.answer(id, true);
    let t = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(t).await.0, Outcome::Done);
    assert!(
        wait_child(child).await.0,
        "the Python sender reported failure"
    );
    assert_eq!(b.received(), vec![("from-python.bin".to_owned(), data)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the Python wormhole client (SUKKULA_PY_WORMHOLE)"]
async fn a_folder_from_python_arrives_as_one_unopened_zip() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let src = tempfile::tempdir().unwrap();
    let folder = src.path().join("album");
    #[allow(clippy::disallowed_methods)] // The scene: a folder to send.
    std::fs::create_dir(&folder).unwrap();
    write_file(&folder, "one.txt", b"one");
    write_file(&folder, "two.txt", b"two");
    let p = folder.to_string_lossy().into_owned();
    let mut child = run_python(&mb.url, &rl.url, &["send", "--hide-progress", &p]);
    let (code, child) = tokio::task::spawn_blocking(move || {
        let c = code_of(&mut child);
        (c, child)
    })
    .await
    .unwrap();
    let adapter = b.adapter.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(code).await });
    let (id, offer) = b.pending().await;
    assert_eq!(offer.files[0].name.as_str(), "album.zip");
    b.answer(id, true);
    let t = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(t).await.0, Outcome::Done);
    assert!(wait_child(child).await.0);
    let files = b.received();
    assert_eq!(files.len(), 1, "one file, not a folder");
    assert_eq!(files[0].0, "album.zip");
    assert!(files[0].1.starts_with(b"PK\x03\x04"), "the zip, as sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the Python wormhole client (SUKKULA_PY_WORMHOLE)"]
async fn sukkula_sends_a_file_and_a_text_to_python() {
    let mb = mailbox(Behaviour::default()).await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);

    let data = content(180_000);
    let path = write_file(a.dir.path(), "to-python.bin", &data);
    let item = Outgoing::File(OutgoingFile {
        path,
        name: name::sanitize("to-python.bin"),
        size: 180_000,
        mime: None,
    });
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![item])
        .await
        .unwrap();
    let code = a.code().await;
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("got.bin");
    let t = target.to_string_lossy().into_owned();
    let child = run_python(
        &mb.url,
        &rl.url,
        &[
            "receive",
            "--accept-file",
            "--hide-progress",
            "--output-file",
            &t,
            &code,
        ],
    );
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    assert!(
        wait_child(child).await.0,
        "the Python receiver reported failure"
    );
    assert_eq!(std::fs::read(&target).unwrap(), data);

    let sent = a
        .adapter
        .send(
            SendTarget::Wormhole,
            vec![Outgoing::Text("hei Python".into())],
        )
        .await
        .unwrap();
    let code = loop {
        let codes: Vec<String> = a
            .log
            .lock()
            .unwrap()
            .events
            .iter()
            .filter_map(|e| match e {
                Event::WormholeCode { transfer, code, .. } if *transfer == sent => {
                    Some(code.clone())
                }
                _ => None,
            })
            .collect();
        if let Some(c) = codes.into_iter().next() {
            break c;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let child = run_python(&mb.url, &rl.url, &["receive", &code]);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    let (ok, stdout) = wait_child(child).await;
    assert!(ok);
    assert_eq!(stdout.trim(), "hei Python");
}
