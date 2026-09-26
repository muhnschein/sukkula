//! Offline fixtures for the wormhole tests: an in-process mailbox server, an
//! in-process transit relay, Sukkula sides built on a bare `Ctx`, and peers
//! that use magic-wormhole directly -- honestly, or not.
//!
//! The mailbox speaks the protocol of magic-wormhole-mailbox-server as the
//! library uses it (`src/core/server_messages.rs`): `welcome` on connect,
//! then an `ack` for every client message followed by its reply --
//! `bind`, `list`/`nameplates`, `allocate`/`allocated`, `claim`/`claimed`,
//! `release`/`released`, `open` (which replays the mailbox), `add` (which
//! echoes to every side, the adder included), `close`/`closed`,
//! `ping`/`pong`. The relay pairs two connections that send the same
//! "please relay <token> for side <side>" line and splices them.

// Fixtures, not tests: clippy's in-test allowances do not reach them, and a
// fixture that cannot be built is a broken test, not an error to handle.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::missing_panics_doc,
    clippy::unreachable,
    clippy::type_complexity
)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_tungstenite::tungstenite::Message;
use magic_wormhole::transfer::{APP_CONFIG, AppVersion};
use magic_wormhole::transit::{self, Abilities, Hints, RelayHint, TransitRole};
use magic_wormhole::{AppConfig, Key, MailboxConnection, Wormhole};
use serde_json::{Value, json};
use sukkula_core::config::{Settings, WormholeSettings};
use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision, OfferId};
use sukkula_core::inbox::Inbox;
use sukkula_core::offer::Offer;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_engine::adapter::Adapter;
use sukkula_engine::api::Event;
use sukkula_engine::ctx::{Ctx, EventSink};
use sukkula_engine::wormhole::{self, Tuning};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::compat::TokioAsyncReadCompatExt;

/// How long a "this must happen" wait lasts before it is called a hang.
pub const DEADLINE: Duration = Duration::from_secs(20);

/// Short timeouts, so the stall tests take seconds.
pub fn tuning() -> Tuning {
    Tuning {
        handshake: Duration::from_secs(5),
        idle: Duration::from_secs(2),
        peer_wait: Duration::from_secs(15),
        answer_wait: Duration::from_secs(15),
    }
}

// ------------------------------------------------------------ mailbox

/// How the fake mailbox misbehaves.
#[derive(Clone, Debug, Default)]
pub struct Behaviour {
    /// Replaces the `welcome` message.
    pub welcome: Option<Value>,
    /// Raw frames sent to a connection right after it opens a mailbox.
    pub after_open: Vec<String>,
    /// Nameplates that exist from the start, claimed by nobody.
    pub nameplates: Vec<String>,
    /// What `allocate` hands out instead of the next free number.
    pub allocate_as: Option<String>,
}

/// A running fake mailbox.
pub struct FakeMailbox {
    /// `ws://127.0.0.1:<port>/v1`.
    pub url: String,
    /// Connections accepted so far.
    pub connections: Arc<AtomicUsize>,
    /// The `type` of every client message received, in order.
    pub received: Arc<Mutex<Vec<String>>>,
}

impl FakeMailbox {
    /// Whether a client has sent a message of type `ty`.
    pub fn has_received(&self, ty: &str) -> bool {
        self.received.lock().unwrap().iter().any(|t| t == ty)
    }
}

#[derive(Default)]
struct MailboxState {
    next_nameplate: u32,
    next_id: u64,
    /// nameplate -> (mailbox id, sides holding it)
    nameplates: HashMap<String, (String, HashSet<String>)>,
    /// mailbox id -> (messages so far, listeners)
    mailboxes: HashMap<String, (Vec<String>, Vec<(u64, mpsc::UnboundedSender<String>)>)>,
}

impl MailboxState {
    fn mailbox_for(&mut self, nameplate: &str) -> String {
        if let Some((m, _)) = self.nameplates.get(nameplate) {
            return m.clone();
        }
        self.next_id += 1;
        let m = format!("mbox{}", self.next_id);
        self.nameplates
            .insert(nameplate.to_owned(), (m.clone(), HashSet::new()));
        m
    }

