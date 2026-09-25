//! S9 end to end: transfers over every LAN and internet protocol with
//! distinctive file names, texts, aliases, PINs and codes, and a look
//! through every log event they caused.
//!
//! One global subscriber records every event and span, of every level,
//! from every crate -- tracing's, and the `log` crate's through the
//! bridge -- while a flow runs. Each event is then judged by the engine's
//! own table (`logging::would_log`) as the engine's log would show it,
//! with logging off (the default) and with logging on:
//!
//! - nothing shown at info or above, either way, carries a name, a text,
//!   an alias, a PIN, a wormhole code, a path, or a peer's address;
//! - nothing from Sukkula's own crates carries one at any level;
//! - nothing at all, from any crate, at any level, carries the TLS private
//!   key (its PEM, or its DER as hex or as a byte list).
//!
//! The wormhole flow runs against a mailbox server that sends a message
//! of its own invention, which magic-wormhole logs verbatim at warn: the
//! test checks that the library did, and that the table keeps it out.
//!
//! The flows use the adapters on bare contexts (the same fixtures as the
//! protocol tests), so that their tasks run on this test's runtime, where
//! the global subscriber sees them. An engine's own threads log to the
//! engine's log instead; the last test runs two engines through the JSON
//! contract and reads their real log lines back.

#![cfg(any(feature = "localsend", feature = "quickshare", feature = "wormhole"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unreachable,
    clippy::too_many_lines
)]

#[cfg(feature = "localsend")]
mod localsend_support;
#[cfg(feature = "wormhole")]
mod wormhole_support;

use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use sukkula_engine::logging::would_log;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_log::NormalizeEvent as _;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

// ------------------------------------------------------------ capture

/// One recorded event or span.
struct Captured {
    level: Level,
    target: String,
    text: String,
}

impl Captured {
    fn line(&self) -> String {
        format!("{} {}:{}", self.level, self.target, self.text)
    }
}

static CAPTURED: Mutex<Vec<Captured>> = Mutex::new(Vec::new());
static CAPTURED_BYTES: AtomicUsize = AtomicUsize::new(0);
static OVERFLOWED: AtomicBool = AtomicBool::new(false);

/// Enough for any flow here many times over; past it the test fails
/// rather than miss something.
const MAX_CAPTURED_BYTES: usize = 256 * 1024 * 1024;

fn keep(level: Level, target: &str, text: String) {
    let n = CAPTURED_BYTES.fetch_add(text.len(), Ordering::Relaxed);
    if n > MAX_CAPTURED_BYTES {
        OVERFLOWED.store(true, Ordering::Relaxed);
        return;
    }
    CAPTURED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Captured {
            level,
            target: target.to_owned(),
            text,
        });
}

/// Every field, strings as they are (so a value is found as it was
/// written), everything else as `Debug` shows it.
struct Everything<'a>(&'a mut String);

impl Visit for Everything<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        let _ = write!(self.0, " {}={value}", field.name());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

struct Capture;

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let mut text = String::new();
        event.record(&mut Everything(&mut text));
        keep(*meta.level(), meta.target(), text);
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, _: &Id, _: Context<'_, S>) {
        let mut text = String::from(" span");
        attrs.record(&mut Everything(&mut text));
        keep(*attrs.metadata().level(), attrs.metadata().target(), text);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(meta) = ctx.metadata(id) {
            let mut text = String::from(" span record");
            values.record(&mut Everything(&mut text));
            keep(*meta.level(), meta.target(), text);
        }
    }
}

/// Installs the capture as the process's default, once.
fn install() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tracing::subscriber::set_global_default(Registry::default().with(Capture)).unwrap();
        // An engine started first has installed the same bridge already.
        let _ = tracing_log::LogTracer::init();
    });
}

/// One flow at a time: the capture is the process's.
static FLOW: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Starts a flow with an empty capture of everything.
async fn begin() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = FLOW.lock().await;
    install();
    // An engine started by another test may have lowered it.
    tracing_log::log::set_max_level(tracing_log::log::LevelFilter::Trace);
    CAPTURED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    CAPTURED_BYTES.store(0, Ordering::Relaxed);
    guard
}

