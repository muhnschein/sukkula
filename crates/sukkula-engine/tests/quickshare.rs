//! The Quick Share adapter (F-QS1, F-QS3, F-QS4, F-QS5, F-C4, F-C5) over
//! 127.0.0.1, offline: a Sukkula sender and a Sukkula receiver, and a
//! hostile sender that speaks the wire protocol itself -- its own UKEY2
//! handshake, its own frames built with prost -- so that it can send what
//! no well-behaved sender would.
//!
//! Every hostile case must end in a clean refusal: nothing written outside
//! the download directory, nothing before consent, no partial file left, no
//! unbounded allocation, no panic, no hang. "No panic" is checked by a panic
//! hook that counts panics on the runtime's worker threads, where the
//! adapter's tasks run (the tests' own assertions run on the test thread);
//! "no hang" by a deadline on every wait; memory by the receiver hanging up
//! on a length prefix it will not honour before the body is even sent.
//!
//! mDNS is off (`Options::mdns`), so nothing leaves loopback.

#![cfg(feature = "quickshare")]
// Fixture code outside `#[test]` functions, where clippy's test allowances
// do not reach. A fixture that cannot be built is a broken test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use prost::Message as _;
use rqs_lib::location_nearby_connections as lnc;
use rqs_lib::location_nearby_connections::payload_transfer_frame::{
    PacketType, PayloadChunk, PayloadHeader, payload_header::PayloadType,
};
use rqs_lib::securegcm::{
    self, DeviceToDeviceMessage, GcmMetadata, Ukey2ClientFinished, Ukey2ClientInit,
    Ukey2HandshakeCipher, Ukey2Message, Ukey2ServerInit, ukey2_client_init::CipherCommitment,
    ukey2_message,
};
use rqs_lib::securemessage::{
    EcP256PublicKey, EncScheme, GenericPublicKey, Header, HeaderAndBody, PublicKeyType,
    SecureMessage, SigScheme,
};
use rqs_lib::sharing_nearby::{
    self as sn, FileMetadata, IntroductionFrame, TextMetadata, WifiCredentialsMetadata,
    connection_response_frame::Status,
};
use rqs_lib::utils;
use sha2::{Digest, Sha256, Sha512};
use sukkula_core::config::{Settings, Visibility};
use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision};
use sukkula_core::inbox::{Inbox, STAGING_DIR};
use sukkula_core::name;
use sukkula_core::offer::Offer;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_engine::adapter::{Adapter, Outgoing, OutgoingFile};
use sukkula_engine::api::{ErrorCode, Event, Outcome, SendTarget, TransferId};
use sukkula_engine::ctx::Ctx;
use sukkula_engine::quickshare::{Options, QuickShareAdapter, Timeouts, adapter_with};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

type HmacSha256 = Hmac<Sha256>;

/// The longest any one wait in these tests may take before it is a hang.
const DEADLINE: Duration = Duration::from_secs(20);

// ------------------------------------------------------------ panics

static WORKER_PANICS: AtomicUsize = AtomicUsize::new(0);

/// Counts panics on tokio worker threads -- where the adapter's tasks run in
/// a multi-thread runtime -- and still prints them.
fn watch_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let worker = std::thread::current()
                .name()
                .is_some_and(|n| n.starts_with("tokio-runtime-worker"));
            if worker {
                WORKER_PANICS.fetch_add(1, Ordering::SeqCst);
            }
            previous(info);
        }));
    });
}

fn assert_no_panics() {
    assert_eq!(
        WORKER_PANICS.load(Ordering::SeqCst),
        0,
        "a task of the adapter panicked"
    );
}

// ------------------------------------------------------------ the rig

/// What a rig saw: the engine's events and the consent queue's.
#[derive(Default)]
struct Seen {
    events: Mutex<Vec<Event>>,
    consent: Mutex<Vec<ConsentEvent>>,
}

impl Seen {
    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn consent(&self) -> Vec<ConsentEvent> {
        self.consent.lock().unwrap().clone()
    }

    fn offers(&self) -> Vec<(u64, Arc<Offer>)> {
        self.consent()
            .into_iter()
            .filter_map(|e| match e {
                ConsentEvent::Pending { id, offer } => Some((id, offer)),
                ConsentEvent::Closed { .. } => None,
            })
            .collect()
    }
}

/// Waits until `f` finds something, or panics after [`DEADLINE`]. The event
/// sink is called from the adapter's tasks, so everything here is polled.
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

struct Rig {
    dir: tempfile::TempDir,
    ctx: Arc<Ctx>,
    adapter: Arc<QuickShareAdapter>,
    seen: Arc<Seen>,
}

fn quick() -> Timeouts {
    Timeouts {
        handshake: Duration::from_secs(5),
        idle: Duration::from_secs(5),
        connect: Duration::from_secs(2),
        send_consent: Duration::from_secs(10),
    }
}

impl Rig {
    fn new(device_name: &str) -> Rig {
        Rig::with(device_name, quick(), Duration::from_secs(10), |_| {})
    }

    fn with(
        device_name: &str,
        timeouts: Timeouts,
        consent_timeout: Duration,
        settings: impl FnOnce(&mut Settings),
    ) -> Rig {
        watch_panics();
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Seen::default());
        let s = seen.clone();
        let consent = ConsentBroker::with_limits(
            Arc::new(move |e| s.consent.lock().unwrap().push(e)),
            consent_timeout,
            2,
        );
        let mut set = Settings {
            device_name: device_name.to_owned(),
            ..Settings::default()
        };
        settings(&mut set);
        let s = seen.clone();
        let ctx = Arc::new(Ctx::new(
            set,
            "Test Phone".into(),
            Store::open(&dir.path().join("data")).unwrap(),
            Inbox::open(&dir.path().join("dl")).unwrap(),
            consent,
            ReachPolicy {
                allow_loopback: true,
            },
            Arc::new(move |e| s.events.lock().unwrap().push(e)),
        ));
        let adapter = adapter_with(
            ctx.clone(),
            Options {
                mdns: false,
                listen: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
                system_bus: None,
                timeouts,
            },
        );
        Rig {
            dir,
            ctx,
            adapter,
            seen,
        }
    }

    async fn listen(&self) -> SocketAddr {
        self.adapter.start_receiving().await.unwrap();
        let port = self.adapter.local_port().await.expect("listening");
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
    }

    fn download_dir(&self) -> PathBuf {
        self.dir.path().join("dl")
    }

    /// The download directory's entries, not counting the (empty) staging
    /// directory, and asserting that nothing else appeared anywhere in the
    /// rig's directory and nothing is left half-written.
    fn received(&self) -> BTreeSet<String> {
        let top: BTreeSet<String> = std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            top,
            ["data", "dl"].iter().map(|s| s.to_string()).collect(),
            "something was written outside the download directory"
        );
        let staging = self.download_dir().join(STAGING_DIR);
        if staging.exists() {
            let left: Vec<_> = std::fs::read_dir(&staging).unwrap().collect();
            assert!(left.is_empty(), "a partial file was left behind: {left:?}");
        }
        std::fs::read_dir(self.download_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != STAGING_DIR)
            .collect()
    }

    fn answer(&self, id: u64, accept: bool) {
        let decision = if accept {
            Decision::Accept
        } else {
            Decision::Decline
        };
        assert!(
            self.ctx.consent().answer(id, decision),
            "offer {id} is gone"
        );
    }

    /// The next offer the user is asked about, after `seen` earlier ones.
    async fn offer(&self, seen: usize) -> (u64, Arc<Offer>) {
        wait_for("an offer", || self.seen.offers().into_iter().nth(seen)).await
    }

    async fn finished(&self, id: TransferId) -> (Outcome, Vec<String>) {
        wait_for("the transfer to finish", || {
            self.seen.events().into_iter().find_map(|e| match e {
                Event::TransferFinished {
                    transfer,
                    outcome,
                    saved,
                } if transfer == id => Some((outcome, saved)),
                _ => None,
            })
        })
        .await
    }

    /// The one incoming transfer, once it started.
    async fn incoming(&self) -> TransferId {
        wait_for("an incoming transfer", || {
            self.seen.events().into_iter().find_map(|e| match e {
                Event::TransferStarted { transfer }
                    if transfer.direction == sukkula_engine::api::Direction::Incoming =>
                {
                    Some(transfer.id)
                }
                _ => None,
            })
        })
        .await
    }

    fn file(&self, name: &str, content: &[u8]) -> OutgoingFile {
        let dir = self.dir.path().join("data").join("out");
        #[allow(clippy::disallowed_methods)] // The scene: where files to send live.
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        write_file(&path, content);
        OutgoingFile {
            path,
            name: name::sanitize(name),
            size: content.len() as u64,
            mime: None,
        }
    }

    /// Sends `items` to the receiver at `to`, as if discovery had found it.
    async fn send(&self, to: SocketAddr, items: Vec<Outgoing>) -> TransferId {
        let peer = self
            .adapter
            .insert_peer(*b"Rcvr", to, "Receiver")
            .expect("peer added");
        self.adapter
            .send(SendTarget::QuickShare { peer }, items)
            .await
            .unwrap()
    }
}

