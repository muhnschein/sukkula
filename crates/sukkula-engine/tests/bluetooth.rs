//! The Bluetooth adapter (F-BT1, F-BT2) against a fake BlueZ and a fake
//! obexd, each on a private `dbus-daemon`: no real Bluetooth, no host bus,
//! nothing activatable (the bus config has no service directory).
//!
//! The fakes speak the real wire protocol through the same libdbus, so
//! what is tested is the adapter's side of the conversation as it happens
//! on the phone: the calls it makes, the order, the cleanup, and what it
//! does with replies and signals it should not trust.
//!
//! Needs the `dbus-daemon` binary (Debian/Ubuntu: `apt-get install
//! dbus-daemon`) and, to build the `dbus` crate, `libdbus-1-dev` and
//! `pkg-config`.

#![cfg(feature = "bluetooth")]
// Fixture code outside `#[test]` functions, where clippy's test allowances
// do not reach. A fixture that cannot be built is a broken test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::io::{BufRead, BufReader, Write};
use std::path::Path as FsPath;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dbus::Message;
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::channel::Channel;
use dbus::message::MessageType;
use dbus::strings::{ErrorName, Path};
use sukkula_core::config::Settings;
use sukkula_core::consent::ConsentBroker;
use sukkula_core::inbox::Inbox;
use sukkula_core::limits::MAX_ALIAS_CHARS;
use sukkula_core::name;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_core::{Protocol, text};
use sukkula_engine::adapter::{Adapter, Outgoing, OutgoingFile};
use sukkula_engine::api::{
    BluetoothDevice, Direction, ErrorCode, ErrorInfo, Event, Outcome, SendTarget, TransferId,
};
use sukkula_engine::bluetooth::{BusConfig, Timeouts, adapter_with};
use sukkula_engine::ctx::{Ctx, EventSink};

const OPP: &str = "00001105-0000-1000-8000-00805f9b34fb";
const SESSION: &str = "/org/bluez/obex/client/session1";
const TRANSFER1: &str = "/org/bluez/obex/client/session1/transfer1";

// ------------------------------------------------------------ the buses

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
            .prefix("skbt")
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
        // The address line is printed once the socket is listening.
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

/// A registered connection, owning `name` if it is not empty.
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

fn add_match(ch: &Channel, rule: &str) {
    let m = Message::new_method_call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "AddMatch",
    )
    .unwrap()
    .append1(rule);
    ch.send_with_reply_and_block(m, Duration::from_secs(5))
        .unwrap();
}

/// What a fake service does.
trait Serve: Send + 'static {
    fn setup(&mut self, _ch: &Channel) {}
    fn call(&mut self, ch: &Channel, m: &Message);
    fn signal(&mut self, _ch: &Channel, _m: &Message) {}
    fn tick(&mut self, _ch: &Channel) {}
}