    /// The next number no nameplate has, as `allocate` hands them out.
    fn free_nameplate(&mut self) -> String {
        loop {
            let candidate = self.next_nameplate.to_string();
            self.next_nameplate += 1;
            if !self.nameplates.contains_key(&candidate) {
                break candidate;
            }
        }
    }
}

/// Starts a fake mailbox.
pub async fn mailbox(behaviour: Behaviour) -> FakeMailbox {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let state = Arc::new(Mutex::new(MailboxState {
        next_nameplate: 1,
        ..MailboxState::default()
    }));
    for n in &behaviour.nameplates {
        state.lock().unwrap().mailbox_for(n);
    }
    let counter = connections.clone();
    let received: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = received.clone();
    tokio::spawn(async move {
        let mut conn_id = 0u64;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            conn_id += 1;
            tokio::spawn(serve_mailbox(
                stream,
                state.clone(),
                behaviour.clone(),
                conn_id,
                log.clone(),
            ));
        }
    });
    FakeMailbox {
        url: format!("ws://{addr}/v1"),
        connections,
        received,
    }
}

async fn serve_mailbox(
    stream: TcpStream,
    state: Arc<Mutex<MailboxState>>,
    behaviour: Behaviour,
    conn: u64,
    received: Arc<Mutex<Vec<String>>>,
) {
    let Ok(mut ws) = async_tungstenite::accept_async(stream.compat()).await else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let welcome = behaviour
        .welcome
        .clone()
        .unwrap_or_else(|| json!({"type": "welcome", "welcome": {"motd": "fake"}}));
    if ws.send(Message::text(welcome.to_string())).await.is_err() {
        return;
    }
    let mut client = Client {
        conn,
        tx,
        side: String::new(),
        opened: None,
    };
    loop {
        let incoming = tokio::select! {
            m = next_ws(&mut ws) => m,
            out = rx.recv() => {
                match out {
                    Some(text) => {
                        if ws.send(Message::text(text)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    None => break,
                }
            }
        };
        let text = match incoming {
            Some(Ok(Message::Text(t))) => t.as_str().to_owned(),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => break,
        };
        let Ok(m) = serde_json::from_str::<Value>(&text) else {
            break;
        };
        let ack = json!({"type": "ack", "id": m.get("id").cloned().unwrap_or(Value::Null)});
        if ws.send(Message::text(ack.to_string())).await.is_err() {
            break;
        }
        let ty = m["type"].as_str().unwrap_or("");
        received.lock().unwrap().push(ty.to_owned());
        let (replies, raw_after) = {
            let mut st = state.lock().unwrap();
            client.answer(&mut st, &behaviour, ty, &m)
        };
        for r in replies {
            if ws.send(Message::text(r.to_string())).await.is_err() {
                return;
            }
        }
        for r in raw_after {
            if ws.send(Message::text(r)).await.is_err() {
                return;
            }
        }
    }
}

/// One connection to the fake mailbox, and what it has told the mailbox so
/// far.
struct Client {
    /// Which connection it is, among a mailbox's listeners.
    conn: u64,
    /// Where the messages of the mailbox it opens go.
    tx: mpsc::UnboundedSender<String>,
    /// The side it bound as.
    side: String,
    /// The mailbox it has open.
    opened: Option<String>,
}

impl Client {
    /// What the mailbox does with the client message `m`, of type `ty`:
    /// the replies, then the raw frames that follow them.
    fn answer(
        &mut self,
        st: &mut MailboxState,
        behaviour: &Behaviour,
        ty: &str,
        m: &Value,
    ) -> (Vec<Value>, Vec<String>) {
        let mut replies: Vec<Value> = Vec::new();
        let mut raw_after: Vec<String> = Vec::new();
        match ty {
            "bind" => self.side = m["side"].as_str().unwrap_or("").to_owned(),
            "list" => {
                let list: Vec<Value> = st.nameplates.keys().map(|n| json!({"id": n})).collect();
                replies.push(json!({"type": "nameplates", "nameplates": list}));
            }
            "allocate" => {
                let n = behaviour
                    .allocate_as
                    .clone()
                    .unwrap_or_else(|| st.free_nameplate());
                st.mailbox_for(&n);
                if let Some((_, sides)) = st.nameplates.get_mut(&n) {
                    sides.insert(self.side.clone());
                }
                replies.push(json!({"type": "allocated", "nameplate": n}));
            }
            "claim" => {
                let n = m["nameplate"].as_str().unwrap_or("").to_owned();
                let mbox = st.mailbox_for(&n);
                let crowded = {
                    let (_, sides) = st.nameplates.get_mut(&n).unwrap();
                    sides.insert(self.side.clone());
                    sides.len() > 2
                };
                if crowded {
                    replies.push(json!({"type": "error", "error": "crowded", "orig": m}));
                } else {
                    replies.push(json!({"type": "claimed", "mailbox": mbox}));
                }
            }
            "release" => {
                let n = m["nameplate"].as_str().unwrap_or("").to_owned();
                if let Some((_, sides)) = st.nameplates.get_mut(&n) {
                    sides.remove(&self.side);
                    if sides.is_empty() {
                        st.nameplates.remove(&n);
                    }
                }
                replies.push(json!({"type": "released"}));
            }
            "open" => {
                let mbox = m["mailbox"].as_str().unwrap_or("").to_owned();
                let entry = st.mailboxes.entry(mbox.clone()).or_default();
                entry.1.push((self.conn, self.tx.clone()));
                for old in &entry.0 {
                    let _ = self.tx.send(old.clone());
                }
                self.opened = Some(mbox);
                raw_after = behaviour.after_open.clone();
            }
            "add" => {
                if let Some(mbox) = &self.opened {
                    st.next_id += 1;
                    let msg = json!({
                        "type": "message",
                        "side": self.side,
                        "phase": m["phase"],
                        "body": m["body"],
                        "id": format!("{:x}", st.next_id),
                    })
                    .to_string();
                    let entry = st.mailboxes.entry(mbox.clone()).or_default();
                    entry.0.push(msg.clone());
                    for (_, l) in &entry.1 {
                        let _ = l.send(msg.clone());
                    }
                }
            }
            "close" => {
                if let Some(mbox) = self.opened.take()
                    && let Some(entry) = st.mailboxes.get_mut(&mbox)
                {
                    entry.1.retain(|(c, _)| *c != self.conn);
                }
                replies.push(json!({"type": "closed"}));
            }
            "ping" => replies.push(json!({"type": "pong", "pong": m["ping"]})),
            _ => {}
        }
        (replies, raw_after)
    }
}

async fn next_ws(
    ws: &mut async_tungstenite::WebSocketStream<tokio_util::compat::Compat<TcpStream>>,
) -> Option<Result<Message, async_tungstenite::tungstenite::Error>> {
    use futures_core::Stream;
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *ws).poll_next(cx)).await
}