// Test fixtures write their own input files; S3's ban is for the engine.
#[allow(clippy::disallowed_methods)]
fn write_file(path: &Path, content: &[u8]) {
    std::fs::write(path, content).unwrap();
}

fn pseudo_random(len: usize, seed: u8) -> Vec<u8> {
    let mut x = u32::from(seed) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

// ------------------------------------------------------------ Sukkula to Sukkula

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_go_from_sukkula_to_sukkula() {
    let receiver = Rig::new("Receiver Phone");
    let sender = Rig::new("Sender Phone");
    let to = receiver.listen().await;

    let big = pseudo_random(300_000, 7);
    let items = vec![
        Outgoing::File(sender.file("photo.jpg", &big)),
        Outgoing::File(sender.file("empty.txt", b"")),
        Outgoing::File(sender.file("notes.txt", b"hello")),
    ];
    let sent = sender.send(to, items).await;

    // F-C2, F-QS3: the user sees who, what, how much, and the PIN.
    let (offer_id, offer) = receiver.offer(0).await;
    assert_eq!(offer.sender, "Sender Phone");
    let names: Vec<&str> = offer.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["photo.jpg", "empty.txt", "notes.txt"]);
    assert_eq!(offer.total_bytes, 300_005);
    let pin = offer.pin.clone().expect("a PIN");
    assert_eq!(pin.len(), 4);
    assert!(pin.bytes().all(|b| b.is_ascii_digit()));
    // S5: nothing on disk before the answer.
    assert!(receiver.received().is_empty());
    receiver.answer(offer_id, true);

    let incoming = receiver.incoming().await;
    let (outcome, saved) = receiver.finished(incoming).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved, ["photo.jpg", "empty.txt", "notes.txt"]);
    assert_eq!(sender.finished(sent).await.0, Outcome::Done);

    let dl = receiver.download_dir();
    assert_eq!(std::fs::read(dl.join("photo.jpg")).unwrap(), big);
    assert_eq!(std::fs::read(dl.join("empty.txt")).unwrap(), b"");
    assert_eq!(std::fs::read(dl.join("notes.txt")).unwrap(), b"hello");
    assert_eq!(receiver.received().len(), 3);

    // Progress reached the total on both sides.
    for (rig, id) in [(&receiver, incoming), (&sender, sent)] {
        let last = rig.seen.events().into_iter().rev().find_map(|e| match e {
            Event::TransferProgress {
                transfer,
                bytes,
                total,
            } if transfer == id => Some((bytes, total)),
            _ => None,
        });
        assert_eq!(last, Some((300_005, 300_005)));
    }
    assert_no_panics();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_goes_from_sukkula_to_sukkula() {
    let receiver = Rig::new("Receiver Phone");
    let sender = Rig::new("Sender Phone");
    let to = receiver.listen().await;

    let text = "Hi! https://example.com/\u{202e}exe.txt\nsecond line";
    let sent = sender.send(to, vec![Outgoing::Text(text.into())]).await;
    let (offer_id, offer) = receiver.offer(0).await;
    assert!(offer.files.is_empty());
    let preview = offer.text.clone().expect("a preview");
    assert!(!preview.contains('\u{202e}'), "S2 applies to the preview");
    assert!(offer.pin.is_some());
    receiver.answer(offer_id, true);

    let incoming = receiver.incoming().await;
    assert_eq!(receiver.finished(incoming).await.0, Outcome::Done);
    assert_eq!(sender.finished(sent).await.0, Outcome::Done);
    // F-C4: the text as text, after S2 -- the bidi override is gone, the
    // line break stays -- and nothing on disk.
    let got = receiver
        .seen
        .events()
        .into_iter()
        .find_map(|e| match e {
            Event::TextReceived {
                transfer,
                from,
                text,
            } if transfer == incoming => Some((from, text)),
            _ => None,
        })
        .expect("the text");
    assert_eq!(got.0, "Sender Phone");
    assert_eq!(got.1, "Hi! https://example.com/exe.txt\nsecond line");
    assert!(receiver.received().is_empty());
    assert_no_panics();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decline_reaches_the_sender() {
    let receiver = Rig::new("Receiver Phone");
    let sender = Rig::new("Sender Phone");
    let to = receiver.listen().await;
    let sent = sender
        .send(to, vec![Outgoing::File(sender.file("a.txt", b"abc"))])
        .await;
    let (id, _) = receiver.offer(0).await;
    receiver.answer(id, false);
    match sender.finished(sent).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Refused),
        other => panic!("{other:?}"),
    }
    assert!(receiver.received().is_empty());
    assert_no_panics();
}

/// F-C5: the receiver cancels mid-transfer; both sides end as cancelled,
/// and the partial file is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_receiver_cancels() {
    let receiver = Rig::new("Receiver Phone");
    let sender = Rig::new("Sender Phone");
    let to = receiver.listen().await;
    let big = pseudo_random(24 << 20, 3);
    let sent = sender
        .send(to, vec![Outgoing::File(sender.file("big.bin", &big))])
        .await;
    let (id, _) = receiver.offer(0).await;
    receiver.answer(id, true);
    let incoming = receiver.incoming().await;
    wait_for("progress", || {
        receiver.seen.events().into_iter().find(
            |e| matches!(e, Event::TransferProgress { transfer, .. } if *transfer == incoming),
        )
    })
    .await;
    assert!(receiver.ctx.transfers().cancel(incoming));
    assert_eq!(receiver.finished(incoming).await.0, Outcome::Cancelled);
    assert_eq!(sender.finished(sent).await.0, Outcome::Cancelled);
    assert!(receiver.received().is_empty());
    assert_no_panics();
}

