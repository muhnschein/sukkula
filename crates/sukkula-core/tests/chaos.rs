//! Kill-during-write chaos, after clove's `ci/chaos.sh`: the promise that a
//! crash never leaves a torn or orphaned file is worth nothing untested.
//!
//! A child process -- this test binary, run again with [`ROLE`] set --
//! writes in a loop: the settings and the key through `Store::write`, or
//! received files through the inbox, both with staging next to the target
//! (placing is a link) and on another file system (placing is the copy
//! fallback). The parent SIGKILLs it at a varying moment, many times; the
//! low-memory killer does the same to a backgrounded app. After every kill
//! the next open must leave:
//!
//! - every file under a real name whole: the old version or the new one,
//!   never a mix and never short, and never a partial received file;
//! - no temporary, staging or copy file, which the open sweeps.
//!
//! A kill that only ever hit the child between writes would prove nothing,
//! so each kill is checked to have killed the child (not a child that had
//! already died), and the kills that left work in progress for the sweep
//! are counted: each part requires some, as clove requires its storm to
//! land.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::print_stdout,
    clippy::print_stderr,
    // The parent sets the scene and cleans up with plain file calls.
    clippy::disallowed_methods
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sukkula_core::inbox::{Inbox, STAGING_DIR};
use sukkula_core::name::sanitize;
use sukkula_core::store::Store;

/// Which loop the re-run binary runs: `store` or `inbox`.
const ROLE: &str = "SUKKULA_CHAOS_ROLE";
/// The directory it writes to.
const DIR: &str = "SUKKULA_CHAOS_DIR";
/// The inbox's staging directory, when it is elsewhere.
const STAGING: &str = "SUKKULA_CHAOS_STAGING";
/// What the child prints once it is about to write.
const READY: &str = "chaos: writing";
/// What the inbox child prints before each commit.
const COMMITTING: &str = "chaos: committing";

/// Size of every store file and received file: large enough that a write,
/// its fsync and a copy take long enough to be caught in.
const STORE_BYTES: usize = 256 * 1024;
const FILE_BYTES: usize = 512 * 1024;

/// Kill cycles per part.
const CYCLES: u32 = 30;

/// How long a child may wait to be killed before it gives up on its own. A
/// child is killed within milliseconds; this only stops one whose parent
/// died from filling the disk.
const CHILD_LIFETIME: Duration = Duration::from_secs(30);

/// The store files the child writes.
const STORE_FILES: [&str; 2] = ["settings.json", "key.pem"];

// ---------------------------------------------------------------------------
// The child.

/// Not a test of its own: the entry point of the re-run binary. As an
/// ordinary test, without [`ROLE`], it returns at once.
#[test]
fn chaos_child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let dir = PathBuf::from(std::env::var(DIR).unwrap());
    match role.as_str() {
        "store" => store_child(&dir),
        "inbox" => inbox_child(&dir, std::env::var(STAGING).ok().map(PathBuf::from)),
        other => panic!("unknown role {other}"),
    }
}

/// Tells the parent where the child is.
fn mark(marker: &str) {
    let mut out = std::io::stdout();
    writeln!(out, "{marker}").unwrap();
    out.flush().unwrap();
}

/// Tells the parent the loop starts, and makes sure the child does not
/// outlive it.
fn ready() -> Instant {
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL)).unwrap();
    mark(READY);
    Instant::now()
}

/// Rewrites both store files forever, each time whole in a new byte.
fn store_child(dir: &Path) -> ! {
    let store = Store::open(dir).unwrap();
    let started = ready();
    let mut n: u64 = 0;
    while started.elapsed() < CHILD_LIFETIME {
        n += 1;
        let content = vec![(n % 251) as u8; STORE_BYTES];
        for name in STORE_FILES {
            store.write(name, &content).unwrap();
        }
    }
    panic!("never killed");
}