// -------------------------------------------------------------- relay

/// How the fake relay misbehaves.
#[derive(Clone, Debug)]
pub enum RelayMode {
    /// Pairs and splices.
    Honest,
    /// Once the sender's handshake ("transit sender ... ready\n\n" and
    /// "go\n") has passed, writes these bytes to the receiver and stops
    /// forwarding: a lying length prefix, as anyone on the path could.
    Inject(Vec<u8>),
}

/// A running fake relay.
pub struct FakeRelay {
    /// `tcp://127.0.0.1:<port>`.
    pub url: String,
    /// Connections accepted so far.
    pub connections: Arc<AtomicUsize>,
    /// Pairs spliced so far.
    pub paired: Arc<AtomicUsize>,
}

/// Starts a fake relay.
pub async fn relay(mode: RelayMode) -> FakeRelay {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let paired = Arc::new(AtomicUsize::new(0));
    let waiting: Arc<Mutex<HashMap<String, (String, TcpStream)>>> = Arc::default();
    let (c, p) = (connections.clone(), paired.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            c.fetch_add(1, Ordering::SeqCst);
            let waiting = waiting.clone();
            let p = p.clone();
            let mode = mode.clone();
            tokio::spawn(async move {
                let Some(line) = read_line(&mut s).await else {
                    return;
                };
                let Some(rest) = line.strip_prefix("please relay ") else {
                    return;
                };
                let (token, side) = match rest.trim_end().split_once(" for side ") {
                    Some((t, s)) => (t.to_owned(), s.to_owned()),
                    None => (rest.trim_end().to_owned(), String::new()),
                };
                let partner = {
                    let mut w = waiting.lock().unwrap();
                    match w.remove(&token) {
                        Some((other_side, other)) if other_side != side => Some(other),
                        Some(same) => {
                            w.insert(token.clone(), same);
                            None
                        }
                        None => {
                            w.insert(token, (side, s));
                            return;
                        }
                    }
                };
                let Some(mut other) = partner else {
                    return;
                };
                p.fetch_add(1, Ordering::SeqCst);
                if s.write_all(b"ok\n").await.is_err() || other.write_all(b"ok\n").await.is_err() {
                    return;
                }
                splice_pair(s, other, mode).await;
            });
        }
    });
    FakeRelay {
        url: format!("tcp://{addr}"),
        connections,
        paired,
    }
}