/// F-C5: the sender cancels mid-transfer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sender_cancels() {
    let receiver = Rig::new("Receiver Phone");
    let sender = Rig::new("Sender Phone");
    let to = receiver.listen().await;
    let big = pseudo_random(24 << 20, 5);
    let sent = sender
        .send(to, vec![Outgoing::File(sender.file("big.bin", &big))])
        .await;
    let (id, _) = receiver.offer(0).await;
    receiver.answer(id, true);
    wait_for("progress", || {
        sender
            .seen
            .events()
            .into_iter()
            .find(|e| matches!(e, Event::TransferProgress { transfer, .. } if *transfer == sent))
    })
    .await;
    assert!(sender.ctx.transfers().cancel(sent));
    assert_eq!(sender.finished(sent).await.0, Outcome::Cancelled);
    let incoming = receiver.incoming().await;
    assert_eq!(receiver.finished(incoming).await.0, Outcome::Cancelled);
    assert!(receiver.received().is_empty());
    assert_no_panics();
}

/// F-QS4: hidden means nothing listens at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hidden_means_no_listener() {
    let rig = Rig::with("Hidden", quick(), Duration::from_secs(10), |s| {
        s.quickshare.visibility = Visibility::Hidden;
    });
    rig.adapter.start_receiving().await.unwrap();
    assert_eq!(rig.adapter.local_port().await, None);
    // Everyone, after a settings change: a listener.
    let mut s = rig.ctx.settings();
    s.quickshare.visibility = Visibility::Everyone;
    rig.ctx.set_settings(s);
    rig.adapter.start_receiving().await.unwrap();
    let port = rig.adapter.local_port().await.expect("listening");
    // And off again: the port is closed.
    rig.adapter.stop_receiving().await;
    assert_eq!(rig.adapter.local_port().await, None);
    assert!(
        TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_err()
    );
}

/// Peers are reported as PeerFound, with showable names and plain-ASCII
/// ids, at most MAX_PEERS of them and MAX_PEERS_PER_SOURCE from one
/// source; a full table still takes a device from a source of its own,
/// and a listed id stays its source's (kept[6]); a new round of discovery
/// reports the old ones lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peers_are_bounded_and_reported() {
    use std::collections::HashSet;
    use sukkula_core::limits::MAX_PEERS;
    const PER_SOURCE: usize = 4;
    let rig = Rig::new("Looking");
    let from = |s: usize| {
        SocketAddr::new(
            Ipv4Addr::new(127, 3, (s / 250) as u8, (s % 250) as u8 + 1).into(),
            9,
        )
    };
    let endpoint = |i: usize| {
        [
            b'a',
            b'0' + (i / 100 % 10) as u8,
            b'0' + (i / 10 % 10) as u8,
            b'0' + (i % 10) as u8,
        ]
    };
    let listed = |rig: &Rig| {
        let mut listed = HashSet::new();
        for e in rig.seen.events() {
            match e {
                Event::PeerFound { peer } => {
                    listed.insert(peer.id);
                }
                Event::PeerLost { peer } => {
                    listed.remove(&peer);
                }
                _ => {}
            }
        }
        listed
    };

    // One source, announcing without end: its newest few are listed.
    for i in 0..(MAX_PEERS + 10) {
        assert!(
            rig.adapter
                .insert_peer(endpoint(i), from(0), "\u{202e}Evil\nPhone")
                .is_some()
        );
    }
    assert_eq!(listed(&rig).len(), PER_SOURCE);
    // Enough sources to fill the table.
    let mut n = MAX_PEERS + 10;
    for source in 1..=MAX_PEERS / PER_SOURCE {
        for _ in 0..PER_SOURCE {
            let _ = rig.adapter.insert_peer(endpoint(n), from(source), "Phone");
            n += 1;
        }
    }
    assert_eq!(listed(&rig).len(), MAX_PEERS);
    // A device announcing from an address of its own still gets in.
    assert_eq!(
        rig.adapter
            .insert_peer(*b"Hnst", from(900), "Honest")
            .as_deref(),
        Some("qs:Hnst")
    );
    assert!(listed(&rig).contains("qs:Hnst"));
    assert_eq!(listed(&rig).len(), MAX_PEERS);
    // Its id announced from elsewhere does not move it.
    assert_eq!(rig.adapter.insert_peer(*b"Hnst", from(901), "Honest"), None);
    // A peer outside the reach policy is never listed.
    let public = SocketAddr::new(Ipv4Addr::new(8, 8, 8, 8).into(), 9);
    assert_eq!(rig.adapter.insert_peer(*b"zzzz", public, "x"), None);
    for e in rig.seen.events() {
        if let Event::PeerFound { peer: p } = e {
            assert!(p.id.is_ascii() && p.id.starts_with("qs:"), "{}", p.id);
            assert!(
                !p.name.contains('\u{202e}') && !p.name.contains('\n'),
                "{:?}",
                p.name
            );
        }
    }
    let before = rig.seen.events().len();
    rig.adapter.start_discovery().await.unwrap();
    let lost = rig.seen.events()[before..]
        .iter()
        .filter(|e| matches!(e, Event::PeerLost { .. }))
        .count();
    assert_eq!(lost, MAX_PEERS);
    rig.adapter.stop_discovery().await;
}

/// A file to send is checked where it is opened: a FIFO swapped in for the
/// file, or a file that changed size, is refused, never blocks, never sends
/// more than the size the receiver was promised.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_swapped_after_the_check_is_refused() {
    let receiver = Rig::new("Receiver Phone");
    let sender = Rig::new("Sender Phone");
    let to = receiver.listen().await;

    for swap in ["fifo", "grown"] {
        let file = sender.file(&format!("{swap}.bin"), b"abc");
        #[allow(clippy::disallowed_methods)] // The scene: the file is swapped.
        std::fs::remove_file(&file.path).unwrap();
        #[allow(clippy::disallowed_methods)] // The scene: for a FIFO.
        match swap {
            "fifo" => rustix::fs::mknodat(
                rustix::fs::CWD,
                &file.path,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o600),
                0,
            )
            .unwrap(),
            _ => write_file(&file.path, b"abcdef"),
        }
        let before = receiver.seen.offers().len();
        let sent = sender.send(to, vec![Outgoing::File(file)]).await;
        let (id, _) = receiver.offer(before).await;
        receiver.answer(id, true);
        match sender.finished(sent).await.0 {
            Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::BadFile, "{swap}"),
            other => panic!("{swap}: {other:?}"),
        }
    }
    // The receiver got nothing it can keep.
    wait_for("the receiver to give up", || {
        let done = receiver
            .seen
            .events()
            .iter()
            .filter(|e| matches!(e, Event::TransferFinished { .. }))
            .count();
        (done == 2).then_some(())
    })
    .await;
    assert!(receiver.received().is_empty());
    assert_no_panics();
}

// ------------------------------------------------------------ the hostile sender

