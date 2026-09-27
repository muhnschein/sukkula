//! Interop with croc itself (spec §7: "Sukkula to the reference client on
//! the same host"), offline: croc's Go binary as the relay and as the
//! other peer, on loopback. `SUKKULA_CROC` is that binary: croc v11.5.4,
//! or v10.7.0, the suite runs against either. `SUKKULA_CROC10`, if set, is
//! croc v10.7.0 too, for croc 10 peers on the first binary's relay (the
//! public relays run croc 11 now, and croc 10 clients still use them).
//!
//! Ignored by a plain `cargo test`, which has no croc. CI's `croc-interop`
//! job builds both pinned commits with `-mod=readonly`, every module the
//! one croc's `go.sum` names, and runs these with `--include-ignored`,
//! once with each as `SUKKULA_CROC`; `make croc-interop` does the same
//! locally. With croc binaries at hand:
//!
//! ```sh
//! SUKKULA_CROC=/path/to/croc11 SUKKULA_CROC10=/path/to/croc10 \
//!   cargo test -p sukkula-engine --test croc_interop -- --ignored
//! ```
//!
//! The Go client is always given the code in `CROC_SECRET` (the only way
//! it takes one on Linux) and the relay with `--relay`, so it never looks
//! for croc's public relay. Senders start first, as croc's users do.

#![cfg(feature = "croc")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unreachable
)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sukkula_core::config::Settings;
use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision};
use sukkula_core::inbox::Inbox;
use sukkula_core::name;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_engine::adapter::{Adapter, Outgoing, OutgoingFile};
use sukkula_engine::api::{ErrorCode, Event, Outcome, SendTarget, TransferId};
use sukkula_engine::croc::{self, Tuning};
use sukkula_engine::ctx::Ctx;
use tokio::sync::mpsc;

/// The Go client, from `SUKKULA_CROC`.
fn croc_bin() -> String {
    std::env::var("SUKKULA_CROC").expect("set SUKKULA_CROC to croc's executable")
}

/// croc 10's Go client, from `SUKKULA_CROC10`, if set.
fn croc10_bin() -> Option<String> {
    std::env::var("SUKKULA_CROC10").ok()
}

/// `n` ports for a relay. Asking the system for free ones and letting them
/// go raced: until croc had bound them, another test's relay could be
/// given the same ones, or an outgoing connection one as its own. So they
/// come from below Linux's ephemeral range (32768 up), where no outgoing
/// connection is given a port; from a counter, so no two tests here share
/// one; from a block of the process's own, by its pid, for another test
/// binary running at once; and each is checked free.
fn free_ports(n: usize) -> Vec<u16> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let base = 20_000 + (std::process::id() % 100) * 100;
    let mut out = Vec::new();
    while out.len() < n {
        let k = NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(k < 100, "out of test ports");
        let port = u16::try_from(base + k).unwrap();
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            out.push(port);
        }
    }
    out
}

