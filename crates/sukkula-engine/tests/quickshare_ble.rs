//! The Quick Share BLE nudge (F-QS2) against a fake BlueZ on a private
//! `dbus-daemon`: no real Bluetooth, no host bus, nothing activatable (the
//! bus configuration, shared with tests/bluetooth.rs, has no service
//! directory).
//!
//! The fake speaks the real protocol through the same libdbus: it takes
//! `RegisterAdvertisement`, calls back for the advertisement's properties
//! the way BlueZ does before it answers, and records `Unregister`. What is
//! tested is the nudge's side of it: what it advertises, that it is best
//! effort (every way BlueZ can say no ends it quietly and at once), that
//! only BlueZ can release it, and that stopping it fits the hub's stop
//! budget.
//!
//! Needs the `dbus-daemon` binary, like tests/bluetooth.rs.

#![cfg(feature = "quickshare")]
// Fixture code outside `#[test]` functions, where clippy's test allowances
// do not reach. A fixture that cannot be built is a broken test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path as FsPath;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dbus::Message;
use dbus::arg::{PropMap, RefArg};
use dbus::channel::Channel;
use dbus::message::MessageType;
use dbus::strings::ErrorName;
use sukkula_core::config::Settings;
use sukkula_core::consent::ConsentBroker;
use sukkula_core::inbox::Inbox;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_engine::adapter::Adapter;
use sukkula_engine::ctx::Ctx;
use sukkula_engine::quickshare::ble::{
    ADVERTISEMENT_PATH, NudgeError, SERVICE_DATA, SERVICE_UUID, advertise,
};
use sukkula_engine::quickshare::{Options, Timeouts, adapter_with};
use tokio_util::sync::CancellationToken;

const ADAPTER: &str = "/org/bluez/hci0";
const MANAGER: &str = "org.bluez.LEAdvertisingManager1";