/// Receives files forever. `f<b>.bin` holds nothing but the byte `b`.
fn inbox_child(dir: &Path, staging: Option<PathBuf>) -> ! {
    let inbox = match staging {
        Some(s) => Inbox::open_with_staging(dir, &s),
        None => Inbox::open(dir),
    }
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let started = ready();
    runtime.block_on(async move {
        let mut n: u64 = 0;
        while started.elapsed() < CHILD_LIFETIME {
            n += 1;
            let b = (n % 251) as u8;
            let mut f = inbox
                .begin(&sanitize(&format!("f{b:03}.bin")), FILE_BYTES as u64, None)
                .await
                .unwrap();
            let chunk = vec![b; 64 * 1024];
            for _ in 0..FILE_BYTES / chunk.len() {
                f.write(&chunk).await.unwrap();
            }
            mark(COMMITTING);
            f.commit().await.unwrap();
        }
    });
    panic!("never killed");
}

// ---------------------------------------------------------------------------
// The parent.

/// A child, and the markers it prints as they arrive.
struct Running {
    child: Child,
    markers: mpsc::Receiver<&'static str>,
}

/// Starts the child and waits until it is about to write.
// S8 bans spawning processes in Sukkula; this test needs one to kill.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)]
fn spawn(role: &str, dir: &Path, staging: Option<&Path>) -> Running {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "chaos_child", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(DIR, dir)
        .env_remove(STAGING)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(s) = staging {
        cmd.env(STAGING, s);
    }
    let mut child = cmd.spawn().unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    // libtest prints lines of its own, and `test chaos_child ... ` without
    // a line break before the test's first output. Read on a thread, so a
    // child that never gets there fails the test instead of hanging it.
    let (tx, markers) = mpsc::channel();
    std::thread::spawn(move || {
        for line in out.lines() {
            let Ok(line) = line else { break };
            let line = line.trim_end();
            for marker in [READY, COMMITTING] {
                if line.ends_with(marker) && tx.send(marker).is_err() {
                    return;
                }
            }
        }
    });
    let mut run = Running { child, markers };
    run.wait_for(READY);
    run
}

impl Running {
    fn wait_for(&mut self, marker: &str) {
        loop {
            match self.markers.recv_timeout(Duration::from_secs(60)) {
                Ok(m) if m == marker => return,
                Ok(_) => {}
                Err(_) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("the child never got to {marker:?}");
                }
            }
        }
    }

    /// Kills the child `delay` after it prints `marker` (or at once, with
    /// none), and checks that the kill is what ended it.
    fn kill(mut self, marker: Option<&str>, delay: Duration) {
        if let Some(m) = marker {
            self.wait_for(m);
        }
        std::thread::sleep(delay);
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        assert_eq!(
            status.signal(),
            Some(9),
            "the child was not ended by the SIGKILL: {status}"
        );
    }
}

/// A different moment for every cycle: 0 up to `max_ms`.
fn delay(cycle: u32, max_ms: u32) -> Duration {
    Duration::from_micros(u64::from(cycle.wrapping_mul(7_919) % (max_ms * 1000)))
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn assert_uniform(what: &str, bytes: &[u8], len: usize, byte: Option<u8>) {
    assert_eq!(bytes.len(), len, "{what} is torn: {} bytes", bytes.len());
    let first = byte.unwrap_or(bytes[0]);
    assert!(
        bytes.iter().all(|b| *b == first),
        "{what} mixes two versions"
    );
}

#[test]
fn a_store_write_killed_at_any_moment_leaves_the_old_or_the_new_file_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mut landed = 0;
    for cycle in 0..CYCLES {
        spawn("store", &data, None).kill(None, delay(cycle, 40));
        if names(&data).iter().any(|n| n.ends_with(".tmp")) {
            landed += 1;
        }
        // The next start.
        let store = Store::open(&data).unwrap();
        for name in names(&data) {
            assert!(
                STORE_FILES.contains(&name.as_str()),
                "cycle {cycle}: {name} survived the open"
            );
            let bytes = store.read(&name, STORE_BYTES).unwrap().unwrap();
            assert_uniform(&format!("cycle {cycle}: {name}"), &bytes, STORE_BYTES, None);
        }
    }
    eprintln!("chaos: {landed} of {CYCLES} store kills landed mid-write");
    assert!(
        landed > 0,
        "no kill landed mid-write; the test proved nothing"
    );
}