/// A fake service on its own thread. Dropping it closes its connection,
/// which the bus announces like any service exiting.
struct Fake {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Fake {
    fn run(address: &str, name: &'static str, mut serve: impl Serve) -> Fake {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let address = address.to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            let ch = connect(&address, name);
            serve.setup(&ch);
            ready_tx.send(()).unwrap();
            while !stopping.load(Ordering::Relaxed) {
                if ch.read_write(Some(Duration::from_millis(5))).is_err() {
                    break;
                }
                while let Some(m) = ch.pop_message() {
                    match m.msg_type() {
                        MessageType::MethodCall => serve.call(&ch, &m),
                        MessageType::Signal => serve.signal(&ch, &m),
                        _ => {}
                    }
                }
                serve.tick(&ch);
                ch.flush();
            }
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Fake {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn error(m: &Message, name: &str) -> Message {
    // A message with a path in it, as obexd's real ones have: it must never
    // reach an ErrorInfo.
    m.error(
        &ErrorName::new(name).unwrap(),
        &CString::new("failed on /home/defaultuser/secret-plans.txt").unwrap(),
    )
}

fn v<T: RefArg + 'static>(t: T) -> Variant<Box<dyn RefArg>> {
    Variant(Box::new(t))
}

// ------------------------------------------------------------ fake BlueZ

type Objects = HashMap<Path<'static>, HashMap<String, PropMap>>;

#[derive(Clone)]
struct Dev {
    address: String,
    alias: String,
    paired: bool,
    opp: bool,
}

fn dev(address: &str, alias: &str) -> Dev {
    Dev {
        address: address.into(),
        alias: alias.into(),
        paired: true,
        opp: true,
    }
}

fn objects(devs: &[Dev], powered: bool) -> Objects {
    let mut o = Objects::new();
    let mut adapter = PropMap::new();
    adapter.insert("Powered".into(), v(powered));
    adapter.insert("Address".into(), v("00:1A:7D:DA:71:13".to_owned()));
    o.entry(Path::new("/org/bluez/hci0").unwrap())
        .or_default()
        .insert("org.bluez.Adapter1".into(), adapter);
    for (i, d) in devs.iter().enumerate() {
        let mut p = PropMap::new();
        p.insert("Address".into(), v(d.address.clone()));
        p.insert("AddressType".into(), v("public".to_owned()));
        p.insert("Alias".into(), v(d.alias.clone()));
        p.insert("Name".into(), v(d.alias.clone()));
        p.insert("Paired".into(), v(d.paired));
        p.insert("Trusted".into(), v(false));
        p.insert("Blocked".into(), v(false));
        p.insert("Class".into(), v(0x5a020cu32));
        p.insert("RSSI".into(), v(-60i16));
        let mut uuids = vec!["0000110a-0000-1000-8000-00805f9b34fb".to_owned()];
        if d.opp {
            uuids.push(OPP.to_owned());
        }
        p.insert("UUIDs".into(), v(uuids));
        p.insert(
            "Adapter".into(),
            v(Path::new("/org/bluez/hci0").unwrap().into_static()),
        );
        let mut ifaces = HashMap::new();
        ifaces.insert("org.bluez.Device1".to_owned(), p);
        ifaces.insert("org.freedesktop.DBus.Properties".to_owned(), PropMap::new());
        o.insert(
            Path::new(format!("/org/bluez/hci0/dev_{i}")).unwrap(),
            ifaces,
        );
    }
    o
}

/// BlueZ, answering `GetManagedObjects` with whatever `reply` makes of the
/// call (`None`: never answer).
struct Bluez<F>(F);

impl<F: FnMut(&Message) -> Option<Message> + Send + 'static> Serve for Bluez<F> {
    fn call(&mut self, ch: &Channel, m: &Message) {
        let member = m.member().map(|x| x.to_string()).unwrap_or_default();
        if member == "GetManagedObjects" {
            if let Some(r) = (self.0)(m) {
                ch.send(r).unwrap();
            }
        } else {
            ch.send(error(m, "org.freedesktop.DBus.Error.UnknownMethod"))
                .unwrap();
        }
    }
}

fn bluez_with(devs: Vec<Dev>, powered: bool) -> impl Serve {
    Bluez(move |m: &Message| Some(m.method_return().append1(objects(&devs, powered))))
}

// ------------------------------------------------------------ fake obexd

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    /// Accept, report progress in `steps` steps 300 ms apart, complete.
    Complete { steps: u64 },
    /// Go active, then fail.
    Error,
    /// Go active, then say nothing ever again.
    Stall,
    /// Report more bytes sent than the file has.
    Overrun,
    /// Report a Size other than the file's.
    WrongSize,
    /// Send half, then drop the transfer object without a final status.
    Vanish,
    /// Never answer CreateSession.
    NoAnswer,
    /// Refuse CreateSession the way obexd does for an unreachable device.
    Unreachable,
    /// Answer CreateSession with "/".
    BadSessionPath,
    /// Put the transfer outside the session.
    BadTransferPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Rec {
    CreateSession {
        destination: String,
        target: String,
    },
    SendFile(String),
    Cancel(String),
    RemoveSession(String),
    /// The connection that created the session left the bus.
    ClientGone,
}

type Log = Arc<Mutex<Vec<Rec>>>;

struct Transfer {
    path: String,
    size: u64,
    transferred: u64,
    status: &'static str,
}

enum Step {
    Status(&'static str),
    Transferred(u64),
    Remove,
}

struct Obexd {
    script: Script,
    log: Log,
    client: Option<String>,
    transfers: u32,
    transfer: Option<Transfer>,
    timeline: VecDeque<(Instant, Step)>,
}

impl Obexd {
    fn new(script: Script, log: Log) -> Obexd {
        Obexd {
            script,
            log,
            client: None,
            transfers: 0,
            transfer: None,
            timeline: VecDeque::new(),
        }
    }

    fn record(&self, r: Rec) {
        self.log.lock().unwrap().push(r);
    }

    fn props(t: &Transfer) -> PropMap {
        let mut p = PropMap::new();
        p.insert("Status".into(), v(t.status.to_owned()));
        p.insert("Size".into(), v(t.size));
        p.insert("Transferred".into(), v(t.transferred));
        p.insert("Name".into(), v("whatever".to_owned()));
        p.insert(
            "Session".into(),
            v(Path::new(SESSION).unwrap().into_static()),
        );
        p
    }

    fn changed(ch: &Channel, path: &str, props: PropMap) {
        let m = Message::new_signal(path, "org.freedesktop.DBus.Properties", "PropertiesChanged")
            .unwrap()
            .append3("org.bluez.obex.Transfer1", props, Vec::<String>::new());
        ch.send(m).unwrap();
    }