fn captured() -> Vec<Captured> {
    assert!(
        !OVERFLOWED.load(Ordering::Relaxed),
        "the capture overflowed"
    );
    std::mem::take(&mut *CAPTURED.lock().unwrap_or_else(PoisonError::into_inner))
}

// ------------------------------------------------------------ judging

const OURS: [&str; 3] = ["sukkula_core", "sukkula_engine", "sukkula_ffi"];

/// What a flow must never let into the log.
#[derive(Default)]
struct Watch {
    /// Never at info or above in the engine's log, with logging off or on;
    /// never from Sukkula's own crates at any level.
    private: Vec<String>,
    /// As `private`, but matched as a whole word: short numbers (the Quick
    /// Share PIN) that could otherwise be part of a size.
    private_words: Vec<String>,
    /// Never anywhere, at any level, from anyone.
    secret: Vec<String>,
}

impl Watch {
    fn private(&mut self, s: impl Into<String>) -> &mut Self {
        let s = s.into();
        assert!(
            s.len() >= 6,
            "a marker that short could match by chance: {s}"
        );
        self.private.push(s);
        self
    }
}

fn severe(level: &Level) -> bool {
    *level == Level::INFO || *level == Level::WARN || *level == Level::ERROR
}

fn words(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !c.is_ascii_alphanumeric())
}

/// Holds every captured event to `watch`, and returns how many there were
/// from each of `expect_from` (so a flow can prove it was heard at all).
fn judge(what: &str, events: &[Captured], watch: &Watch, expect_from: &[&str]) {
    for e in events {
        let line = e.line();
        for s in &watch.secret {
            assert!(
                !line.contains(s.as_str()),
                "{what}: the TLS key in a log event: {line}"
            );
        }
        let ours = OURS.iter().any(|c| e.target.starts_with(c));
        let shown = severe(&e.level)
            && (would_log(false, &e.target, &e.level) || would_log(true, &e.target, &e.level));
        if !(ours || shown) {
            continue;
        }
        for m in &watch.private {
            assert!(!line.contains(m.as_str()), "{what}: {m:?} logged: {line}");
        }
        for w in &watch.private_words {
            assert!(
                !words(&line).any(|x| x == w),
                "{what}: {w:?} logged: {line}"
            );
        }
    }
    for target in expect_from {
        assert!(
            events.iter().any(|e| e.target.starts_with(target)),
            "{what}: nothing heard from {target}; the capture is not seeing the flow"
        );
    }
    // Our own debug lines ran and were captured: the flows went through
    // the shared receive path.
    assert!(
        events
            .iter()
            .any(|e| e.target == "sukkula_engine::ctx" && e.text.contains("offer accepted")),
        "{what}: the receive path's own debug lines were not captured"
    );
}

/// The TLS key as it could leak: its PEM lines, and a piece of its DER as
/// hex and as `Debug` prints bytes.
#[cfg(feature = "localsend")]
fn key_material(pem: &str) -> Vec<String> {
    use rustls::pki_types::PrivateKeyDer;
    use rustls::pki_types::pem::PemObject;
    let mut out: Vec<String> = pem
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let der = PrivateKeyDer::from_pem_slice(pem.as_bytes()).unwrap();
    let piece = &der.secret_der()[100..116];
    out.push(piece.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }));
    out.push(format!("{piece:?}").trim_matches(['[', ']']).to_owned());
    assert!(out[0].contains("PRIVATE KEY"), "{}", out[0]);
    out
}

/// The markers every flow uses. Distinctive, so that a match is a leak and
/// not a coincidence.
const ALIAS: &str = "Zorbulon Qwixley";
const RECEIVER_ALIAS: &str = "Vendelmoor Plax";
const FILE: &str = "zorbulon-ledger-5521.txt";
const TEXT: &str = "zorbulon quartz whisper 9917";