/// One inbox part: kills a receiver, and after every kill checks what the
/// next open leaves. With staging elsewhere, the kills aim at the commit,
/// where the copy is. Returns how many kills left work in progress, and how
/// many of those were mid-copy.
fn inbox_storm(target: &Path, staging: Option<&Path>) -> (u32, u32) {
    let (mut landed, mut mid_copy) = (0, 0);
    let copies = target.join(STAGING_DIR);
    let parts = |d: &Path| names(d).iter().filter(|n| n.ends_with(".part")).count();
    for cycle in 0..CYCLES {
        let run = spawn("inbox", target, staging);
        match staging {
            None => run.kill(None, delay(cycle, 40)),
            Some(_) => run.kill(Some(COMMITTING), delay(cycle, 4)),
        }
        let staged = staging.map_or(0, parts);
        let copying = parts(&copies);
        if staged + copying > 0 {
            landed += 1;
        }
        if staging.is_some() && copying > 0 {
            mid_copy += 1;
        }
        // The next start.
        drop(match staging {
            Some(s) => Inbox::open_with_staging(target, s).unwrap(),
            None => Inbox::open(target).unwrap(),
        });
        assert_eq!(names(&copies), Vec::<String>::new(), "cycle {cycle}");
        if let Some(s) = staging {
            assert_eq!(names(s), Vec::<String>::new(), "cycle {cycle}");
        }
        for name in names(target) {
            if name == STAGING_DIR {
                continue;
            }
            // `f007.bin`, or `f007 (3).bin` when the name was taken.
            let byte: u8 = name
                .get(1..4)
                .and_then(|b| b.parse().ok())
                .unwrap_or_else(|| panic!("cycle {cycle}: {name} is not a received file"));
            let path = target.join(&name);
            let bytes = std::fs::read(&path).unwrap();
            assert_uniform(
                &format!("cycle {cycle}: {name}"),
                &bytes,
                FILE_BYTES,
                Some(byte),
            );
            // Checked; out of the way, so the disk stays small.
            std::fs::remove_file(&path).unwrap();
        }
    }
    (landed, mid_copy)
}

#[test]
fn a_receive_killed_at_any_moment_leaves_whole_files_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("Sukkula");
    let (landed, _) = inbox_storm(&target, None);
    eprintln!("chaos: {landed} of {CYCLES} receive kills landed mid-file");
    assert!(
        landed > 0,
        "no kill landed mid-file; the test proved nothing"
    );
}

/// The copy fallback, for real: staging on `/dev/shm`, the target on disk,
/// so the link fails with `EXDEV`. It used to create the file under its
/// real name and copy into it, and a kill mid-copy left a truncated file
/// that looked complete.
#[test]
fn a_receive_killed_mid_copy_leaves_no_partial_file_under_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("Sukkula");
    std::fs::create_dir(&target).unwrap();
    let Ok(shm) = tempfile::tempdir_in("/dev/shm") else {
        eprintln!("chaos: SKIP the copy fallback: no /dev/shm here");
        return;
    };
    let dev = |p: &Path| std::os::unix::fs::MetadataExt::dev(&std::fs::metadata(p).unwrap());
    if dev(shm.path()) == dev(&target) {
        eprintln!("chaos: SKIP the copy fallback: /dev/shm is the target's file system");
        return;
    }
    let staging = shm.path().join("staging");
    let (landed, mid_copy) = inbox_storm(&target, Some(&staging));
    eprintln!("chaos: {landed} of {CYCLES} receive kills landed mid-file, {mid_copy} mid-copy");
    assert!(
        mid_copy > 0,
        "no kill landed mid-copy; the test proved nothing"
    );
}