    fn schedule(&mut self, size: u64) {
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        let t = &mut self.timeline;
        match self.script {
            Script::Complete { steps } => {
                t.push_back((at(50), Step::Status("active")));
                for i in 1..=steps {
                    t.push_back((at(50 + i * 300), Step::Transferred(size * i / steps)));
                }
                t.push_back((at(100 + steps * 300), Step::Status("complete")));
                t.push_back((at(100 + steps * 300), Step::Remove));
            }
            Script::Error => {
                t.push_back((at(50), Step::Status("active")));
                t.push_back((at(300), Step::Status("error")));
                t.push_back((at(300), Step::Remove));
            }
            Script::Stall | Script::WrongSize => t.push_back((at(50), Step::Status("active"))),
            Script::Overrun => {
                t.push_back((at(50), Step::Status("active")));
                t.push_back((at(300), Step::Transferred(size + 1000)));
            }
            Script::Vanish => {
                t.push_back((at(50), Step::Status("active")));
                t.push_back((at(200), Step::Transferred(size / 2)));
                t.push_back((at(400), Step::Remove));
            }
            Script::NoAnswer
            | Script::Unreachable
            | Script::BadSessionPath
            | Script::BadTransferPath => {}
        }
    }
}

impl Serve for Obexd {
    fn setup(&mut self, ch: &Channel) {
        add_match(
            ch,
            "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged'",
        );
    }

    fn call(&mut self, ch: &Channel, m: &Message) {
        let member = m.member().map(|x| x.to_string()).unwrap_or_default();
        let path = m.path().map(|x| x.to_string()).unwrap_or_default();
        let reply = match member.as_str() {
            "CreateSession" => {
                let (destination, args): (&str, PropMap) = m.read2().unwrap();
                let target = args
                    .get("Target")
                    .and_then(|v| v.0.as_str())
                    .unwrap_or("")
                    .to_owned();
                self.record(Rec::CreateSession {
                    destination: destination.to_owned(),
                    target,
                });
                self.client = m.sender().map(|s| s.to_string());
                match self.script {
                    Script::NoAnswer => None,
                    Script::Unreachable => Some(error(m, "org.bluez.obex.Error.Failed")),
                    Script::BadSessionPath => {
                        Some(m.method_return().append1(Path::new("/").unwrap()))
                    }
                    _ => Some(m.method_return().append1(Path::new(SESSION).unwrap())),
                }
            }
            "RemoveSession" => {
                let p: Path<'_> = m.read1().unwrap();
                self.record(Rec::RemoveSession(p.to_string()));
                Some(m.method_return())
            }
            "SendFile" => {
                let file: &str = m.read1().unwrap();
                self.record(Rec::SendFile(file.to_owned()));
                match std::fs::metadata(file) {
                    Err(_) => Some(error(m, "org.bluez.obex.Error.InvalidArguments")),
                    Ok(meta) => {
                        self.transfers += 1;
                        let tpath = if self.script == Script::BadTransferPath {
                            "/elsewhere/transfer1".to_owned()
                        } else {
                            format!("{SESSION}/transfer{}", self.transfers)
                        };
                        let size = if self.script == Script::WrongSize {
                            meta.len() + 10
                        } else {
                            meta.len()
                        };
                        let t = Transfer {
                            path: tpath.clone(),
                            size,
                            transferred: 0,
                            status: "queued",
                        };
                        let r = m
                            .method_return()
                            .append2(Path::new(tpath).unwrap(), Self::props(&t));
                        self.transfer = Some(t);
                        self.schedule(size);
                        Some(r)
                    }
                }
            }
            "Cancel" => {
                self.record(Rec::Cancel(path.clone()));
                match self.transfer.take() {
                    Some(t) if t.path == path => {
                        ch.send(m.method_return()).unwrap();
                        let mut p = PropMap::new();
                        p.insert("Status".into(), v("error".to_owned()));
                        Self::changed(ch, &t.path, p);
                        self.timeline.clear();
                        None
                    }
                    other => {
                        self.transfer = other;
                        Some(error(m, "org.freedesktop.DBus.Error.UnknownObject"))
                    }
                }
            }
            "GetAll" => match &self.transfer {
                Some(t) if t.path == path => Some(m.method_return().append1(Self::props(t))),
                _ => Some(error(m, "org.freedesktop.DBus.Error.UnknownObject")),
            },
            _ => Some(error(m, "org.freedesktop.DBus.Error.UnknownMethod")),
        };
        if let Some(r) = reply {
            ch.send(r).unwrap();
        }
    }

    fn signal(&mut self, _ch: &Channel, m: &Message) {
        if m.member().is_some_and(|x| &*x == "NameOwnerChanged") {
            let (name, _old, new) = m.get3::<&str, &str, &str>();
            if name.is_some() && name.map(str::to_owned) == self.client && new == Some("") {
                self.record(Rec::ClientGone);
            }
        }
    }