/// A sender that does the UKEY2 handshake itself and then sends whatever
/// the test tells it to. Built from rqs_lib's protobuf types and crypto
/// helpers only, not from its sender.
struct Evil {
    socket: TcpStream,
    client_seq: i32,
    server_seq: i32,
    encrypt_key: Vec<u8>,
    send_hmac: Vec<u8>,
    decrypt_key: Vec<u8>,
    recv_hmac: Vec<u8>,
    pin: String,
}

async fn write_raw(socket: &mut TcpStream, bytes: &[u8]) {
    let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(bytes);
    socket.write_all(&framed).await.unwrap();
}

async fn read_raw(socket: &mut TcpStream) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    tokio::time::timeout(DEADLINE, socket.read_exact(&mut len))
        .await
        .expect("the receiver neither answered nor hung up")
        .ok()?;
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    socket.read_exact(&mut body).await.ok()?;
    Some(body)
}

fn offline(v1: lnc::V1Frame) -> lnc::OfflineFrame {
    lnc::OfflineFrame {
        version: Some(lnc::offline_frame::Version::V1.into()),
        v1: Some(v1),
    }
}

fn sharing(kind: sn::v1_frame::FrameType, fill: impl FnOnce(&mut sn::V1Frame)) -> sn::Frame {
    let mut v1 = sn::V1Frame {
        r#type: Some(kind.into()),
        ..Default::default()
    };
    fill(&mut v1);
    sn::Frame {
        version: Some(sn::frame::Version::V1.into()),
        v1: Some(v1),
    }
}