// ------------------------------------------------------------ LocalSend

#[cfg(feature = "localsend")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn localsend_keeps_names_texts_aliases_the_pin_and_the_key_out_of_the_log() {
    use localsend_support::*;
    use sukkula_core::config::Settings;
    use sukkula_engine::adapter::{Adapter, Outgoing};
    use sukkula_engine::api::{Event, Outcome, SendTarget};

    let _flow = begin().await;
    const PIN: &str = "Q7Z4K9W2";
    const GUESS: &str = "Zorbguess77";
    // The raw peer: identity 3, from addresses of its own, so that its
    // offers do not use up the Sukkula sender's allowance (S7).
    const RAW: usize = 3;
    let raw_from = |last: u8| Some(std::net::Ipv4Addr::new(127, 0, 0, last));
    let a = Node::new(&NodeConfig::new(0, ALIAS));
    let mut b_cfg = NodeConfig::new(1, RECEIVER_ALIAS);
    b_cfg.pin = Some(PIN.into());
    let b = Node::new(&b_cfg);
    let port = b.receive().await;
    let src = tempfile::tempdir().unwrap();
    let path = make_file(src.path(), FILE, 5000);

    // F-LS4: a wrong PIN is refused before the user is asked; the right
    // one gets the offer in front of the user, who declines it (S5).
    let files = serde_json::json!({ "f": file_json("f", FILE, serde_json::json!(5000)) });
    let body = offer_json(RAW, ALIAS, &files);
    let prepare = |pin: &str| format!("{PREPARE}?pin={pin}");
    let mut s = raw_tls(port, Some(RAW), raw_from(60)).await.unwrap();
    let r = exchange(&mut s, &post(&prepare(GUESS), body.len()), &body)
        .await
        .unwrap();
    assert_eq!(r.status, 401);
    assert_eq!(b.offers_shown(), 0);
    let decline = async {
        let (id, offer) = b.offer().await;
        assert_eq!(offer.sender, ALIAS);
        b.answer(id, false);
    };
    let mut s = raw_tls(port, Some(RAW), raw_from(61)).await.unwrap();
    let head = post(&prepare(PIN), body.len());
    let (r, ()) = tokio::join!(exchange(&mut s, &head, &body), decline);
    assert_eq!(r.unwrap().status, 403);

    // Sukkula to Sukkula, with no PIN set: a file and a text.
    b.ctx.set_settings(Settings {
        device_name: RECEIVER_ALIAS.into(),
        ..Settings::default()
    });
    let peer = a.find(&b).await;
    let to = || SendTarget::LocalSend { peer: peer.clone() };
    let sent = a.ls.send(to(), vec![outgoing(&path)]).await.unwrap();
    let (id, offer) = b.offer_after(1).await;
    assert_eq!(offer.sender, ALIAS);
    b.answer(id, true);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    let incoming = b.incoming().await;
    assert_eq!(b.finished(incoming).await.1, vec![FILE.to_owned()]);

    let sent =
        a.ls.send(to(), vec![Outgoing::Text(TEXT.into())])
            .await
            .unwrap();
    let (id, _) = b.offer_after(2).await;
    b.answer(id, true);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    b.wait_event("the text", |e| match e {
        Event::TextReceived { text, .. } if text == TEXT => Some(()),
        _ => None,
    })
    .await;

    a.ls.stop_discovery().await;
    b.ls.stop_receiving().await;

    let mut watch = Watch::default();
    watch
        .private(ALIAS)
        .private(RECEIVER_ALIAS)
        .private(FILE)
        .private(TEXT)
        .private(PIN)
        .private(GUESS)
        .private("127.0.0.60")
        .private("127.0.0.61")
        .private(b.downloads().to_string_lossy())
        .private(src.path().to_string_lossy())
        .private("127.0.0.1");
    watch.secret = [0, 1, RAW]
        .iter()
        .flat_map(|&i| key_material(&identity(i).private_key_pem))
        .collect();
    judge("LocalSend", &captured(), &watch, &[]);
}