async fn read_line(s: &mut TcpStream) -> Option<String> {
    let mut line = Vec::new();
    loop {
        let b = s.read_u8().await.ok()?;
        line.push(b);
        if b == b'\n' {
            return String::from_utf8(line).ok();
        }
        if line.len() > 256 {
            return None;
        }
    }
}

async fn splice_pair(a: TcpStream, b: TcpStream, mode: RelayMode) {
    match mode {
        RelayMode::Honest => {
            let (mut a, mut b) = (a, b);
            let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
        }
        RelayMode::Inject(bytes) => {
            // Tell the sender from the receiver by the first bytes each
            // sends: "transit sender " or "transit receiv".
            let (mut a, mut b) = (a, b);
            let mut ha = [0u8; 15];
            let mut hb = [0u8; 15];
            if a.read_exact(&mut ha).await.is_err() || b.read_exact(&mut hb).await.is_err() {
                return;
            }
            let (mut leader, lh, mut follower, fh) = if &ha == b"transit sender " {
                (a, ha, b, hb)
            } else {
                (b, hb, a, ha)
            };
            let _ = follower.write_all(&lh).await;
            let _ = leader.write_all(&fh).await;
            let (mut lr, mut lw) = leader.split();
            let (mut fr, mut fw) = follower.split();
            let to_leader = async {
                let _ = tokio::io::copy(&mut fr, &mut lw).await;
            };
            let to_follower = async {
                // The rest of "transit sender <hex> ready\n\n" and "go\n".
                let mut rest = vec![0u8; 90 - 15];
                if lr.read_exact(&mut rest).await.is_err() {
                    return;
                }
                let _ = fw.write_all(&rest).await;
                let _ = fw.write_all(&bytes).await;
                // Hold the connection open, forwarding nothing more.
                tokio::time::sleep(DEADLINE).await;
            };
            tokio::join!(to_leader, to_follower);
        }
    }
}

// --------------------------------------------------------- a Sukkula side

/// What a side has been told.
#[derive(Default)]
pub struct Log {
    pub events: Vec<Event>,
    pub pending: Vec<(OfferId, Arc<Offer>)>,
    pub closed: Vec<OfferId>,
}

/// One Sukkula, on a bare `Ctx`, with its own directories.
pub struct Side {
    pub ctx: Arc<Ctx>,
    pub adapter: Arc<dyn Adapter>,
    pub log: Arc<Mutex<Log>>,
    pub dir: tempfile::TempDir,
}

/// Settings pointing at the fakes (F-MW4).
pub fn settings(mailbox: &str, relay: &str) -> Settings {
    Settings {
        wormhole: WormholeSettings {
            mailbox_url: Some(mailbox.to_owned()),
            relay_url: Some(relay.to_owned()),
            enabled: true, // CONTRACT: Settings.wormhole.enabled (F-C1)
        },
        ..Settings::default()
    }
    .validate()
    .unwrap()
}

impl Side {
    /// A side whose unanswered offers are declined after `consent_timeout`.
    pub fn new(mailbox: &str, relay: &str, consent_timeout: Duration) -> Side {
        Side::with_settings(settings(mailbox, relay), consent_timeout)
    }

    pub fn with_settings(settings: Settings, consent_timeout: Duration) -> Side {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let l = log.clone();
        let sink: EventSink = Arc::new(move |e| l.lock().unwrap().events.push(e));
        let l = log.clone();
        let observer: sukkula_core::consent::Observer = Arc::new(move |e| match e {
            ConsentEvent::Pending { id, offer } => l.lock().unwrap().pending.push((id, offer)),
            ConsentEvent::Closed { id, .. } => l.lock().unwrap().closed.push(id),
        });
        let consent = ConsentBroker::with_limits(observer, consent_timeout, 2);
        let store = Store::open(&dir.path().join("data")).unwrap();
        let inbox = Inbox::open(&dir.path().join("dl")).unwrap();
        let ctx = Arc::new(Ctx::new(
            settings,
            "Test Phone".into(),
            store,
            inbox,
            consent,
            ReachPolicy {
                allow_loopback: true,
            },
            sink,
        ));
        let adapter = wormhole::adapter_with(ctx.clone(), tuning());
        Side {
            ctx,
            adapter,
            log,
            dir,
        }
    }