/// A private `dbus-daemon`, killed on drop.
struct Daemon {
    child: std::process::Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl Daemon {
    // S8 bans spawning processes in Sukkula; this is test scaffolding that
    // runs the reference bus daemon, the one thing a fake cannot stand in
    // for honestly.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
    fn start() -> Daemon {
        // Unix socket paths are limited to 108 bytes; stay short.
        let dir = tempfile::Builder::new()
            .prefix("skqs")
            .tempdir_in("/tmp")
            .unwrap();
        let address = format!("unix:path={}", dir.path().join("bus").display());
        let config = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dbus/bus.conf");
        let mut child = std::process::Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .arg(format!("--address={address}"))
            .arg("--nofork")
            .arg("--print-address=1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("these tests need dbus-daemon (apt-get install dbus-daemon)");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(
            line.starts_with("unix:"),
            "dbus-daemon did not start: {line:?}"
        );
        Daemon {
            child,
            address,
            _dir: dir,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// CONTRACT: S8 bans libdbus's connection constructors outside the checked
// ones (clippy.toml); the fake connects to the test's own daemon, at an
// address it made.
#[allow(clippy::disallowed_methods)]
fn connect(address: &str, name: &str) -> Channel {
    let mut ch = Channel::open_private(address).unwrap();
    ch.register().unwrap();
    if !name.is_empty() {
        let m = Message::new_method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
        )
        .unwrap()
        .append2(name, 4u32);
        let r = ch
            .send_with_reply_and_block(m, Duration::from_secs(5))
            .unwrap();
        assert_eq!(r.get1::<u32>(), Some(1), "could not own {name}");
    }
    ch
}

/// How the fake answers `RegisterAdvertisement`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// As BlueZ does: read the properties, then say yes.
    Accept,
    /// Read the properties, say yes, then release the advertisement.
    AcceptThenRelease,
    /// Out of advertising slots, or not allowed.
    Refuse,
    /// Never answer.
    Silent,
}

#[derive(Clone, Debug, Default)]
struct Log {
    registered: Vec<String>,
    /// The properties it read back, as (name, debug rendering).
    properties: Vec<(String, String)>,
    service_data: Option<(String, Vec<u8>)>,
    unregistered: Vec<String>,
    released: bool,
}

/// A fake BlueZ on its own thread.
struct Bluez {
    log: Arc<Mutex<Log>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Bluez {
    fn run(address: &str, mode: Mode) -> Bluez {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let address = address.to_owned();
        let log = Arc::new(Mutex::new(Log::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, stopping) = (log.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let ch = connect(&address, "org.bluez");
            ready_tx.send(()).unwrap();
            // The Register call waiting for its GetAll reply, by the GetAll
            // serial.
            let mut pending: Option<(u32, Message)> = None;
            let mut release_sent = false;
            while !stopping.load(Ordering::Relaxed) {
                if ch.read_write(Some(Duration::from_millis(5))).is_err() {
                    break;
                }
                while let Some(m) = ch.pop_message() {
                    match m.msg_type() {
                        MessageType::MethodCall => {
                            let member = m.member().map(|s| s.to_string()).unwrap_or_default();
                            let path = m.path().map(|s| s.to_string()).unwrap_or_default();
                            if path != ADAPTER
                                || m.interface().map(|s| s.to_string()).as_deref() != Some(MANAGER)
                            {
                                let e = m.error(
                                    &ErrorName::new("org.freedesktop.DBus.Error.UnknownMethod")
                                        .unwrap(),
                                    c"no",
                                );
                                let _ = ch.send(e);
                                continue;
                            }
                            match member.as_str() {
                                "RegisterAdvertisement" => {
                                    let (obj, _opts): (dbus::Path<'_>, PropMap) =
                                        m.read2().unwrap();
                                    l.lock().unwrap().registered.push(obj.to_string());
                                    match mode {
                                        Mode::Silent => {}
                                        Mode::Refuse => {
                                            let e = m.error(
                                                &ErrorName::new("org.bluez.Error.NotPermitted")
                                                    .unwrap(),
                                                c"Maximum advertisements reached",
                                            );
                                            let _ = ch.send(e);
                                        }
                                        Mode::Accept | Mode::AcceptThenRelease => {
                                            let get = Message::new_method_call(
                                                m.sender().unwrap(),
                                                obj,
                                                "org.freedesktop.DBus.Properties",
                                                "GetAll",
                                            )
                                            .unwrap()
                                            .append1("org.bluez.LEAdvertisement1");
                                            let serial = ch.send(get).unwrap();
                                            pending = Some((serial, m));
                                        }
                                    }
                                }
                                "UnregisterAdvertisement" => {
                                    let obj: dbus::Path<'_> = m.read1().unwrap();
                                    l.lock().unwrap().unregistered.push(obj.to_string());
                                    let _ = ch.send(Message::new_method_return(&m).unwrap());
                                }
                                _ => {}
                            }
                        }
                        MessageType::MethodReturn => {
                            let Some((serial, _)) = &pending else {
                                continue;
                            };
                            if m.get_reply_serial() != Some(*serial) {
                                continue;
                            }
                            let props: PropMap = m.read1().unwrap();
                            {
                                let mut log = l.lock().unwrap();
                                for (k, v) in &props {
                                    log.properties.push((k.clone(), format!("{v:?}")));
                                }
                                if let Some(sd) = props.get("ServiceData") {
                                    let mut it = sd.0.as_iter().unwrap();
                                    let uuid = it.next().unwrap().as_str().unwrap().to_owned();
                                    let value = it.next().unwrap();
                                    // A variant holding an array of bytes.
                                    let bytes: Vec<u8> = value
                                        .as_iter()
                                        .unwrap()
                                        .next()
                                        .unwrap()
                                        .as_iter()
                                        .unwrap()
                                        .map(|b| u8::try_from(b.as_u64().unwrap()).unwrap())
                                        .collect();
                                    log.service_data = Some((uuid, bytes));
                                }
                            }
                            let (_, register) = pending.take().unwrap();
                            let _ = ch.send(Message::new_method_return(&register).unwrap());
                            if mode == Mode::AcceptThenRelease && !release_sent {
                                release_sent = true;
                                let release = Message::new_method_call(
                                    register.sender().unwrap(),
                                    ADVERTISEMENT_PATH,
                                    "org.bluez.LEAdvertisement1",
                                    "Release",
                                )
                                .unwrap();
                                let _ = ch.send(release);
                            }
                        }
                        MessageType::Error | MessageType::Signal => {}
                    }
                }
                ch.flush();
                if release_sent {
                    l.lock().unwrap().released = true;
                }
            }
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Bluez {
            log,
            stop,
            thread: Some(thread),
        }
    }

    fn log(&self) -> Log {
        self.log.lock().unwrap().clone()
    }

    fn wait(&self, what: &str, f: impl Fn(&Log) -> bool) {
        let start = Instant::now();
        while !f(&self.log()) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}: {:?}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Bluez {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Runs the nudge on a thread of its own, as the adapter does on a blocking
/// thread.
fn nudge(address: &str) -> (CancellationToken, JoinHandle<Result<(), NudgeError>>) {
    let token = CancellationToken::new();
    let (t, a) = (token.clone(), address.to_owned());
    (token, std::thread::spawn(move || advertise(&a, &t)))
}

#[test]
fn advertises_the_beacon_and_unregisters_on_stop() {
    let bus = Daemon::start();
    let bluez = Bluez::run(&bus.address, Mode::Accept);
    let (token, running) = nudge(&bus.address);
    bluez.wait("registration and the property read", |l| {
        l.service_data.is_some()
    });
    let log = bluez.log();
    assert_eq!(log.registered, [ADVERTISEMENT_PATH]);
    // A non-connectable broadcast of the service data, and nothing that
    // identifies the phone: no name, no appearance, no address.
    let names: Vec<&str> = log.properties.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"Type") && names.contains(&"ServiceData"));
    let ty = &log.properties.iter().find(|(k, _)| k == "Type").unwrap().1;
    assert!(ty.contains("broadcast"), "{ty}");
    assert_eq!(
        log.service_data,
        Some((SERVICE_UUID.to_owned(), SERVICE_DATA.to_vec()))
    );

    // Stopping unregisters, within the stop budget.
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    token.cancel();
    assert_eq!(running.join().unwrap(), Ok(()));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    bluez.wait("unregistration", |l| !l.unregistered.is_empty());
    assert_eq!(bluez.log().unregistered, [ADVERTISEMENT_PATH]);
}

#[test]
fn only_bluez_can_release_it_and_then_it_ends() {
    let bus = Daemon::start();
    let bluez = Bluez::run(&bus.address, Mode::AcceptThenRelease);
    let (token, running) = nudge(&bus.address);
    bluez.wait("registration", |l| l.service_data.is_some());
    // BlueZ releases it right after registering: the nudge ends by itself,
    // without unregistering what is no longer registered.
    let started = Instant::now();
    assert_eq!(running.join().unwrap(), Ok(()));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(bluez.log().released);
    assert!(bluez.log().unregistered.is_empty());
    drop(token);
}

#[test]
fn an_impostor_cannot_release_it() {
    let bus = Daemon::start();
    let bluez = Bluez::run(&bus.address, Mode::Accept);
    let (token, running) = nudge(&bus.address);
    bluez.wait("registration", |l| l.service_data.is_some());
    // Find the nudge's unique name through the bus, then call Release on its
    // object from a connection that is not BlueZ.
    let impostor = connect(&bus.address, "");
    let names = Message::new_method_call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "ListNames",
    )
    .unwrap();
    let reply = impostor
        .send_with_reply_and_block(names, Duration::from_secs(5))
        .unwrap();
    let names: Vec<String> = reply.read1().unwrap();
    let mut refused = 0;
    for name in names.iter().filter(|n| n.starts_with(':')) {
        let release = Message::new_method_call(
            name.as_str(),
            ADVERTISEMENT_PATH,
            "org.bluez.LEAdvertisement1",
            "Release",
        )
        .unwrap();
        if let Err(e) = impostor.send_with_reply_and_block(release, Duration::from_secs(2))
            && e.name() == Some("org.freedesktop.DBus.Error.AccessDenied")
        {
            refused += 1;
        }
    }
    assert_eq!(refused, 1, "the nudge refused the impostor's Release");
    // Still on the air: it ends only when stopped, and then unregisters.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!running.is_finished());
    token.cancel();
    assert_eq!(running.join().unwrap(), Ok(()));
    bluez.wait("unregistration", |l| !l.unregistered.is_empty());
}

#[test]
fn every_no_from_bluez_ends_it_quietly_and_at_once() {
    let bus = Daemon::start();
    // BlueZ out of slots.
    {
        let _bluez = Bluez::run(&bus.address, Mode::Refuse);
        let started = Instant::now();
        let (_token, running) = nudge(&bus.address);
        assert_eq!(
            running.join().unwrap(),
            Err(NudgeError::Remote("org.bluez.Error.NotPermitted".into()))
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    // No BlueZ at all: nobody owns the name, and nothing can be activated.
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let (_token, running) = nudge(&bus.address);
    match running.join().unwrap() {
        Err(NudgeError::Remote(name)) => assert!(
            name == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name == "org.freedesktop.DBus.Error.NameHasNoOwner",
            "{name}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    // No bus at all, and addresses that would spawn a process (S8).
    for address in [
        format!("unix:path={}/none", bus._dir.path().display()),
        "autolaunch:".to_owned(),
        "unixexec:path=/bin/true".to_owned(),
    ] {
        let (_token, running) = nudge(&address);
        assert_eq!(running.join().unwrap(), Err(NudgeError::Unreachable));
    }
}

#[test]
fn a_bluez_that_never_answers_is_bounded_and_cancellable() {
    let bus = Daemon::start();
    let bluez = Bluez::run(&bus.address, Mode::Silent);
    // Cancelled while waiting: ends within a tick or two.
    let (token, running) = nudge(&bus.address);
    bluez.wait("registration", |l| !l.registered.is_empty());
    let started = Instant::now();
    token.cancel();
    assert_eq!(running.join().unwrap(), Err(NudgeError::Cancelled));
    assert!(started.elapsed() < Duration::from_millis(500));
    // Left alone: gives up after its own deadline.
    let started = Instant::now();
    let (_token, running) = nudge(&bus.address);
    assert_eq!(running.join().unwrap(), Err(NudgeError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(8));
}

/// Through the adapter: discovery starts the nudge when the setting is on,
/// stopping discovery takes it down within the hub's stop budget, and with
/// the setting off there is none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_runs_the_nudge_and_stops_it_in_time() {
    let bus = Daemon::start();
    let bluez = Bluez::run(&bus.address, Mode::Accept);
    for on in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.quickshare.ble_nudge = on;
        let ctx = Arc::new(Ctx::new(
            settings,
            "Test Phone".into(),
            Store::open(&dir.path().join("data")).unwrap(),
            Inbox::open(&dir.path().join("dl")).unwrap(),
            ConsentBroker::new(Arc::new(|_| {})),
            ReachPolicy {
                allow_loopback: true,
            },
            Arc::new(|_| {}),
        ));
        let adapter = adapter_with(
            ctx,
            Options {
                mdns: false,
                listen: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
                system_bus: Some(bus.address.clone()),
                timeouts: Timeouts::default(),
            },
        );
        let before = bluez.log().registered.len();
        adapter.start_discovery().await.unwrap();
        if on {
            let start = Instant::now();
            while bluez.log().service_data.is_none() || bluez.log().registered.len() == before {
                assert!(start.elapsed() < Duration::from_secs(10));
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        } else {
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(bluez.log().registered.len(), before, "no nudge when off");
        }
        let unregistered = bluez.log().unregistered.len();
        let started = Instant::now();
        adapter.stop_discovery().await;
        assert!(
            started.elapsed() < Duration::from_millis(1300),
            "{:?}",
            started.elapsed()
        );
        if on {
            assert_eq!(bluez.log().unregistered.len(), unregistered + 1);
        }
    }
}