fn payload_frame(
    kind: PayloadType,
    id: i64,
    total: i64,
    offset: i64,
    body: &[u8],
    last: bool,
) -> lnc::OfflineFrame {
    offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::PayloadTransfer.into()),
        payload_transfer: Some(lnc::PayloadTransferFrame {
            packet_type: Some(PacketType::Data.into()),
            payload_header: Some(PayloadHeader {
                id: Some(id),
                r#type: Some(kind.into()),
                total_size: Some(total),
                ..Default::default()
            }),
            payload_chunk: Some(PayloadChunk {
                offset: Some(offset),
                flags: Some(i32::from(last)),
                body: Some(body.to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn endpoint_info(name: &str) -> Vec<u8> {
    let mut info = vec![1u8 << 1];
    info.extend_from_slice(&[7u8; 16]);
    info.push(name.len() as u8);
    info.extend_from_slice(name.as_bytes());
    info
}

fn public_key(key: &p256::PublicKey) -> Vec<u8> {
    let point = key.to_encoded_point(false);
    GenericPublicKey {
        r#type: PublicKeyType::EcP256.into(),
        ec_p256_public_key: Some(EcP256PublicKey {
            x: utils::encode_point(point.x().unwrap().to_vec().into()).unwrap(),
            y: utils::encode_point(point.y().unwrap().to_vec().into()).unwrap(),
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

/// Source addresses handed out so far. Every hostile connection comes from
/// an address of its own in 127/8 (all of which is loopback on Linux), so
/// that the per-address offer rate (S7) and connection caps, which these
/// tests do not test, do not get in the way of those they do.
static NEXT_SOURCE: AtomicUsize = AtomicUsize::new(0);

async fn connect_from(to: SocketAddr, from: Ipv4Addr) -> TcpStream {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(SocketAddr::new(from.into(), 0)).unwrap();
    socket.connect(to).await.unwrap()
}

impl Evil {
    async fn connect(to: SocketAddr) -> TcpStream {
        let n = NEXT_SOURCE.fetch_add(1, Ordering::SeqCst);
        let from = Ipv4Addr::new(127, 1, (n / 250) as u8, (n % 250) as u8 + 2);
        connect_from(to, from).await
    }

    /// The whole handshake, as a real sender does it, up to (not including)
    /// the introduction.
    async fn handshake(to: SocketAddr, name: &str) -> Evil {
        let mut socket = Evil::connect(to).await;
        let request = offline(lnc::V1Frame {
            r#type: Some(lnc::v1_frame::FrameType::ConnectionRequest.into()),
            connection_request: Some(lnc::ConnectionRequestFrame {
                endpoint_id: Some("Evil".into()),
                endpoint_info: Some(endpoint_info(name)),
                ..Default::default()
            }),
            ..Default::default()
        });
        write_raw(&mut socket, &request.encode_to_vec()).await;

        let (secret, public) = utils::gen_ecdsa_keypair();
        let finish = Ukey2Message {
            message_type: Some(ukey2_message::Type::ClientFinish.into()),
            message_data: Some(
                Ukey2ClientFinished {
                    public_key: Some(public_key(&public)),
                }
                .encode_to_vec(),
            ),
        }
        .encode_to_vec();
        let init = Ukey2Message {
            message_type: Some(ukey2_message::Type::ClientInit.into()),
            message_data: Some(
                Ukey2ClientInit {
                    version: Some(1),
                    random: Some(utils::gen_random(32)),
                    next_protocol: Some("AES_256_CBC-HMAC_SHA256".into()),
                    cipher_commitments: vec![CipherCommitment {
                        handshake_cipher: Some(Ukey2HandshakeCipher::P256Sha512.into()),
                        commitment: Some(Sha512::digest(&finish).to_vec()),
                    }],
                }
                .encode_to_vec(),
            ),
        }
        .encode_to_vec();
        write_raw(&mut socket, &init).await;
        let server_init = read_raw(&mut socket).await.expect("ServerInit");
        let msg = Ukey2Message::decode(&*server_init).unwrap();
        assert_eq!(msg.message_type(), ukey2_message::Type::ServerInit);
        let si = Ukey2ServerInit::decode(msg.message_data()).unwrap();
        let theirs = GenericPublicKey::decode(si.public_key()).unwrap();
        let theirs = theirs.ec_p256_public_key.unwrap();
        let theirs = utils::decode_p256_point(&theirs.x, &theirs.y).unwrap();
        write_raw(&mut socket, &finish).await;

        let shared = diffie_hellman(secret.to_nonzero_scalar(), theirs.as_affine());
        let derived = Sha256::digest(shared.raw_secret_bytes());
        let mut info = init.clone();
        info.extend_from_slice(&server_init);
        let hkdf = |salt: &[u8], ikm: &[u8], info: &[u8]| {
            utils::hkdf_extract_expand(salt, ikm, info, 32).unwrap()
        };
        let auth = hkdf(b"UKEY2 v1 auth", &derived, &info);
        let next = hkdf(b"UKEY2 v1 next", &derived, &info);
        let d2d_salt =
            hex_decode("82AA55A0D397F88346CA1CEE8D3909B95F13FA7DEB1D4AB38376B8256DA85510");
        let key_salt =
            hex_decode("BF9D2A53C63616D75DB0A7165B91C1EF73E537F2427405FA23610A4BE657642E");
        let client = hkdf(&d2d_salt, &next, b"client");
        let server = hkdf(&d2d_salt, &next, b"server");

        let response = offline(lnc::V1Frame {
            r#type: Some(lnc::v1_frame::FrameType::ConnectionResponse.into()),
            connection_response: Some(lnc::ConnectionResponseFrame {
                response: Some(lnc::connection_response_frame::ResponseStatus::Accept.into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        write_raw(&mut socket, &response.encode_to_vec()).await;
        // The receiver's connection response, in plaintext.
        read_raw(&mut socket).await.expect("ConnectionResponse");

        let mut evil = Evil {
            socket,
            client_seq: 0,
            server_seq: 0,
            encrypt_key: hkdf(&key_salt, &client, b"ENC:2"),
            send_hmac: hkdf(&key_salt, &client, b"SIG:1"),
            decrypt_key: hkdf(&key_salt, &server, b"ENC:2"),
            recv_hmac: hkdf(&key_salt, &server, b"SIG:1"),
            pin: utils::to_four_digit_string(&auth),
        };
        evil.send_sharing(&sharing(
            sn::v1_frame::FrameType::PairedKeyEncryption,
            |v| {
                v.paired_key_encryption = Some(sn::PairedKeyEncryptionFrame {
                    secret_id_hash: Some(vec![0; 6]),
                    signed_data: Some(vec![0; 72]),
                    ..Default::default()
                });
            },
        ))
        .await;
        evil.send_sharing(&sharing(sn::v1_frame::FrameType::PairedKeyResult, |v| {
            v.paired_key_result = Some(sn::PairedKeyResultFrame {
                status: Some(sn::paired_key_result_frame::Status::Unable.into()),
            });
        }))
        .await;
        evil
    }

    fn seal(&mut self, frame: &lnc::OfflineFrame) -> Vec<u8> {
        self.client_seq += 1;
        let d2d = DeviceToDeviceMessage {
            sequence_number: Some(self.client_seq),
            message: Some(frame.encode_to_vec()),
        };
        let iv = utils::gen_random(16);
        let body = utils::aes_cbc_encrypt(&self.encrypt_key, &iv, &d2d.encode_to_vec()).unwrap();
        let hb = HeaderAndBody {
            header: Header {
                signature_scheme: SigScheme::HmacSha256.into(),
                encryption_scheme: EncScheme::Aes256Cbc.into(),
                iv: Some(iv),
                public_metadata: Some(
                    GcmMetadata {
                        r#type: securegcm::Type::DeviceToDeviceMessage.into(),
                        version: Some(1),
                    }
                    .encode_to_vec(),
                ),
                ..Default::default()
            },
            body,
        }
        .encode_to_vec();
        let mut mac = HmacSha256::new_from_slice(&self.send_hmac).unwrap();
        mac.update(&hb);
        SecureMessage {
            header_and_body: hb,
            signature: mac.finalize().into_bytes().to_vec(),
        }
        .encode_to_vec()
    }

    async fn send_offline(&mut self, frame: &lnc::OfflineFrame) {
        let sealed = self.seal(frame);
        write_raw(&mut self.socket, &sealed).await;
    }

    /// Like `send_offline`, but a receiver that already hung up is not an
    /// error: it is often the point.
    async fn try_send_offline(&mut self, frame: &lnc::OfflineFrame) -> bool {
        let sealed = self.seal(frame);
        let mut framed = (sealed.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&sealed);
        self.socket.write_all(&framed).await.is_ok()
    }

    /// A sharing frame as one BYTES payload, as senders send them.
    async fn send_sharing(&mut self, frame: &sn::Frame) {
        let bytes = frame.encode_to_vec();
        let id = i64::from(self.client_seq) + 1000;
        let len = bytes.len() as i64;
        self.send_offline(&payload_frame(
            PayloadType::Bytes,
            id,
            len,
            0,
            &bytes,
            false,
        ))
        .await;
        self.send_offline(&payload_frame(PayloadType::Bytes, id, len, len, &[], true))
            .await;
    }

    async fn introduce(&mut self, intro: IntroductionFrame) {
        self.send_sharing(&sharing(sn::v1_frame::FrameType::Introduction, |v| {
            v.introduction = Some(intro);
        }))
        .await;
    }

    /// The next frame from the receiver, decrypted; `None` once it hung up.
    async fn next(&mut self) -> Option<lnc::OfflineFrame> {
        let raw = read_raw(&mut self.socket).await?;
        let smsg = SecureMessage::decode(&*raw).unwrap();
        let mut mac = HmacSha256::new_from_slice(&self.recv_hmac).unwrap();
        mac.update(&smsg.header_and_body);
        mac.verify_slice(&smsg.signature)
            .expect("the receiver's HMAC");
        let hb = HeaderAndBody::decode(&*smsg.header_and_body).unwrap();
        let plain = utils::aes_cbc_decrypt(&self.decrypt_key, hb.header.iv(), &hb.body).unwrap();
        let d2d = DeviceToDeviceMessage::decode(&*plain).unwrap();
        self.server_seq += 1;
        assert_eq!(d2d.sequence_number(), self.server_seq);
        Some(lnc::OfflineFrame::decode(d2d.message()).unwrap())
    }

    /// The receiver's answer to the introduction: the status of its
    /// Response sharing frame, or `None` if it hung up without one.
    async fn answer(&mut self) -> Option<Status> {
        let mut buffers: std::collections::HashMap<i64, Vec<u8>> = Default::default();
        loop {
            let frame = self.next().await?;
            let Some(v1) = frame.v1 else { continue };
            let Some(pt) = v1.payload_transfer else {
                continue;
            };
            let (Some(header), Some(chunk)) = (pt.payload_header, pt.payload_chunk) else {
                continue;
            };
            let buf = buffers.entry(header.id()).or_default();
            buf.extend_from_slice(chunk.body());
            if chunk.flags() & 1 == 1 {
                let bytes = buffers.remove(&header.id()).unwrap_or_default();
                let f = sn::Frame::decode(&*bytes).unwrap();
                let v1 = f.v1.unwrap();
                if v1.r#type() == sn::v1_frame::FrameType::Response {
                    return Some(v1.connection_response.unwrap().status());
                }
            }
        }
    }

    /// Whether the receiver hangs up within `within`, reading and dropping
    /// whatever it still sends.
    async fn hung_up(&mut self, within: Duration) -> bool {
        let mut buf = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.socket.read(&mut buf)).await {
                Err(_) => return false,
                Ok(Ok(0) | Err(_)) => return true,
                Ok(Ok(_)) => {}
            }
        }
    }
}

fn hex_decode(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn file_meta(id: i64, name: &str, size: i64) -> FileMetadata {
    FileMetadata {
        payload_id: Some(id),
        name: Some(name.into()),
        size: Some(size),
        mime_type: Some("application/octet-stream".into()),
        ..Default::default()
    }
}

fn intro(files: Vec<FileMetadata>) -> IntroductionFrame {
    IntroductionFrame {
        file_metadata: files,
        ..Default::default()
    }
}

/// Whether the socket reads end-of-file (or an error) within `within`.
async fn closed(socket: &mut TcpStream, within: Duration) -> bool {
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, socket.read(&mut buf)).await {
            Err(_) => return false,
            Ok(Ok(0) | Err(_)) => return true,
            Ok(Ok(_)) => {}
        }
    }
}

/// After a hostile case: the receiver still works, for an honest sender.
async fn still_serves(receiver: &Rig, to: SocketAddr) {
    let before = receiver.seen.offers().len();
    let mut evil = Evil::handshake(to, "Honest").await;
    evil.introduce(intro(vec![file_meta(1, "ok.txt", 2)])).await;
    let (id, offer) = receiver.offer(before).await;
    assert_eq!(offer.pin.as_deref(), Some(evil.pin.as_str()), "F-QS3");
    receiver.answer(id, true);
    assert_eq!(evil.answer().await, Some(Status::Accept));
    evil.send_offline(&payload_frame(PayloadType::File, 1, 2, 0, b"ok", false))
        .await;
    evil.send_offline(&payload_frame(PayloadType::File, 1, 2, 2, b"", true))
        .await;
    let transfer = wait_for("the honest transfer", || {
        receiver
            .seen
            .events()
            .into_iter()
            .rev()
            .find_map(|e| match e {
                Event::TransferStarted { transfer } => Some(transfer.id),
                _ => None,
            })
    })
    .await;
    let (outcome, saved) = receiver.finished(transfer).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved, ["ok.txt"]);
    assert!(receiver.received().contains("ok.txt"));
    #[allow(clippy::disallowed_methods)] // Clearing the scene for the next probe.
    std::fs::remove_file(receiver.download_dir().join("ok.txt")).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_names_become_one_safe_component() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;
    let outside = std::env::temp_dir().join(format!(
        "sukkula-qs-escape-{}",
        u32::from_be_bytes(utils::gen_random(4).try_into().unwrap())
    ));
    let names = [
        "../../escape.txt".to_owned(),
        outside.to_string_lossy().into_owned(),
        "a/b\\c.txt".to_owned(),
        "..".to_owned(),
        ".hidden".to_owned(),
        "\u{202e}fdp.exe".to_owned(),
        "nul\0byte.txt".to_owned(),
    ];
    let mut evil = Evil::handshake(to, "Evil").await;
    let files = names
        .iter()
        .enumerate()
        .map(|(i, n)| file_meta(i as i64 + 1, n, 1))
        .collect();
    evil.introduce(intro(files)).await;
    let (id, offer) = receiver.offer(0).await;
    for f in &offer.files {
        let n = f.name.as_str();
        assert!(name::is_safe(n), "{n:?}");
        assert!(
            !n.contains('/') && !n.contains('\\') && !n.starts_with('.'),
            "{n:?}"
        );
    }
    receiver.answer(id, true);
    assert_eq!(evil.answer().await, Some(Status::Accept));
    for i in 1..=names.len() as i64 {
        evil.send_offline(&payload_frame(PayloadType::File, i, 1, 0, b"x", false))
            .await;
        evil.send_offline(&payload_frame(PayloadType::File, i, 1, 1, b"", true))
            .await;
    }
    let incoming = receiver.incoming().await;
    let (outcome, saved) = receiver.finished(incoming).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved.len(), names.len());
    // Every file is in the download directory, under the name the user
    // was shown (made unique), and nowhere else.
    let received = receiver.received();
    assert_eq!(received.len(), names.len());
    for s in &saved {
        assert!(received.contains(s), "{s}");
    }
    assert!(!outside.exists());
    assert_no_panics();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_sizes_are_refused_before_the_user_is_asked() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;
    let over = (8i64 << 30) + 1;
    for files in [
        vec![file_meta(1, "neg.txt", -1)],
        vec![file_meta(1, "min.txt", i64::MIN)],
        vec![file_meta(1, "max.txt", i64::MAX)],
        vec![file_meta(1, "big.txt", over)],
        // Two that are fine alone, over 16 GiB together (S6), and two that
        // wrap to a small total if added as u64 after a negative (Q5).
        vec![
            file_meta(1, "a", 8 << 30),
            file_meta(2, "b", 8 << 30),
            file_meta(3, "c", 1),
        ],
        vec![file_meta(1, "a", 10), file_meta(2, "b", -5)],
        // More files than an offer may have.
        (1..=501).map(|i| file_meta(i, "f", 1)).collect(),
        // The same payload id twice.
        vec![file_meta(1, "a", 1), file_meta(1, "b", 1)],
    ] {
        let mut evil = Evil::handshake(to, "Evil").await;
        evil.introduce(intro(files)).await;
        let status = evil.answer().await;
        assert!(matches!(status, None | Some(Status::Reject)), "{status:?}");
        assert!(evil.hung_up(DEADLINE).await);
    }
    assert!(receiver.seen.offers().is_empty(), "the user was asked");
    assert!(receiver.received().is_empty());
    still_serves(&receiver, to).await;
    assert_no_panics();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wifi_credentials_are_refused() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;
    for with_file in [false, true] {
        let mut evil = Evil::handshake(to, "Evil").await;
        evil.introduce(IntroductionFrame {
            file_metadata: if with_file {
                vec![file_meta(1, "a.txt", 1)]
            } else {
                vec![]
            },
            wifi_credentials_metadata: vec![WifiCredentialsMetadata {
                ssid: Some("FreeWifi".into()),
                security_type: Some(sn::wifi_credentials_metadata::SecurityType::WpaPsk.into()),
                payload_id: Some(9),
                id: Some(9),
            }],
            ..Default::default()
        })
        .await;
        // F-QS5: refused as unsupported, without asking anybody.
        assert_eq!(evil.answer().await, Some(Status::UnsupportedAttachmentType));
        // The password payload that would follow is not taken either.
        let _ = evil
            .try_send_offline(&payload_frame(
                PayloadType::Bytes,
                9,
                8,
                0,
                b"password",
                true,
            ))
            .await;
        assert!(evil.hung_up(DEADLINE).await);
    }
    assert!(receiver.seen.offers().is_empty());
    assert!(receiver.received().is_empty());
    still_serves(&receiver, to).await;
    assert_no_panics();
}

/// S5: file bytes and text bytes sent before the user answered end the
/// connection and withdraw the offer; nothing reaches the disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_taken_before_consent() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;
    for (n, text) in [(0, false), (1, true)] {
        let mut evil = Evil::handshake(to, "Evil").await;
        if text {
            evil.introduce(IntroductionFrame {
                text_metadata: vec![TextMetadata {
                    text_title: Some("hi".into()),
                    r#type: Some(sn::text_metadata::Type::Text.into()),
                    payload_id: Some(5),
                    size: Some(2),
                    id: Some(5),
                }],
                ..Default::default()
            })
            .await;
        } else {
            evil.introduce(intro(vec![file_meta(5, "early.bin", 3)]))
                .await;
        }
        let (id, _) = receiver.offer(n).await;
        let kind = if text {
            PayloadType::Bytes
        } else {
            PayloadType::File
        };
        let total = if text { 2 } else { 3 };
        let body: &[u8] = if text { b"hi" } else { b"abc" };
        evil.send_offline(&payload_frame(kind, 5, total, 0, body, true))
            .await;
        assert!(evil.hung_up(DEADLINE).await);
        wait_for("the offer to be withdrawn", || {
            receiver
                .seen
                .consent()
                .iter()
                .any(|e| matches!(e, ConsentEvent::Closed { id: closed, .. } if *closed == id))
                .then_some(())
        })
        .await;
        assert!(!receiver.ctx.consent().answer(id, Decision::Accept));
    }
    assert!(receiver.received().is_empty());
    assert!(!receiver.seen.events().iter().any(|e| matches!(
        e,
        Event::TransferStarted { .. } | Event::TextReceived { .. }
    )));
    still_serves(&receiver, to).await;
    assert_no_panics();
}

/// Q4: payloads longer than they declared, and payloads that never end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payloads_stop_at_their_declared_size() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;

    // A BYTES payload (the introduction's carrier) that outgrows what it
    // declared, before consent.
    let mut evil = Evil::handshake(to, "Evil").await;
    let bytes = sharing(sn::v1_frame::FrameType::Introduction, |v| {
        v.introduction = Some(intro(vec![file_meta(1, "a", 1)]));
    })
    .encode_to_vec();
    evil.send_offline(&payload_frame(PayloadType::Bytes, 77, 4, 0, &bytes, true))
        .await;
    assert!(evil.hung_up(DEADLINE).await);

    // A BYTES payload that declares more than any control frame may be,
    // or less than zero (Q3), and one that keeps coming without its last
    // chunk until it passes the cap.
    for total in [i64::MAX, 1 << 20, -1] {
        let mut evil = Evil::handshake(to, "Evil").await;
        evil.send_offline(&payload_frame(
            PayloadType::Bytes,
            78,
            total,
            0,
            b"x",
            false,
        ))
        .await;
        assert!(evil.hung_up(DEADLINE).await, "{total}");
    }
    let mut evil = Evil::handshake(to, "Evil").await;
    let chunk = vec![0u8; 32 * 1024];
    let mut offset = 0i64;
    let mut refused = false;
    for _ in 0..64 {
        if !evil
            .try_send_offline(&payload_frame(
                PayloadType::Bytes,
                79,
                256 * 1024,
                offset,
                &chunk,
                false,
            ))
            .await
        {
            refused = true;
            break;
        }
        offset += chunk.len() as i64;
    }
    assert!(refused || evil.hung_up(DEADLINE).await);
    assert!(receiver.seen.offers().is_empty());

    // After consent: a file chunk past the declared size, and a file whose
    // chunks keep coming with no last one. Each fails the transfer and
    // leaves no partial file.
    for endless in [false, true] {
        let before = receiver.seen.offers().len();
        let mut evil = Evil::handshake(to, "Evil").await;
        evil.introduce(intro(vec![file_meta(1, "f.bin", 1000)]))
            .await;
        let (id, _) = receiver.offer(before).await;
        receiver.answer(id, true);
        assert_eq!(evil.answer().await, Some(Status::Accept));
        if endless {
            let mut offset = 0i64;
            for _ in 0..20 {
                if !evil
                    .try_send_offline(&payload_frame(
                        PayloadType::File,
                        1,
                        1000,
                        offset,
                        &[1u8; 100],
                        false,
                    ))
                    .await
                {
                    break;
                }
                offset += 100;
            }
        } else {
            evil.send_offline(&payload_frame(
                PayloadType::File,
                1,
                1000,
                0,
                &[1u8; 999],
                false,
            ))
            .await;
            let _ = evil
                .try_send_offline(&payload_frame(PayloadType::File, 1, 1000, 999, b"xy", true))
                .await;
        }
        let transfer = wait_for("the transfer", || {
            receiver
                .seen
                .events()
                .into_iter()
                .rev()
                .find_map(|e| match e {
                    Event::TransferStarted { transfer } => Some(transfer.id),
                    _ => None,
                })
        })
        .await;
        match receiver.finished(transfer).await.0 {
            Outcome::Failed { .. } => {}
            other => panic!("{other:?}"),
        }
        assert!(receiver.received().is_empty());
    }
    still_serves(&receiver, to).await;
    assert_no_panics();
}

/// Garbage where the handshake should be, in every shape: the connection is
/// closed, the user is never asked, and the listener carries on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_garbled_handshake_is_dropped() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;

    let not_protobuf = vec![0xffu8; 64];
    let wrong_type = offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::KeepAlive.into()),
        ..Default::default()
    })
    .encode_to_vec();
    let short_info = offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::ConnectionRequest.into()),
        connection_request: Some(lnc::ConnectionRequestFrame {
            endpoint_info: Some(vec![2, 0, 0]),
            ..Default::default()
        }),
        ..Default::default()
    })
    .encode_to_vec();
    let mut long_name = endpoint_info("x");
    long_name[17] = 200;
    let lying_length = offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::ConnectionRequest.into()),
        connection_request: Some(lnc::ConnectionRequestFrame {
            endpoint_info: Some(long_name),
            ..Default::default()
        }),
        ..Default::default()
    })
    .encode_to_vec();
    for first in [not_protobuf, wrong_type, short_info, lying_length] {
        let mut s = Evil::connect(to).await;
        write_raw(&mut s, &first).await;
        assert!(closed(&mut s, DEADLINE).await);
    }

    // A good connection request, then UKEY2 messages that are wrong in
    // turn: garbage, the wrong message type, a commitment that does not
    // match, a public key that is not on the curve.
    let request = offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::ConnectionRequest.into()),
        connection_request: Some(lnc::ConnectionRequestFrame {
            endpoint_info: Some(endpoint_info("Evil")),
            ..Default::default()
        }),
        ..Default::default()
    })
    .encode_to_vec();
    let client_init = |commitment: Vec<u8>| {
        Ukey2Message {
            message_type: Some(ukey2_message::Type::ClientInit.into()),
            message_data: Some(
                Ukey2ClientInit {
                    version: Some(1),
                    random: Some(vec![1; 32]),
                    next_protocol: Some("AES_256_CBC-HMAC_SHA256".into()),
                    cipher_commitments: vec![CipherCommitment {
                        handshake_cipher: Some(Ukey2HandshakeCipher::P256Sha512.into()),
                        commitment: Some(commitment),
                    }],
                }
                .encode_to_vec(),
            ),
        }
        .encode_to_vec()
    };
    let finish_with = |key: Vec<u8>| {
        Ukey2Message {
            message_type: Some(ukey2_message::Type::ClientFinish.into()),
            message_data: Some(
                Ukey2ClientFinished {
                    public_key: Some(key),
                }
                .encode_to_vec(),
            ),
        }
        .encode_to_vec()
    };
    let off_curve = finish_with(
        GenericPublicKey {
            r#type: PublicKeyType::EcP256.into(),
            ec_p256_public_key: Some(EcP256PublicKey {
                x: vec![1; 32],
                y: vec![2; 32],
            }),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let garbage_key = finish_with(vec![0xff; 40]);
    let (_, public) = utils::gen_ecdsa_keypair();
    let honest_finish = finish_with(public_key(&public));
    let cases: Vec<(Vec<u8>, Option<Vec<u8>>)> = vec![
        (vec![0x0a, 0xff, 0xff], None),
        (honest_finish.clone(), None),
        (client_init(vec![0; 64]), Some(honest_finish.clone())),
        (
            client_init(Sha512::digest(&off_curve).to_vec()),
            Some(off_curve.clone()),
        ),
        (
            client_init(Sha512::digest(&garbage_key).to_vec()),
            Some(garbage_key.clone()),
        ),
    ];
    for (init, finish) in cases {
        let mut s = Evil::connect(to).await;
        write_raw(&mut s, &request).await;
        write_raw(&mut s, &init).await;
        if let Some(finish) = finish {
            // The server init comes first; a bad one gets an alert or a
            // hang-up instead.
            let _ = read_raw(&mut s).await;
            let _ = s.write_all(&(finish.len() as u32).to_be_bytes()).await;
            let _ = s.write_all(&finish).await;
        }
        assert!(closed(&mut s, DEADLINE).await);
    }

    // Past the handshake, frames whose MAC does not match, or that are not
    // SecureMessages at all.
    let mut evil = Evil::handshake(to, "Evil").await;
    let mut sealed = evil.seal(&payload_frame(PayloadType::Bytes, 1, 1, 0, b"x", true));
    let last = sealed.len() - 1;
    sealed[last] ^= 1;
    write_raw(&mut evil.socket, &sealed).await;
    assert!(evil.hung_up(DEADLINE).await);
    let mut evil = Evil::handshake(to, "Evil").await;
    write_raw(&mut evil.socket, &[0xff; 100]).await;
    assert!(evil.hung_up(DEADLINE).await);

    assert!(receiver.seen.offers().is_empty());
    assert!(receiver.received().is_empty());
    still_serves(&receiver, to).await;
    assert_no_panics();
}

/// A length prefix the receiver will not honour is refused before a byte
/// of the body is sent, so a peer cannot make it allocate by announcing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_frame_lengths_are_refused_up_front() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;
    // During the handshake: more than 32 KiB, up to 4 GiB.
    for len in [33u32 * 1024, 5 << 20, u32::MAX] {
        let mut s = Evil::connect(to).await;
        s.write_all(&len.to_be_bytes()).await.unwrap();
        assert!(closed(&mut s, DEADLINE).await, "{len}");
    }
    // An empty frame.
    let mut s = Evil::connect(to).await;
    s.write_all(&0u32.to_be_bytes()).await.unwrap();
    assert!(closed(&mut s, DEADLINE).await);
    // After the handshake, before consent: more than one sharing frame.
    let mut evil = Evil::handshake(to, "Evil").await;
    evil.socket
        .write_all(&(1u32 << 20).to_be_bytes())
        .await
        .unwrap();
    assert!(evil.hung_up(DEADLINE).await);
    assert!(receiver.seen.offers().is_empty());
    still_serves(&receiver, to).await;
    assert_no_panics();
}