// ------------------------------------------------------------ Magic Wormhole

#[cfg(feature = "wormhole")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wormhole_keeps_codes_texts_and_names_out_of_the_log() {
    use std::time::Duration;
    use sukkula_engine::adapter::{Outgoing, OutgoingFile};
    use sukkula_engine::api::{Event, Outcome, SendTarget};
    use wormhole_support::*;

    let _flow = begin().await;
    const CONSENT: Duration = Duration::from_secs(10);
    // A mailbox server that writes into the log: magic-wormhole logs a
    // message of a type it does not know verbatim, at warn. The mailbox
    // guard lets such messages through (the protocol may grow), so only
    // the table stands between this and the journal.
    const INJECTED: &str = "Zorbulon mailbox note 4471";
    let mb = mailbox(Behaviour {
        after_open: vec![format!(r#"{{"type":"zorbulon-news","note":"{INJECTED}"}}"#)],
        ..Behaviour::default()
    })
    .await;
    let rl = relay(RelayMode::Honest).await;
    let a = Side::new(&mb.url, &rl.url, CONSENT);
    let b = Side::new(&mb.url, &rl.url, CONSENT);
    let c = Side::new(&mb.url, &rl.url, CONSENT);
    let mut watch = Watch::default();

    // A text.
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![Outgoing::Text(TEXT.into())])
        .await
        .unwrap();
    let text_code = a.code().await;
    let adapter = b.adapter.clone();
    let code = text_code.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(code).await });
    let (id, _) = b.pending().await;
    b.answer(id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(b.finished(got).await.0, Outcome::Done);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);
    assert!(
        b.log
            .lock()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e, Event::TextReceived { text, .. } if text == TEXT))
    );

    // A file, through the relay.
    let data = content(70_000);
    let path = write_file(a.dir.path(), FILE, &data);
    let item = Outgoing::File(OutgoingFile {
        path,
        name: sukkula_core::name::sanitize(FILE),
        size: u64::try_from(data.len()).unwrap(),
        mime: None,
    });
    let sent = a
        .adapter
        .send(SendTarget::Wormhole, vec![item])
        .await
        .unwrap();
    let file_code = match a
        .event(|e| matches!(e, Event::WormholeCode { transfer, .. } if *transfer == sent))
        .await
    {
        Event::WormholeCode { code, .. } => code,
        _ => unreachable!(),
    };
    let adapter = c.adapter.clone();
    let code = file_code.clone();
    let rx = tokio::spawn(async move { adapter.receive_code(code).await });
    let (id, _) = c.pending().await;
    c.answer(id, true);
    let got = rx.await.unwrap().unwrap();
    assert_eq!(c.finished(got).await.1, vec![FILE.to_owned()]);
    assert_eq!(a.finished(sent).await.0, Outcome::Done);

    for code in [&text_code, &file_code] {
        // The whole code, and the secret part: the words after the
        // nameplate.
        let password = code.split_once('-').unwrap().1;
        watch.private(code.as_str()).private(password);
    }
    watch
        .private(FILE)
        .private(TEXT)
        .private(INJECTED)
        .private(a.dir.path().to_string_lossy())
        .private(c.download_dir().to_string_lossy());
    let events = captured();
    // The finding is real: the library did log what the server sent, at
    // warn -- and the engine's table keeps it out, off and on.
    assert!(
        events.iter().any(|e| e.level == Level::WARN
            && e.target.starts_with("magic_wormhole::core")
            && e.text.contains(INJECTED)),
        "the mailbox's message was not logged at warn"
    );
    for e in events.iter().filter(|e| e.text.contains(INJECTED)) {
        // That warning, and the WebSocket layers' frame traces.
        assert!(
            !would_log(false, &e.target, &e.level) && !would_log(true, &e.target, &e.level),
            "{}",
            e.line()
        );
    }
    judge("wormhole", &events, &watch, &["magic_wormhole"]);
}

// ------------------------------------------------------------ Quick Share