    fn tick(&mut self, ch: &Channel) {
        let now = Instant::now();
        while self.timeline.front().is_some_and(|(at, _)| *at <= now) {
            let (_, step) = self.timeline.pop_front().unwrap();
            let Some(t) = self.transfer.as_mut() else {
                continue;
            };
            let mut p = PropMap::new();
            match step {
                Step::Status(s) => {
                    t.status = s;
                    p.insert("Status".into(), v(s.to_owned()));
                }
                Step::Transferred(n) => {
                    t.transferred = n;
                    p.insert("Transferred".into(), v(n));
                }
                Step::Remove => {
                    self.transfer = None;
                    continue;
                }
            }
            let path = t.path.clone();
            Self::changed(ch, &path, p);
        }
    }
}

// ------------------------------------------------------------ the rig

/// Starts a fake BlueZ on the system bus at the address given.
type StartBluez = Box<dyn FnOnce(&str) -> Fake>;

fn quick() -> Timeouts {
    Timeouts {
        call: Duration::from_secs(2),
        connect: Duration::from_secs(2),
        consent: Duration::from_secs(1),
        idle: Duration::from_secs(1),
        poll: Duration::from_millis(300),
        cleanup: Duration::from_millis(500),
    }
}

struct Rig {
    ctx: Arc<Ctx>,
    events: Arc<Mutex<Vec<Event>>>,
    adapter: Arc<dyn Adapter>,
    log: Log,
    obexd: Option<Fake>,
    _bluez: Option<Fake>,
    session: Daemon,
    _system: Daemon,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new(bluez: Option<StartBluez>, script: Option<Script>) -> Rig {
        Rig::with(bluez, script, quick(), |c| c)
    }

    fn with(
        bluez: Option<StartBluez>,
        script: Option<Script>,
        timeouts: Timeouts,
        tweak: impl FnOnce(BusConfig) -> BusConfig,
    ) -> Rig {
        let system = Daemon::start();
        let session = Daemon::start();
        let bluez = bluez.map(|b| b(&system.address));
        let log: Log = Arc::default();
        let obexd = script.map(|s| {
            Fake::run(
                &session.address,
                "org.bluez.obex",
                Obexd::new(s, log.clone()),
            )
        });
        let dir = tempfile::tempdir().unwrap();
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let sink: EventSink = {
            let events = events.clone();
            Arc::new(move |e| events.lock().unwrap().push(e))
        };
        let ctx = Arc::new(Ctx::new(
            Settings::default(),
            "Test Phone".into(),
            Store::open(&dir.path().join("data")).unwrap(),
            Inbox::open(&dir.path().join("dl")).unwrap(),
            ConsentBroker::new(Arc::new(|_| {})),
            ReachPolicy::default(),
            sink,
        ));
        let config = tweak(BusConfig {
            system_bus: Some(system.address.clone()),
            session_bus: Some(session.address.clone()),
            timeouts,
        });
        let adapter = adapter_with(ctx.clone(), config);
        Rig {
            ctx,
            events,
            adapter,
            log,
            obexd,
            _bluez: bluez,
            session,
            _system: system,
            dir,
        }
    }

    /// A file of `len` bytes, named `name`-something.
    fn file(&self, name: &str, len: usize) -> OutgoingFile {
        let mut f = tempfile::Builder::new()
            .prefix(name)
            .suffix(".jpg")
            .tempfile_in(self.dir.path())
            .unwrap();
        f.write_all(&vec![0x5a; len]).unwrap();
        let (_, path) = f.keep().unwrap();
        let raw = path.file_name().unwrap().to_string_lossy().into_owned();
        OutgoingFile {
            size: u64::try_from(len).unwrap(),
            name: name::sanitize(&raw),
            mime: Some("image/jpeg".into()),
            path,
        }
    }

    async fn send(&self, address: &str, items: Vec<Outgoing>) -> Result<TransferId, ErrorInfo> {
        self.adapter
            .send(
                SendTarget::Bluetooth {
                    address: address.into(),
                },
                items,
            )
            .await
    }