/// S6: a sender that stops -- before saying anything, half-way through a
/// frame, or mid-transfer -- is given up on, and nothing is left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_senders_are_given_up_on() {
    let timeouts = Timeouts {
        handshake: Duration::from_millis(800),
        idle: Duration::from_millis(800),
        ..quick()
    };
    let receiver = Rig::with("Receiver", timeouts, Duration::from_secs(10), |_| {});
    let to = receiver.listen().await;

    // Silent from the start; half a length prefix; half a frame.
    let mut silent = Evil::connect(to).await;
    let mut half_prefix = Evil::connect(to).await;
    half_prefix.write_all(&[0, 0]).await.unwrap();
    let mut half_frame = Evil::connect(to).await;
    half_frame.write_all(&100u32.to_be_bytes()).await.unwrap();
    half_frame.write_all(&[0u8; 10]).await.unwrap();
    for s in [&mut silent, &mut half_prefix, &mut half_frame] {
        let started = Instant::now();
        assert!(closed(s, Duration::from_secs(5)).await);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    // Mid-transfer: half the file, then nothing.
    let mut evil = Evil::handshake(to, "Evil").await;
    evil.introduce(intro(vec![file_meta(1, "slow.bin", 1000)]))
        .await;
    let (id, _) = receiver.offer(0).await;
    receiver.answer(id, true);
    assert_eq!(evil.answer().await, Some(Status::Accept));
    evil.send_offline(&payload_frame(
        PayloadType::File,
        1,
        1000,
        0,
        &[1u8; 500],
        false,
    ))
    .await;
    let incoming = receiver.incoming().await;
    match receiver.finished(incoming).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Network),
        other => panic!("{other:?}"),
    }
    assert!(receiver.received().is_empty());
    // The sender was told (a cancel), then hung up on.
    assert!(evil.hung_up(DEADLINE).await);
    assert_no_panics();
}