    /// Waits for an event matching `f`.
    pub async fn event(&self, f: impl Fn(&Event) -> bool) -> Event {
        let start = tokio::time::Instant::now();
        loop {
            if let Some(e) = self.log.lock().unwrap().events.iter().find(|e| f(e)) {
                return e.clone();
            }
            assert!(start.elapsed() < DEADLINE, "no such event");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Waits for the code of a send.
    pub async fn code(&self) -> String {
        match self
            .event(|e| matches!(e, Event::WormholeCode { .. }))
            .await
        {
            Event::WormholeCode { code, .. } => code,
            _ => unreachable!(),
        }
    }

    /// Waits for how transfer `id` ended.
    pub async fn finished(&self, id: u64) -> (sukkula_engine::api::Outcome, Vec<String>) {
        match self
            .event(|e| matches!(e, Event::TransferFinished { transfer, .. } if *transfer == id))
            .await
        {
            Event::TransferFinished { outcome, saved, .. } => (outcome, saved),
            _ => unreachable!(),
        }
    }

    /// Waits for an offer to be put to the user.
    pub async fn pending(&self) -> (OfferId, Arc<Offer>) {
        let start = tokio::time::Instant::now();
        loop {
            if let Some(p) = self.log.lock().unwrap().pending.first() {
                return p.clone();
            }
            assert!(start.elapsed() < DEADLINE, "no offer was put to the user");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Whether any offer was put to the user.
    pub fn was_asked(&self) -> bool {
        !self.log.lock().unwrap().pending.is_empty()
    }

    pub fn answer(&self, id: OfferId, accept: bool) {
        let d = if accept {
            Decision::Accept
        } else {
            Decision::Decline
        };
        assert!(self.ctx.consent().answer(id, d));
    }

    pub fn download_dir(&self) -> PathBuf {
        self.dir.path().join("dl")
    }

    /// The files received, by name, and a check that nothing is left
    /// staged or placed anywhere else.
    pub fn received(&self) -> Vec<(String, Vec<u8>)> {
        let dl = self.download_dir();
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dl).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".partial" {
                continue;
            }
            assert!(e.file_type().unwrap().is_file(), "{name} is not a file");
            out.push((name, std::fs::read(e.path()).unwrap()));
        }
        let staged = std::fs::read_dir(dl.join(".partial")).unwrap().count();
        assert_eq!(staged, 0, "a partial file was left behind");
        let data = std::fs::read_dir(self.dir.path().join("data"))
            .unwrap()
            .count();
        assert_eq!(data, 0, "something was written to the data dir");
        out.sort();
        out
    }
}

/// Writes a file to send.
#[allow(clippy::disallowed_methods)] // Tests set the scene with plain writes.
pub fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// Bytes that are not all the same, so a misplaced chunk shows.
pub fn content(len: usize) -> Vec<u8> {
    (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
}

// ------------------------------------------------------ library peers

/// The transfer app config against the fake mailbox.
pub fn app_config(mailbox: &str) -> AppConfig<AppVersion> {
    APP_CONFIG.rendezvous_url(mailbox.to_owned().into())
}

/// The fake relay as a relay hint.
pub fn relay_hint(relay: &str) -> RelayHint {
    RelayHint::from_urls(None, [relay.parse().unwrap()]).unwrap()
}

/// A sending peer: allocates a code and returns it with the half-open
/// connection.
pub async fn peer_allocate(mailbox: &str) -> (MailboxConnection<AppVersion>, String) {
    let mc = MailboxConnection::create(app_config(mailbox), 2)
        .await
        .unwrap();
    let code = mc.code().to_string();
    (mc, code)
}

/// A receiving peer, connected with `code`.
pub async fn peer_connect(mailbox: &str, code: &str) -> Wormhole {
    let mc = MailboxConnection::connect(app_config(mailbox), code.parse().unwrap(), false)
        .await
        .unwrap();
    Wormhole::connect(mc).await.unwrap()
}

/// The transit key as the v1 protocol derives it.
pub fn transit_key(w: &Wormhole) -> Key<transit::TransitKey> {
    w.key()
        .derive_subkey_from_purpose(&format!("{}/transit-key", w.appid()))
}

/// Receives the next message as JSON.
pub async fn recv_json(w: &mut Wormhole) -> Value {
    tokio::time::timeout(DEADLINE, w.receive_json::<Value>())
        .await
        .expect("peer message timed out")
        .unwrap()
        .unwrap()
}

/// What a hand-built sender does after its offer is accepted.
pub enum Payload {
    /// These records, then wait for the transit ack.
    Records(Vec<Vec<u8>>),
    /// Nothing: connect and stall.
    Stall,
}

/// A hand-built v1 sender, the way the Python client orders it: transit,
/// offer, then the receiver's transit and answer, then records. Returns
/// the peer's answer and the transit ack, if one came.
pub async fn scripted_send(
    mut w: Wormhole,
    relay: &str,
    offer: Value,
    payload: Payload,
) -> (Value, Option<Value>) {
    let connector = transit::init(Abilities::FORCE_RELAY, None, vec![relay_hint(relay)])
        .await
        .unwrap();
    w.send_json(&json!({"transit": {
        "abilities-v1": connector.our_abilities(),
        "hints-v1": &**connector.our_hints(),
    }}))
    .await
    .unwrap();
    w.send_json(&offer).await.unwrap();
    let mut their: Option<(Abilities, Hints)> = None;
    let answer = loop {
        let m = recv_json(&mut w).await;
        if let Some(t) = m.get("transit") {
            let a: Abilities = serde_json::from_value(t["abilities-v1"].clone()).unwrap();
            let h: Hints = serde_json::from_value(t["hints-v1"].clone()).unwrap();
            their = Some((a, h));
            continue;
        }
        break m;
    };
    if answer.get("answer").is_none() {
        return (answer, None);
    }
    let (a, h) = their.expect("the receiver sent no transit hints");
    let key = transit_key(&w);
    let (mut t, _) = connector
        .connect(TransitRole::Leader, key, a, Arc::new(h))
        .await
        .unwrap();
    match payload {
        Payload::Records(records) => {
            for r in records {
                if t.send_record(&r).await.is_err() {
                    return (answer, None);
                }
            }
            let _ = t.flush().await;
            let ack = tokio::time::timeout(DEADLINE, t.receive_record()).await;
            let ack = match ack {
                Ok(Ok(r)) => serde_json::from_slice(&r).ok(),
                _ => None,
            };
            (answer, ack)
        }
        Payload::Stall => {
            tokio::time::sleep(DEADLINE).await;
            drop(t);
            (answer, None)
        }
    }
}

/// What a raw transit sender puts on the wire once the handshake is done.
pub enum Wire {
    /// A record sealed with the next nonce.
    Record(Vec<u8>),
    /// A record sealed with a nonce out of sequence.
    WrongNonce(Vec<u8>),
    /// A record sealed with the next nonce, written a byte at a time.
    Trickle(Vec<u8>, Duration),
}

#[derive(Debug)]
struct AnyPurpose;

impl magic_wormhole::KeyPurpose for AnyPurpose {}

fn nonce(counter: u64) -> [u8; 24] {
    let mut n = [0u8; 24];
    n[16..].copy_from_slice(&counter.to_be_bytes());
    n
}

/// A hand-built v1 sender with a hand-built transit leader, for what
/// magic-wormhole's own transit refuses to send: empty records, oversized
/// ones, replayed nonces, one byte at a time. The handshake follows the
/// library's `transit/crypto.rs`. Returns the receiver's answer.
pub async fn raw_send(w: Wormhole, relay: &str, offer: Value, wire: Vec<Wire>) -> Value {
    raw_send_via(w, Route::Relay(relay.to_owned()), offer, wire).await
}

/// How the raw sender is reached.
pub enum Route {
    /// Through the relay at this `tcp://` URL.
    Relay(String),
    /// Directly: it listens on loopback and offers only that hint.
    Direct,
}

/// [`raw_send`], by either route.
pub async fn raw_send_via(mut w: Wormhole, route: Route, offer: Value, wire: Vec<Wire>) -> Value {
    use crypto_secretbox::aead::Aead;
    use crypto_secretbox::aead::generic_array::GenericArray;
    use crypto_secretbox::{KeyInit, XSalsa20Poly1305};

    let (transit, listener) = match &route {
        Route::Relay(relay) => {
            let addr = relay.trim_start_matches("tcp://");
            let (host, port) = addr.rsplit_once(':').unwrap();
            let port: u16 = port.parse().unwrap();
            let t = json!({"transit": {
                "abilities-v1": [{"type": "relay-v1"}],
                "hints-v1": [{"type": "relay-v1", "hints": [
                    {"type": "direct-tcp-v1", "hostname": host, "port": port}]}],
            }});
            (t, None)
        }
        Route::Direct => {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            let t = json!({"transit": {
                "abilities-v1": [{"type": "direct-tcp-v1"}],
                "hints-v1": [{"type": "direct-tcp-v1", "hostname": "127.0.0.1", "port": port}],
            }});
            (t, Some(l))
        }
    };
    w.send_json(&transit).await.unwrap();
    w.send_json(&offer).await.unwrap();
    let answer = loop {
        let m = recv_json(&mut w).await;
        if m.get("transit").is_none() {
            break m;
        }
    };
    if answer != json!({"answer": {"file_ack": "ok"}}) {
        return answer;
    }
    let key = transit_key(&w);
    let sub = |purpose: &str| key.derive_subkey_from_purpose::<AnyPurpose>(purpose);
    let mut s = match (&route, listener) {
        (Route::Relay(relay), _) => {
            let mut s = TcpStream::connect(relay.trim_start_matches("tcp://"))
                .await
                .unwrap();
            s.write_all(
                format!(
                    "please relay {} for side 0123456789abcdef\n",
                    sub("transit_relay_token").to_hex()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            let mut ok = [0u8; 3];
            s.read_exact(&mut ok).await.unwrap();
            assert_eq!(&ok, b"ok\n");
            s
        }
        (Route::Direct, Some(l)) => {
            tokio::time::timeout(DEADLINE, l.accept())
                .await
                .expect("the receiver never connected directly")
                .unwrap()
                .0
        }
        (Route::Direct, None) => unreachable!(),
    };
    s.write_all(
        format!(
            "transit sender {} ready\n\n",
            sub("transit_sender").to_hex()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut theirs = [0u8; 89];
    s.read_exact(&mut theirs).await.unwrap();
    s.write_all(b"go\n").await.unwrap();

    let record_key = sub("transit_record_sender_key");
    let cipher =
        XSalsa20Poly1305::new(GenericArray::from_slice(AsRef::<[u8]>::as_ref(&record_key)));
    let seal = |counter: u64, plain: &[u8]| {
        let n = nonce(counter);
        let ct = cipher.encrypt(GenericArray::from_slice(&n), plain).unwrap();
        let len = u32::try_from(n.len() + ct.len()).unwrap();
        [len.to_be_bytes().as_slice(), &n, &ct].concat()
    };
    let mut counter = 0u64;
    for item in wire {
        let bytes = match item {
            Wire::Record(p) => {
                counter += 1;
                seal(counter - 1, &p)
            }
            Wire::WrongNonce(p) => seal(counter + 7, &p),
            Wire::Trickle(p, gap) => {
                counter += 1;
                for b in seal(counter - 1, &p) {
                    if s.write_all(&[b]).await.is_err() {
                        return answer;
                    }
                    tokio::time::sleep(gap).await;
                }
                continue;
            }
        };
        if s.write_all(&bytes).await.is_err() {
            return answer;
        }
    }
    // Hold the line until the receiver hangs up.
    let mut sink = Vec::new();
    let _ = tokio::time::timeout(DEADLINE, s.read_to_end(&mut sink)).await;
    answer
}

/// A hand-built sender that offers `offer` with no transit and waits for
/// the answer: a text sender, or one whose offer should never be answered
/// with a yes.
pub async fn scripted_offer(mut w: Wormhole, offer: Value) -> Value {
    w.send_json(&offer).await.unwrap();
    loop {
        let m = recv_json(&mut w).await;
        if m.get("transit").is_some() {
            continue;
        }
        return m;
    }
}