    async fn outcome(&self, id: TransferId, within: Duration) -> Outcome {
        let deadline = Instant::now() + within;
        loop {
            let found = self.events.lock().unwrap().iter().find_map(|e| match e {
                Event::TransferFinished {
                    transfer, outcome, ..
                } if *transfer == id => Some(outcome.clone()),
                _ => None,
            });
            if let Some(o) = found {
                return o;
            }
            assert!(Instant::now() < deadline, "transfer {id} did not finish");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn logged(&self, what: impl Fn(&Rec) -> bool, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.log.lock().unwrap().iter().any(&what) {
            assert!(
                Instant::now() < deadline,
                "never logged; log: {:?}",
                self.log.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn log(&self) -> Vec<Rec> {
        self.log.lock().unwrap().clone()
    }

    fn progress(&self, id: TransferId) -> Vec<(u64, u64)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::TransferProgress {
                    transfer,
                    bytes,
                    total,
                } if *transfer == id => Some((*bytes, *total)),
                _ => None,
            })
            .collect()
    }
}

fn bluez(devs: Vec<Dev>, powered: bool) -> Option<StartBluez> {
    Some(Box::new(move |addr: &str| {
        Fake::run(addr, "org.bluez", bluez_with(devs, powered))
    }))
}

fn raw_bluez<F>(f: F) -> Option<StartBluez>
where
    F: FnMut(&Message) -> Option<Message> + Send + 'static,
{
    Some(Box::new(move |addr: &str| {
        Fake::run(addr, "org.bluez", Bluez(f))
    }))
}

fn phone() -> Vec<Dev> {
    vec![dev("aa:bb:cc:dd:ee:01", "Pekka's phone")]
}

const PHONE: &str = "AA:BB:CC:DD:EE:01";

fn failed(o: &Outcome) -> &ErrorInfo {
    match o {
        Outcome::Failed { error } => error,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn assert_clean_message(e: &ErrorInfo) {
    assert!(!e.message.contains("secret"), "{e:?}");
    assert!(!e.message.contains('/'), "{e:?}");
}

const LONG: Duration = Duration::from_secs(10);

// ------------------------------------------------------------ listing

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_only_paired_push_devices() {
    let mut unpaired = dev("AA:BB:CC:DD:EE:02", "Stranger");
    unpaired.paired = false;
    let mut headset = dev("AA:BB:CC:DD:EE:03", "Headset");
    headset.opp = false;
    let rig = Rig::new(
        bluez(
            vec![
                dev("aa:bb:cc:dd:ee:01", "Pekka's phone"),
                unpaired,
                headset,
                dev("AA:BB:CC:DD:EE:04", "Anna's laptop"),
            ],
            true,
        ),
        None,
    );
    let devices = rig.adapter.list_devices().await.unwrap();
    assert_eq!(
        devices,
        vec![
            BluetoothDevice {
                address: "AA:BB:CC:DD:EE:04".into(),
                name: "Anna's laptop".into(),
            },
            BluetoothDevice {
                address: PHONE.into(),
                name: "Pekka's phone".into(),
            },
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_device_names_are_made_showable() {
    let names = [
        "Alice\u{202E}gpj.exe",
        "\u{200B}\u{200D}\u{2066}Bob\u{2069}\u{FEFF}",
        "line\nbreak\r\tand\u{7}bell\u{1b}[31m",
        "\u{200B}\u{200B}",
        &"W".repeat(100_000),
        &"\u{200B}".repeat(50_000),
        "e\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}",
    ];
    let devs: Vec<Dev> = names
        .iter()
        .enumerate()
        .map(|(i, n)| dev(&format!("10:00:00:00:00:{i:02X}"), n))
        .collect();
    let rig = Rig::new(bluez(devs, true), None);
    let devices = rig.adapter.list_devices().await.unwrap();
    assert_eq!(devices.len(), names.len());
    for d in &devices {
        assert!(!d.name.is_empty());
        assert!(d.name.chars().count() <= MAX_ALIAS_CHARS, "{:?}", d.name);
        assert!(!d.name.chars().any(text::is_forbidden), "{:?}", d.name);
    }
    let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"Alicegpj.exe"));
    assert!(names.contains(&"Bob"));
    // Nothing showable: the address stands in.
    assert!(names.contains(&"10:00:00:00:00:03"));
    assert!(names.contains(&"10:00:00:00:00:05"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_addresses_are_skipped_and_refused() {
    let rig = Rig::new(
        bluez(
            vec![
                dev("AA:BB:CC:DD:EE", "short"),
                dev("AA:BB:CC:DD:EE:GG", "not hex"),
                dev("AA-BB-CC-DD-EE-05", "dashes"),
                dev("AA:BB:CC:DD:EE:05 ", "trailing space"),
                dev("00:00:00:00:00:00", "any"),
                dev("FF:FF:FF:FF:FF:FF", "all"),
                dev("AA:BB:CC:DD:EE:01", "good"),
            ],
            true,
        ),
        Some(Script::Complete { steps: 1 }),
    );
    let devices = rig.adapter.list_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].address, PHONE);
    let file = rig.file("x", 10);
    for bad in [
        "",
        "AA:BB:CC:DD:EE",
        "AA:BB:CC:DD:EE:01\0",
        "AA:BB:CC:DD:EE:01/../x",
        "aa-bb-cc-dd-ee-01",
        "AA:BB:CC:DD:EE:0１",
    ] {
        let e = rig
            .send(bad, vec![Outgoing::File(file.clone())])
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadCommand, "{bad:?}");
    }
    // A syntactically fine address that BlueZ lists only malformed.
    let e = rig
        .send("AA:BB:CC:DD:EE:05", vec![Outgoing::File(file)])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(rig.log().is_empty(), "{:?}", rig.log());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn huge_replies_are_bounded() {
    let mut devs = Vec::new();
    for i in 0..3000u32 {
        let mut d = dev(
            &format!("20:00:00:00:{:02X}:{:02X}", i >> 8, i & 0xff),
            &format!("scan result {i}"),
        );
        d.paired = false;
        devs.push(d);
    }
    for i in 0..100u32 {
        devs.push(dev(
            &format!("30:00:00:00:00:{i:02X}"),
            &format!("paired {i:03}"),
        ));
    }
    let rig = Rig::new(bluez(devs, true), None);
    let started = Instant::now();
    let devices = rig.adapter.list_devices().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(devices.len(), 64);
    let mut sorted = devices.clone();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(devices, sorted);
    assert!(devices.iter().all(|d| d.name.starts_with("paired ")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_shaped_replies_fail_cleanly() {
    let rig = Rig::new(
        raw_bluez(|m| Some(m.method_return().append1("not a dictionary"))),
        None,
    );
    let e = rig.adapter.list_devices().await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);

    let rig = Rig::new(
        raw_bluez(|m| Some(error(m, "org.freedesktop.DBus.Error.AccessDenied"))),
        None,
    );
    let e = rig.adapter.list_devices().await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
    assert_clean_message(&e);

    // Wrong types everywhere: nothing listed, nothing panics.
    let rig = Rig::new(
        raw_bluez(|m| {
            let mut p = PropMap::new();
            p.insert("Address".into(), v(0xAABBu32));
            p.insert("Paired".into(), v("yes".to_owned()));
            p.insert("UUIDs".into(), v(OPP.to_owned()));
            p.insert("Alias".into(), v(vec![1u8, 2, 3]));
            let mut ifaces = HashMap::new();
            ifaces.insert("org.bluez.Device1".to_owned(), p);
            let mut o: Objects = HashMap::new();
            o.insert(Path::new("/org/bluez/hci0/dev_x").unwrap(), ifaces);
            Some(m.method_return().append1(o))
        }),
        None,
    );
    assert_eq!(rig.adapter.list_devices().await.unwrap(), vec![]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bluez_that_never_answers_times_out() {
    let rig = Rig::new(raw_bluez(|_| None), None);
    let started = Instant::now();
    let e = rig.adapter.list_devices().await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(8),
        "{took:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_bluez_or_system_bus_is_unavailable() {
    // A bus without BlueZ on it.
    let rig = Rig::new(None, None);
    let e = rig.adapter.list_devices().await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);

    for address in [
        "unix:path=/nonexistent/sukkula/bus",
        "unixexec:path=/bin/true",
        "autolaunch:",
        "tcp:host=127.0.0.1,port=1",
    ] {
        let rig = Rig::with(None, None, quick(), |mut c| {
            c.system_bus = Some(address.into());
            c
        });
        let started = Instant::now();
        let e = rig.adapter.list_devices().await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Unavailable, "{address}");
        assert!(started.elapsed() < Duration::from_secs(2), "{address}");
        let e = rig
            .send(PHONE, vec![Outgoing::File(rig.file("x", 1))])
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unavailable, "{address}");
    }
}

// ------------------------------------------------------------ sending

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_reports_progress_and_completes() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Complete { steps: 4 }));
    let file = rig.file("photo", 40_000);
    let id = rig
        .send("aa:bb:cc:dd:ee:01", vec![Outgoing::File(file.clone())])
        .await
        .unwrap();
    assert_eq!(rig.outcome(id, LONG).await, Outcome::Done);

    let started = rig
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            Event::TransferStarted { transfer } if transfer.id == id => Some(transfer.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(started.direction, Direction::Outgoing);
    assert_eq!(started.protocol, Protocol::Bluetooth);
    assert_eq!(started.peer, "Pekka's phone");
    assert_eq!(started.total_bytes, 40_000);
    assert_eq!(started.file_count, 1);

    let progress = rig.progress(id);
    assert!(progress.len() >= 3, "{progress:?}");
    assert!(
        progress.windows(2).all(|w| w[0].0 <= w[1].0),
        "{progress:?}"
    );
    assert!(progress.iter().all(|&(_, total)| total == 40_000));
    assert_eq!(progress.last().unwrap().0, 40_000);

    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
    assert_eq!(
        rig.log()[..3],
        [
            Rec::CreateSession {
                destination: PHONE.into(),
                target: "opp".into(),
            },
            Rec::SendFile(file.path.to_str().unwrap().into()),
            Rec::RemoveSession(SESSION.into()),
        ]
    );
    assert!(!rig.log().iter().any(|r| matches!(r, Rec::Cancel(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn several_files_go_one_by_one_in_one_session() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Complete { steps: 1 }));
    let a = rig.file("a", 1000);
    let b = rig.file("b", 0);
    let c = rig.file("c", 70_000);
    let id = rig
        .send(
            PHONE,
            vec![
                Outgoing::File(a.clone()),
                Outgoing::File(b.clone()),
                Outgoing::File(c.clone()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(rig.outcome(id, LONG).await, Outcome::Done);
    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
    let log = rig.log();
    let sends: Vec<&Rec> = log
        .iter()
        .filter(|r| matches!(r, Rec::SendFile(_)))
        .collect();
    assert_eq!(
        sends,
        [
            &Rec::SendFile(a.path.to_str().unwrap().into()),
            &Rec::SendFile(b.path.to_str().unwrap().into()),
            &Rec::SendFile(c.path.to_str().unwrap().into()),
        ]
    );
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Rec::CreateSession { .. }))
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Rec::RemoveSession(_)))
            .count(),
        1
    );
    assert_eq!(rig.progress(id).last().unwrap().0, 71_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_error_status_fails_the_transfer() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Error));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Refused);
    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_cancels_the_transfer_and_removes_the_session() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Stall));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    rig.logged(|r| matches!(r, Rec::SendFile(_)), LONG).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rig.ctx.transfers().cancel(id));
    let started = Instant::now();
    assert_eq!(rig.outcome(id, LONG).await, Outcome::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(3));
    let log = rig.log();
    assert!(log.contains(&Rec::Cancel(TRANSFER1.into())), "{log:?}");
    assert!(log.contains(&Rec::RemoveSession(SESSION.into())), "{log:?}");
    let cancel_at = log.iter().position(|r| matches!(r, Rec::Cancel(_)));
    let remove_at = log.iter().position(|r| matches!(r, Rec::RemoveSession(_)));
    assert!(cancel_at < remove_at, "{log:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_transfer_times_out() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Stall));
    let started = Instant::now();
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Network);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_secs(8),
        "{took:?}"
    );
    let log = rig.log();
    assert!(log.contains(&Rec::Cancel(TRANSFER1.into())), "{log:?}");
    assert!(log.contains(&Rec::RemoveSession(SESSION.into())), "{log:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_obexd_is_unavailable_without_a_hang() {
    let rig = Rig::new(bluez(phone(), true), None);
    let started = Instant::now();
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Unavailable);
    assert!(started.elapsed() < Duration::from_secs(3));