/// After the yes, a sender that keeps talking without sending anything --
/// empty file chunks and keep-alives, each well within the idle limit --
/// is given up on once the idle limit has passed without progress. Each
/// frame used to count as activity, so the transfer, and its connection
/// slot, were held for as long as the sender liked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sender_that_only_keeps_talking_is_given_up_on() {
    let timeouts = Timeouts {
        idle: Duration::from_millis(800),
        ..quick()
    };
    let receiver = Rig::with("Receiver", timeouts, Duration::from_secs(10), |_| {});
    let to = receiver.listen().await;
    let mut evil = Evil::handshake(to, "Evil").await;
    evil.introduce(intro(vec![file_meta(1, "chatty.bin", 1000)]))
        .await;
    let (id, _) = receiver.offer(0).await;
    receiver.answer(id, true);
    assert_eq!(evil.answer().await, Some(Status::Accept));
    // One byte of the file, then nothing but talk, every 200 ms, for 5 s.
    evil.send_offline(&payload_frame(PayloadType::File, 1, 1000, 0, &[1u8], false))
        .await;
    let started = Instant::now();
    let keep_alive = offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::KeepAlive.into()),
        keep_alive: Some(lnc::KeepAliveFrame { ack: Some(false) }),
        ..Default::default()
    });
    let talker = tokio::spawn(async move {
        for i in 0u32.. {
            let frame = if i % 2 == 0 {
                payload_frame(PayloadType::File, 1, 1000, 1, &[], false)
            } else {
                keep_alive.clone()
            };
            if started.elapsed() > Duration::from_secs(5) || !evil.try_send_offline(&frame).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    let incoming = receiver.incoming().await;
    match receiver.finished(incoming).await.0 {
        Outcome::Failed { error } => assert_eq!(error.code, ErrorCode::Network),
        other => panic!("{other:?}"),
    }
    // About the idle limit after the last byte, not after the talk ended.
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "given up on after {:?}",
        started.elapsed()
    );
    assert!(receiver.received().is_empty());
    talker.await.unwrap();
    assert_no_panics();
}

/// A sender that keeps the consent dialog busy with keep-alives is still
/// bounded by the consent timeout (F-C3), and gets a timed-out answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_offer_times_out() {
    let receiver = Rig::with("Receiver", quick(), Duration::from_millis(700), |_| {});
    let to = receiver.listen().await;
    let mut evil = Evil::handshake(to, "Evil").await;
    evil.introduce(intro(vec![file_meta(1, "a.bin", 1)])).await;
    receiver.offer(0).await;
    let keep_alive = offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::KeepAlive.into()),
        keep_alive: Some(lnc::KeepAliveFrame { ack: Some(false) }),
        ..Default::default()
    });
    evil.send_offline(&keep_alive).await;
    assert_eq!(evil.answer().await, Some(Status::TimedOut));
    assert!(evil.hung_up(DEADLINE).await);
    assert!(receiver.received().is_empty());
    assert_no_panics();
}

/// S7 and the connection caps: the listener does not keep more than a few
/// connections, however many a peer opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_are_capped() {
    let receiver = Rig::new("Receiver");
    let to = receiver.listen().await;
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(connect_from(to, Ipv4Addr::new(127, 0, 0, 9)).await);
    }
    // At most two per address are kept; the rest are closed at once.
    let mut open = 0;
    for s in &mut held {
        if !closed(s, Duration::from_millis(300)).await {
            open += 1;
        }
    }
    assert!(open <= 2, "{open} connections kept from one address");
    assert_no_panics();
}