#[cfg(feature = "quickshare")]
mod quick_share {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use sukkula_core::config::Settings;
    use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision};
    use sukkula_core::inbox::Inbox;
    use sukkula_core::offer::Offer;
    use sukkula_core::reach::ReachPolicy;
    use sukkula_core::store::Store;
    use sukkula_engine::adapter::{Adapter, Outgoing, OutgoingFile};
    use sukkula_engine::api::{Event, Outcome, SendTarget, TransferId};
    use sukkula_engine::ctx::Ctx;
    use sukkula_engine::quickshare::{Options, QuickShareAdapter, Timeouts, adapter_with};

    use super::*;

    const DEADLINE: Duration = Duration::from_secs(20);

    /// The offers put to the user, in order.
    type Offers = Arc<Mutex<Vec<(u64, Arc<Offer>)>>>;

    struct Rig {
        dir: tempfile::TempDir,
        ctx: Arc<Ctx>,
        adapter: Arc<QuickShareAdapter>,
        events: Arc<Mutex<Vec<Event>>>,
        offers: Offers,
    }

    async fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            if let Some(t) = f() {
                return t;
            }
            assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    impl Rig {
        fn new(device_name: &str) -> Rig {
            let dir = tempfile::tempdir().unwrap();
            let offers: Offers = Arc::default();
            let o = offers.clone();
            let consent = ConsentBroker::with_limits(
                Arc::new(move |e| {
                    if let ConsentEvent::Pending { id, offer } = e {
                        o.lock().unwrap().push((id, offer));
                    }
                }),
                Duration::from_secs(10),
                2,
            );
            let events: Arc<Mutex<Vec<Event>>> = Arc::default();
            let e = events.clone();
            let ctx = Arc::new(Ctx::new(
                Settings {
                    device_name: device_name.to_owned(),
                    ..Settings::default()
                },
                "Test Phone".into(),
                Store::open(&dir.path().join("data")).unwrap(),
                Inbox::open(&dir.path().join("dl")).unwrap(),
                consent,
                ReachPolicy {
                    allow_loopback: true,
                },
                Arc::new(move |ev| e.lock().unwrap().push(ev)),
            ));
            let adapter = adapter_with(
                ctx.clone(),
                Options {
                    mdns: false,
                    listen: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
                    system_bus: None,
                    timeouts: Timeouts {
                        handshake: Duration::from_secs(5),
                        idle: Duration::from_secs(5),
                        connect: Duration::from_secs(2),
                        send_consent: Duration::from_secs(10),
                    },
                },
            );
            Rig {
                dir,
                ctx,
                adapter,
                events,
                offers,
            }
        }

        async fn offer(&self, n: usize) -> (u64, Arc<Offer>) {
            wait_for("an offer", || self.offers.lock().unwrap().get(n).cloned()).await
        }

        fn answer(&self, id: u64, accept: bool) {
            let d = if accept {
                Decision::Accept
            } else {
                Decision::Decline
            };
            assert!(self.ctx.consent().answer(id, d));
        }

        async fn finished(&self, id: TransferId) -> (Outcome, Vec<String>) {
            wait_for("the transfer to finish", || {
                self.events.lock().unwrap().iter().find_map(|e| match e {
                    Event::TransferFinished {
                        transfer,
                        outcome,
                        saved,
                    } if *transfer == id => Some((outcome.clone(), saved.clone())),
                    _ => None,
                })
            })
            .await
        }

        async fn nth_incoming(&self, n: usize) -> TransferId {
            wait_for("an incoming transfer", || {
                self.events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|e| match e {
                        Event::TransferStarted { transfer }
                            if transfer.direction == sukkula_engine::api::Direction::Incoming =>
                        {
                            Some(transfer.id)
                        }
                        _ => None,
                    })
                    .nth(n)
            })
            .await
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quick_share_keeps_names_texts_aliases_and_the_pin_out_of_the_log() {
        let _flow = begin().await;
        let receiver = Rig::new(RECEIVER_ALIAS);
        let sender = Rig::new(ALIAS);
        receiver.adapter.start_receiving().await.unwrap();
        let port = receiver.adapter.local_port().await.unwrap();
        let to = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
        let peer = sender
            .adapter
            .insert_peer(*b"Rcvr", to, RECEIVER_ALIAS)
            .unwrap();
        let target = || SendTarget::QuickShare { peer: peer.clone() };
        let path = sender.dir.path().join(FILE);
        let data = vec![0x5a_u8; 1_234_567];
        #[allow(clippy::disallowed_methods)] // The scene: a file to send.
        std::fs::write(&path, &data).unwrap();
        let file = || {
            Outgoing::File(OutgoingFile {
                path: path.clone(),
                name: sukkula_core::name::sanitize(FILE),
                size: u64::try_from(data.len()).unwrap(),
                mime: None,
            })
        };
        let mut pins = Vec::new();

        // A file.
        let sent = sender.adapter.send(target(), vec![file()]).await.unwrap();
        let (id, offer) = receiver.offer(0).await;
        assert_eq!(offer.sender, ALIAS);
        pins.push(offer.pin.clone().unwrap());
        receiver.answer(id, true);
        let incoming = receiver.nth_incoming(0).await;
        assert_eq!(receiver.finished(incoming).await.1, vec![FILE.to_owned()]);
        assert_eq!(sender.finished(sent).await.0, Outcome::Done);

        // A text.
        let sent = sender
            .adapter
            .send(target(), vec![Outgoing::Text(TEXT.into())])
            .await
            .unwrap();
        let (id, offer) = receiver.offer(1).await;
        pins.push(offer.pin.clone().unwrap());
        receiver.answer(id, true);
        assert_eq!(sender.finished(sent).await.0, Outcome::Done);
        wait_for("the text", || {
            receiver
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, Event::TextReceived { text, .. } if text == TEXT))
                .then_some(())
        })
        .await;

        // One the user declines.
        let sent = sender.adapter.send(target(), vec![file()]).await.unwrap();
        let (id, offer) = receiver.offer(2).await;
        pins.push(offer.pin.clone().unwrap());
        receiver.answer(id, false);
        assert!(!matches!(sender.finished(sent).await.0, Outcome::Done));
        receiver.adapter.stop_receiving().await;

        let mut watch = Watch::default();
        watch
            .private(ALIAS)
            .private(RECEIVER_ALIAS)
            .private(FILE)
            .private(TEXT)
            .private(receiver.dir.path().to_string_lossy())
            .private(sender.dir.path().to_string_lossy())
            .private("127.0.0.1");
        watch.private_words = pins;
        judge("Quick Share", &captured(), &watch, &["rqs_lib"]);
    }
}