/// Runs croc `bin` with `args` and `env`, in `dir`.
///
/// S8 bans spawning processes in Sukkula; this is the test harness running
/// the reference client, never the app, so the ban is lifted here alone.
#[allow(clippy::disallowed_types, clippy::disallowed_methods)]
fn run(bin: &str, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Child {
    std::process::Command::new(bin)
        .current_dir(dir)
        .env("CROC_CONFIG_DIR", dir.join("croc-config"))
        .env("HOME", dir)
        .envs(env.iter().copied())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("croc did not start")
}

/// croc's relay on loopback, with `password`.
struct GoRelay {
    child: Child,
    port: u16,
    password: String,
    _dir: tempfile::TempDir,
}

impl Drop for GoRelay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn go_relay(password: &str) -> GoRelay {
    let dir = tempfile::tempdir().unwrap();
    let ports = free_ports(5);
    let list = ports
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let child = run(
        &croc_bin(),
        dir.path(),
        &[],
        &[
            "--pass",
            password,
            "relay",
            "--host",
            "127.0.0.1",
            "--ports",
            &list,
        ],
    );
    // Up once every port answers: its data rooms are joined on the others.
    for port in &ports {
        let mut up = false;
        for _ in 0..300 {
            if tokio::net::TcpStream::connect(("127.0.0.1", *port))
                .await
                .is_ok()
            {
                up = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(up, "the croc relay did not come up on port {port}");
    }
    GoRelay {
        child,
        port: ports[0],
        password: password.to_owned(),
        _dir: dir,
    }
}

impl GoRelay {
    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

/// A Sukkula side on a bare `Ctx`.
struct Side {
    ctx: Arc<Ctx>,
    adapter: Arc<dyn Adapter>,
    events: Arc<Mutex<Vec<Event>>>,
    offers: mpsc::UnboundedReceiver<u64>,
    dir: tempfile::TempDir,
}

impl Side {
    fn new(relay: &GoRelay) -> Side {
        let dir = tempfile::tempdir().unwrap();
        let (tx, offers) = mpsc::unbounded_channel();
        let consent = ConsentBroker::new(Arc::new(move |e| {
            if let ConsentEvent::Pending { id, .. } = e {
                let _ = tx.send(id);
            }
        }));
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let e = events.clone();
        let mut settings = Settings::default();
        settings.croc.relay = Some(relay.address());
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
        let adapter = croc::adapter_with(
            ctx.clone(),
            Tuning {
                handshake: Duration::from_secs(15),
                idle: Duration::from_secs(15),
                peer_wait: Duration::from_secs(30),
                answer_wait: Duration::from_secs(30),
            },
        );
        Side {
            ctx,
            adapter,
            events,
            offers,
            dir,
        }
    }

    /// The first event `f` picks, within 30 s; else a panic naming `what`
    /// and every event there was.
    async fn event<T>(&self, what: &str, f: impl Fn(&Event) -> Option<T>) -> T {
        for _ in 0..600 {
            if let Some(t) = self.events.lock().unwrap().iter().find_map(&f) {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no {what} in {:#?}", self.events.lock().unwrap());
    }

    async fn code(&self, id: TransferId) -> String {
        self.event("code", |e| match e {
            Event::CrocCode { transfer, code } if *transfer == id => Some(code.clone()),
            _ => None,
        })
        .await
    }

    async fn outcome(&self, id: TransferId) -> (Outcome, Vec<String>) {
        self.event("outcome", |e| match e {
            Event::TransferFinished {
                transfer,
                outcome,
                saved,
            } if *transfer == id => Some((outcome.clone(), saved.clone())),
            _ => None,
        })
        .await
    }

    async fn accept_next(&mut self) {
        let id = tokio::time::timeout(Duration::from_secs(30), self.offers.recv())
            .await
            .expect("no offer")
            .unwrap();
        assert!(self.ctx.consent().answer(id, Decision::Accept));
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

#[allow(clippy::disallowed_methods)] // The test's own scratch files.
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

#[allow(clippy::disallowed_methods)] // The test's own scratch files.
fn mkdir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap() ^ seed)
        .collect()
}

async fn finish(mut child: Child) -> (bool, String, String) {
    tokio::task::spawn_blocking(move || {
        let status = child.wait().unwrap();
        let (mut out, mut err) = (String::new(), String::new());
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        (status.success(), out, err)
    })
    .await
    .unwrap()
}

const CODE: &str = "8123-alpha-bravo-charlie";

/// Starts `bin` sending `args`, with croc's global options `global`, and
/// gives it time to be in the room first.
async fn go_send(bin: &str, dir: &Path, relay: &GoRelay, global: &[&str], args: &[&str]) -> Child {
    let mut all = vec![
        "--ignore-stdin",
        "--disable-clipboard",
        "--pass",
        &relay.password,
        "--relay",
    ];
    let address = relay.address();
    all.push(&address);
    all.extend_from_slice(global);
    all.push("send");
    all.push("--no-local");
    all.extend_from_slice(args);
    let child = run(bin, dir, &[("CROC_SECRET", CODE)], &all);
    tokio::time::sleep(Duration::from_secs(2)).await;
    child
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC)"]
async fn files_from_croc_reach_sukkula() {
    let relay = go_relay("pass123").await;
    let mut b = Side::new(&relay);
    let dir = tempfile::tempdir().unwrap();
    let big = pattern(1_000_003, 3);
    write(&dir.path().join("big.bin"), &big);
    write(&dir.path().join("empty.txt"), b"");
    mkdir(&dir.path().join("folder"));
    write(&dir.path().join("folder/inner.txt"), b"inside");
    let child = go_send(
        &croc_bin(),
        dir.path(),
        &relay,
        &[],
        &["big.bin", "empty.txt", "folder"],
    )
    .await;
    let adapter = b.adapter.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(CODE.into()).await });
    b.accept_next().await;
    let id = rx.await.unwrap().unwrap();
    let (outcome, mut saved) = b.outcome(id).await;
    assert_eq!(outcome, Outcome::Done);
    saved.sort();
    assert_eq!(saved, vec!["big.bin", "empty.txt", "inner.txt"]);
    assert_eq!(b.received("big.bin"), big);
    assert_eq!(b.received("inner.txt"), b"inside");
    let (ok, _, err) = finish(child).await;
    assert!(ok, "croc send failed: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC)"]
async fn uncompressed_single_room_files_from_croc() {
    let relay = go_relay("other-pass").await;
    let mut b = Side::new(&relay);
    let dir = tempfile::tempdir().unwrap();
    let data = pattern(200_000, 9);
    write(&dir.path().join("a.bin"), &data);
    // Chunks not deflated, and all through the first room.
    let child = go_send(
        &croc_bin(),
        dir.path(),
        &relay,
        &["--no-compress"],
        &["--no-multi", "a.bin"],
    )
    .await;
    let adapter = b.adapter.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(CODE.into()).await });
    b.accept_next().await;
    let id = rx.await.unwrap().unwrap();
    assert_eq!(b.outcome(id).await.0, Outcome::Done);
    assert_eq!(b.received("a.bin"), data);
    assert!(finish(child).await.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC)"]
async fn a_text_from_croc_reaches_sukkula() {
    let relay = go_relay("pass123").await;
    let mut b = Side::new(&relay);
    let dir = tempfile::tempdir().unwrap();
    let child = go_send(
        &croc_bin(),
        dir.path(),
        &relay,
        &[],
        &["--text", "terve croc <b>x</b>"],
    )
    .await;
    let adapter = b.adapter.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(CODE.into()).await });
    b.accept_next().await;
    let id = rx.await.unwrap().unwrap();
    assert_eq!(b.outcome(id).await.0, Outcome::Done);
    let text = b
        .event("text", |e| match e {
            Event::TextReceived { text, .. } => Some(text.clone()),
            _ => None,
        })
        .await;
    assert_eq!(text, "terve croc <b>x</b>");
    assert!(finish(child).await.0);
}

/// Runs `bin` as a receiver of `code` into `out`, with croc's global
/// options `extra`.
fn go_receive(
    bin: &str,
    dir: &Path,
    relay: &GoRelay,
    code: &str,
    out: &Path,
    yes: bool,
    extra: &[&str],
) -> Child {
    let address = relay.address();
    let out = out.to_str().unwrap().to_owned();
    let mut args = vec![
        "--pass",
        &relay.password,
        "--relay",
        &address,
        "--out",
        &out,
    ];
    if yes {
        args.insert(0, "--overwrite");
        args.insert(0, "--yes");
    }
    args.extend_from_slice(extra);
    run(bin, dir, &[("CROC_SECRET", code)], &args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC)"]
async fn files_from_sukkula_reach_croc() {
    let relay = go_relay("pass123").await;
    let a = Side::new(&relay);
    let big = pattern(700_001, 5);
    let items = vec![
        a.file("big.bin", &big),
        a.file("small.txt", b"small"),
        a.file("empty.txt", b""),
    ];
    let id = a.adapter.send(SendTarget::Croc, items).await.unwrap();
    let code = a.code(id).await;
    let dir = tempfile::tempdir().unwrap();
    let out: PathBuf = dir.path().join("out");
    mkdir(&out);
    let child = go_receive(&croc_bin(), dir.path(), &relay, &code, &out, true, &[]);
    let (ok, _, err) = finish(child).await;
    assert!(ok, "croc receive failed: {err}");
    assert_eq!(std::fs::read(out.join("big.bin")).unwrap(), big);
    assert_eq!(std::fs::read(out.join("small.txt")).unwrap(), b"small");
    assert_eq!(std::fs::read(out.join("empty.txt")).unwrap(), b"");
    assert_eq!(a.outcome(id).await.0, Outcome::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC)"]
async fn a_text_from_sukkula_reaches_croc() {
    let relay = go_relay("pass123").await;
    let a = Side::new(&relay);
    let id = a
        .adapter
        .send(
            SendTarget::Croc,
            vec![Outgoing::Text("hei croc, täältä Sukkula".into())],
        )
        .await
        .unwrap();
    let code = a.code(id).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    mkdir(&out);
    let child = go_receive(&croc_bin(), dir.path(), &relay, &code, &out, true, &[]);
    let (ok, stdout, err) = finish(child).await;
    assert!(ok, "croc receive failed: {err}");
    assert_eq!(stdout, "hei croc, täältä Sukkula");
    assert_eq!(a.outcome(id).await.0, Outcome::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC)"]
async fn croc_declining_is_said_to_sukkula() {
    let relay = go_relay("pass123").await;
    let a = Side::new(&relay);
    let id = a
        .adapter
        .send(SendTarget::Croc, vec![a.file("a.txt", b"abc")])
        .await
        .unwrap();
    let code = a.code(id).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    mkdir(&out);
    // No --yes and nothing on stdin: croc refuses.
    let child = go_receive(&croc_bin(), dir.path(), &relay, &code, &out, false, &[]);
    let (ok, _, _) = finish(child).await;
    assert!(!ok);
    match a.outcome(id).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Refused),
        other => panic!("{other:?}"),
    }
}

/// A croc receiver on croc's own curve (`--curve siec`): its LAN probe and
/// its key exchange are answered on that curve.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC)"]
async fn a_croc_receiver_on_siec_is_answered_on_siec() {
    let relay = go_relay("pass123").await;
    let a = Side::new(&relay);
    let id = a
        .adapter
        .send(SendTarget::Croc, vec![a.file("s.txt", b"on siec")])
        .await
        .unwrap();
    let code = a.code(id).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    mkdir(&out);
    let child = go_receive(
        &croc_bin(),
        dir.path(),
        &relay,
        &code,
        &out,
        true,
        &["--curve", "siec"],
    );
    let (ok, _, err) = finish(child).await;
    assert!(ok, "croc receive failed: {err}");
    assert_eq!(std::fs::read(out.join("s.txt")).unwrap(), b"on siec");
    assert_eq!(a.outcome(id).await.0, Outcome::Done);
}

/// A croc 10 receiver on the relay of `SUKKULA_CROC` takes our code: it
/// reads it as croc 11 does, and is answered croc 10's way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC, SUKKULA_CROC10)"]
async fn a_croc10_receiver_takes_our_code() {
    let Some(croc10) = croc10_bin() else {
        eprintln!("SUKKULA_CROC10 is not set: skipped");
        return;
    };
    let relay = go_relay("pass123").await;
    let a = Side::new(&relay);
    let data = pattern(100_000, 11);
    let id = a
        .adapter
        .send(SendTarget::Croc, vec![a.file("old.bin", &data)])
        .await
        .unwrap();
    let code = a.code(id).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    mkdir(&out);
    let child = go_receive(&croc10, dir.path(), &relay, &code, &out, true, &[]);
    let (ok, _, err) = finish(child).await;
    assert!(ok, "croc 10 receive failed: {err}");
    assert_eq!(std::fs::read(out.join("old.bin")).unwrap(), data);
    assert_eq!(a.outcome(id).await.0, Outcome::Done);
}

/// A croc 10 sender on the relay of `SUKKULA_CROC` reaches us: its answer
/// to our key exchange says it is croc 10's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs croc (SUKKULA_CROC, SUKKULA_CROC10)"]
async fn a_croc10_sender_reaches_sukkula() {
    let Some(croc10) = croc10_bin() else {
        eprintln!("SUKKULA_CROC10 is not set: skipped");
        return;
    };
    let relay = go_relay("pass123").await;
    let mut b = Side::new(&relay);
    let dir = tempfile::tempdir().unwrap();
    let data = pattern(100_000, 13);
    write(&dir.path().join("old.bin"), &data);
    let child = go_send(&croc10, dir.path(), &relay, &[], &["old.bin"]).await;
    let adapter = b.adapter.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(CODE.into()).await });
    b.accept_next().await;
    let id = rx.await.unwrap().unwrap();
    assert_eq!(b.outcome(id).await.0, Outcome::Done);
    assert_eq!(b.received("old.bin"), data);
    let (ok, _, err) = finish(child).await;
    assert!(ok, "croc 10 send failed: {err}");
}