    let rig = Rig::with(bluez(phone(), true), None, quick(), |mut c| {
        c.session_bus = Some("unix:path=/nonexistent/sukkula/session".into());
        c
    });
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Unavailable);

    let rig = Rig::with(bluez(phone(), true), None, quick(), |mut c| {
        c.session_bus = Some("unixexec:path=/bin/sh".into());
        c
    });
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Unavailable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_device_is_a_network_failure() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Unreachable));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Network);
    assert_clean_message(failed(&o));
    // No session was made, so none is removed; the connection is closed.
    rig.logged(|r| *r == Rec::ClientGone, LONG).await;
    assert!(!rig.log().iter().any(|r| matches!(r, Rec::SendFile(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_create_session_times_out_and_disconnects() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::NoAnswer));
    let started = Instant::now();
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Network);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(8),
        "{took:?}"
    );
    // obexd drops a session whose owner leaves the bus: that is what cleans
    // up a CreateSession that never answered.
    rig.logged(|r| *r == Rec::ClientGone, LONG).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_while_connecting() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::NoAnswer));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    rig.logged(|r| matches!(r, Rec::CreateSession { .. }), LONG)
        .await;
    let started = Instant::now();
    assert!(rig.ctx.transfers().cancel(id));
    assert_eq!(rig.outcome(id, LONG).await, Outcome::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
    rig.logged(|r| *r == Rec::ClientGone, LONG).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_is_refused() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Complete { steps: 1 }));
    let e = rig
        .send(PHONE, vec![Outgoing::Text("hello".into())])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
    let e = rig
        .send(
            PHONE,
            vec![
                Outgoing::File(rig.file("x", 10)),
                Outgoing::Text("hello".into()),
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
    assert!(rig.log().is_empty());
    assert!(
        !rig.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::TransferStarted { .. }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpaired_non_push_and_unknown_devices_are_refused() {
    let mut unpaired = dev("AA:BB:CC:DD:EE:02", "Stranger");
    unpaired.paired = false;
    let mut headset = dev("AA:BB:CC:DD:EE:03", "Headset");
    headset.opp = false;
    let rig = Rig::new(
        bluez(vec![unpaired, headset], true),
        Some(Script::Complete { steps: 1 }),
    );
    for address in [
        "AA:BB:CC:DD:EE:02",
        "AA:BB:CC:DD:EE:03",
        "AA:BB:CC:DD:EE:09",
    ] {
        let e = rig
            .send(address, vec![Outgoing::File(rig.file("x", 10))])
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{address}");
    }
    assert!(rig.log().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bluetooth_off_is_unavailable() {
    let rig = Rig::new(bluez(phone(), false), Some(Script::Complete { steps: 1 }));
    // Still listed: the pairing exists.
    assert_eq!(rig.adapter.list_devices().await.unwrap().len(), 1);
    let e = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
    assert!(rig.log().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn more_bytes_than_the_file_has_is_cancelled() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Overrun));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::BadFile);
    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
    assert!(rig.log().contains(&Rec::Cancel(TRANSFER1.into())));
    // Progress never ran past the file.
    assert!(rig.progress(id).iter().all(|&(b, t)| b <= t && t == 5000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_size_other_than_the_files_is_cancelled() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::WrongSize));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::BadFile);
    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
    assert!(rig.log().contains(&Rec::Cancel(TRANSFER1.into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_that_changed_since_the_check_is_not_handed_over() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Complete { steps: 1 }));
    let mut grown = rig.file("x", 5000);
    grown.size = 4999;
    let mut gone = rig.file("y", 10);
    gone.path = rig.dir.path().join("does-not-exist.jpg");
    for f in [grown, gone] {
        let id = rig.send(PHONE, vec![Outgoing::File(f)]).await.unwrap();
        let o = rig.outcome(id, LONG).await;
        assert_eq!(failed(&o).code, ErrorCode::BadFile);
    }
    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
    assert!(!rig.log().iter().any(|r| matches!(r, Rec::SendFile(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_that_vanishes_without_a_result_fails() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Vanish));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Network);
    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signals_from_anyone_but_obexd_are_ignored() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Stall));
    let impostor = connect(&rig.session.address, "");
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    rig.logged(|r| matches!(r, Rec::SendFile(_)), LONG).await;
    // Another connection on the session bus -- any app of the same user --
    // claims the transfer is done. The match rule names obexd's unique
    // name, and the adapter checks the sender again, so this never counts.
    for _ in 0..5 {
        let mut p = PropMap::new();
        p.insert("Status".into(), v("complete".to_owned()));
        p.insert("Transferred".into(), v(5000u64));
        Obexd::changed(&impostor, TRANSFER1, p);
        impostor.flush();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let o = rig.outcome(id, LONG).await;
    assert_eq!(
        failed(&o).code,
        ErrorCode::Network,
        "idle timeout, not Done"
    );
    assert!(rig.progress(id).iter().all(|&(b, _)| b == 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn odd_object_paths_from_obexd_are_refused() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::BadSessionPath));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Unavailable);
    assert!(!rig.log().iter().any(|r| matches!(r, Rec::SendFile(_))));

    let rig = Rig::new(bluez(phone(), true), Some(Script::BadTransferPath));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 10))])
        .await
        .unwrap();
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Unavailable);
    rig.logged(|r| matches!(r, Rec::RemoveSession(_)), LONG)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn obexd_exiting_mid_transfer_fails_fast() {
    let mut rig = Rig::with(
        bluez(phone(), true),
        Some(Script::Stall),
        Timeouts {
            idle: Duration::from_secs(20),
            consent: Duration::from_secs(20),
            ..quick()
        },
        |c| c,
    );
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    rig.logged(|r| matches!(r, Rec::SendFile(_)), LONG).await;
    let started = Instant::now();
    drop(rig.obexd.take());
    let o = rig.outcome(id, LONG).await;
    assert_eq!(failed(&o).code, ErrorCode::Unavailable);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_shutdown_ends_sends_quickly() {
    let rig = Rig::new(bluez(phone(), true), Some(Script::Stall));
    let id = rig
        .send(PHONE, vec![Outgoing::File(rig.file("x", 5000))])
        .await
        .unwrap();
    rig.logged(|r| matches!(r, Rec::SendFile(_)), LONG).await;
    let started = Instant::now();
    rig.ctx.shut_down();
    assert_eq!(rig.outcome(id, LONG).await, Outcome::Cancelled);
    // Inside the engine's 3 s stop budget, with room to spare.
    assert!(started.elapsed() < Duration::from_millis(2500));
    assert!(rig.log().contains(&Rec::RemoveSession(SESSION.into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiving_is_left_to_the_system() {
    let rig = Rig::new(None, Some(Script::Complete { steps: 1 }));
    assert!(!rig.adapter.receives());
    assert_eq!(rig.adapter.protocol(), Protocol::Bluetooth);
    let e = rig.adapter.start_receiving().await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
    rig.adapter.stop_receiving().await;
    // Nothing was said to obexd at all: no agent, no session.
    assert!(rig.log().is_empty());
}