// ------------------------------------------------------------ the engine's own log

/// Two engines trade a wormhole text through the JSON contract, first with
/// logging off and then on, and their log lines -- what would reach the
/// journal -- are read back.
#[cfg(feature = "wormhole")]
#[test]
fn the_engines_own_log_holds_to_s9_off_and_on() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use sukkula_engine::Engine;
    use sukkula_engine::api::{API_VERSION, Event, Outcome, StartConfig};
    use sukkula_engine::ctx::EventSink;
    use sukkula_engine::logging::LogSink;
    use wormhole_support::*;

    type Shared<T> = Arc<Mutex<Vec<T>>>;

    fn engine(dir: &std::path::Path, lines: &Shared<String>) -> (Engine, Shared<Event>) {
        let events: Shared<Event> = Arc::default();
        let e = events.clone();
        let sink: EventSink = Arc::new(move |ev| e.lock().unwrap().push(ev));
        let l = lines.clone();
        let log: LogSink = Arc::new(move |line| l.lock().unwrap().push(line.to_owned()));
        let cfg = StartConfig {
            v: API_VERSION,
            data_dir: dir.join("data").to_string_lossy().into_owned(),
            download_dir: dir.join("dl").to_string_lossy().into_owned(),
            device_model: Some("Test".into()),
            allow_loopback: true,
        };
        (Engine::start_with_log(cfg, sink, log).unwrap(), events)
    }

    fn wait(events: &Shared<Event>, f: impl Fn(&Event) -> bool) -> Event {
        let start = Instant::now();
        loop {
            if let Some(e) = events.lock().unwrap().iter().find(|e| f(e)) {
                return e.clone();
            }
            assert!(start.elapsed() < DEADLINE, "no such event");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let _flow = FLOW.blocking_lock();
    let fakes = tokio::runtime::Runtime::new().unwrap();
    let mb = fakes.block_on(mailbox(Behaviour::default()));
    let rl = fakes.block_on(relay(RelayMode::Honest));

    for (round, logging) in [(1u64, false), (2, true)] {
        let lines: Shared<String> = Arc::default();
        let da = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let (ea, la) = engine(da.path(), &lines);
        let (eb, lb) = engine(db.path(), &lines);
        let set = |alias: &str| {
            format!(
                r#"{{"v":1,"id":1,"cmd":{{"type":"set_settings","settings":{{"device_name":"{alias}","logging":{logging},"localsend":{{"pin":"Q7Z4K9W2"}},"wormhole":{{"mailbox_url":"{}","relay_url":"{}"}}}}}}}}"#,
                mb.url, rl.url
            )
        };
        ea.command_json(&set(ALIAS));
        eb.command_json(&set(RECEIVER_ALIAS));
        for log in [&la, &lb] {
            wait(log, |e| {
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
        let text = format!("{TEXT} round {round}");
        ea.command_json(&format!(
            r#"{{"v":1,"id":2,"cmd":{{"type":"send","target":{{"protocol":"wormhole"}},"items":[{{"kind":"text","text":"{text}"}}]}}}}"#
        ));
        let code = match wait(&la, |e| matches!(e, Event::WormholeCode { .. })) {
            Event::WormholeCode { code, .. } => code,
            _ => unreachable!(),
        };
        eb.command_json(&format!(
            r#"{{"v":1,"id":3,"cmd":{{"type":"receive_wormhole","code":"{code}"}}}}"#
        ));
        let offer = match wait(&lb, |e| matches!(e, Event::OfferPending { .. })) {
            Event::OfferPending { offer } => offer,
            _ => unreachable!(),
        };
        eb.command_json(&format!(
            r#"{{"v":1,"id":4,"cmd":{{"type":"answer","offer":{},"accept":true}}}}"#,
            offer.id
        ));
        wait(
            &lb,
            |e| matches!(e, Event::TextReceived { text: t, .. } if *t == text),
        );
        wait(&la, |e| {
            matches!(
                e,
                Event::TransferFinished {
                    outcome: Outcome::Done,
                    ..
                }
            )
        });
        ea.stop();
        eb.stop();

        let lines = lines.lock().unwrap().clone();
        let password = code.split_once('-').unwrap().1;
        let dir_a = da.path().to_string_lossy().into_owned();
        let dir_b = db.path().to_string_lossy().into_owned();
        for line in &lines {
            let level = line
                .strip_prefix("sukkula: ")
                .and_then(|l| l.split(' ').next())
                .unwrap_or_else(|| panic!("not a log line: {line:?}"));
            assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
            if !logging {
                assert!(
                    level == "WARN" || level == "ERROR",
                    "logging off, yet: {line}"
                );
            }
            for m in [
                ALIAS,
                RECEIVER_ALIAS,
                text.as_str(),
                TEXT,
                code.as_str(),
                password,
                "Q7Z4K9W2",
                dir_a.as_str(),
                dir_b.as_str(),
            ] {
                assert!(!line.contains(m), "{m:?} in the engine's log: {line}");
            }
        }
        if logging {
            for want in [
                "DEBUG sukkula_engine::ctx: offer accepted",
                "INFO sukkula_engine::logging: debug logging on",
            ] {
                assert!(
                    lines.iter().any(|l| l.contains(want)),
                    "{want:?} missing from {lines:#?}"
                );
            }
        }
    }
}
