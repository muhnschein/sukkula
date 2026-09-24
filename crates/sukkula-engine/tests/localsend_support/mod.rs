//! Fixtures for the LocalSend integration tests: nodes (a context and an
//! adapter on loopback, on an ephemeral port), a pool of pre-made device
//! identities, and a raw TLS peer for hand-built requests.
//!
//! Identities are pre-made because an RSA-2048 key takes seconds to
//! generate in a debug build. They are written to each node's data directory
//! through `Store`, so the adapter's load path is what runs.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::missing_panics_doc
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use localsend::crypto::cert::{SelfSignedCert, generate_self_signed};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sukkula_core::config::Settings;
use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision, OfferId};
use sukkula_core::inbox::Inbox;
use sukkula_core::limits::{MAX_PENDING_OFFERS, OFFER_TIMEOUT};
use sukkula_core::offer::Offer;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_engine::adapter::Adapter;
use sukkula_engine::api::{Event, Outcome, TransferId};
use sukkula_engine::ctx::Ctx;
use sukkula_engine::localsend::{LocalSend, Options, adapter_with};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// How long a "this must happen" wait lasts before it is called a hang.
pub const DEADLINE: Duration = Duration::from_secs(20);

/// Identities in the pool.
const POOL: usize = 5;

/// Pre-made identities; index them per role so peers in one test differ.
pub fn identity(i: usize) -> &'static SelfSignedCert {
    static IDS: OnceLock<Vec<SelfSignedCert>> = OnceLock::new();
    &IDS.get_or_init(|| {
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..POOL)
                .map(|_| s.spawn(|| generate_self_signed().unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    })[i % POOL]
}

/// How a node is set up.
#[derive(Clone)]
pub struct NodeConfig {
    pub identity: usize,
    pub name: String,
    pub allow_loopback: bool,
    pub consent_timeout: Duration,
    pub pin: Option<String>,
    pub port: u16,
    pub idle: Duration,
    pub handshake: Duration,
    pub prepare: Duration,
}

impl NodeConfig {
    pub fn new(identity: usize, name: &str) -> Self {
        NodeConfig {
            identity,
            name: name.to_owned(),
            allow_loopback: true,
            consent_timeout: OFFER_TIMEOUT,
            pin: None,
            port: 0,
            idle: Duration::from_secs(5),
            handshake: Duration::from_secs(5),
            prepare: Duration::from_secs(15),
        }
    }
}

/// A context and a LocalSend adapter, with everything they emit recorded.
pub struct Node {
    pub dir: tempfile::TempDir,
    pub ctx: Arc<Ctx>,
    pub ls: Arc<LocalSend>,
    pub events: Arc<Mutex<Vec<Event>>>,
    pub consent: Arc<Mutex<Vec<ConsentEvent>>>,
}

impl Node {
    pub fn new(cfg: &NodeConfig) -> Node {
        let dir = tempfile::tempdir().unwrap();
        Self::in_dir(dir, cfg)
    }

    /// A node over an existing directory (a restart).
    pub fn in_dir(dir: tempfile::TempDir, cfg: &NodeConfig) -> Node {
        let store = Store::open(&dir.path().join("data")).unwrap();
        let id = identity(cfg.identity);
        store
            .write("localsend-key.pem", id.private_key_pem.as_bytes())
            .unwrap();
        store
            .write("localsend-cert.pem", id.certificate_pem.as_bytes())
            .unwrap();
        let inbox = Inbox::open(&dir.path().join("dl")).unwrap();
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let consent: Arc<Mutex<Vec<ConsentEvent>>> = Arc::default();
        let log = consent.clone();
        let broker = ConsentBroker::with_limits(
            Arc::new(move |e| log.lock().unwrap().push(e)),
            cfg.consent_timeout,
            MAX_PENDING_OFFERS,
        );
        let mut settings = Settings {
            device_name: cfg.name.clone(),
            ..Settings::default()
        };
        settings.localsend.pin = cfg.pin.clone();
        let log = events.clone();
        let ctx = Arc::new(Ctx::new(
            settings,
            "Test Model".into(),
            store,
            inbox,
            broker,
            ReachPolicy {
                allow_loopback: cfg.allow_loopback,
            },
            Arc::new(move |e| log.lock().unwrap().push(e)),
        ));
        let ls = adapter_with(
            ctx.clone(),
            Options {
                port: cfg.port,
                multicast_port: None,
                idle_timeout: cfg.idle,
                handshake_timeout: cfg.handshake,
                prepare_timeout: cfg.prepare,
            },
        );
        Node {
            dir,
            ctx,
            ls,
            events,
            consent,
        }
    }

    /// Starts receiving and returns the port.
    pub async fn receive(&self) -> u16 {
        self.ls.start_receiving().await.unwrap();
        self.ls.port().await.unwrap()
    }

    /// Where this node's server is.
    pub async fn addr(&self) -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), self.ls.port().await.unwrap())
    }

    /// Discovers `other` by address; returns its peer id.
    pub async fn find(&self, other: &Node) -> String {
        self.ls.start_discovery().await.unwrap();
        self.ls.discover_at(other.addr().await).await.unwrap()
    }

    pub fn downloads(&self) -> PathBuf {
        self.dir.path().join("dl")
    }

    /// Names in the download directory, without the staging directory.
    pub fn saved(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.downloads())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".partial")
            .collect();
        names.sort();
        names
    }

    /// Files left in staging.
    pub fn partials(&self) -> usize {
        std::fs::read_dir(self.downloads().join(".partial"))
            .unwrap()
            .count()
    }

    /// Waits until `f` picks something out of the events.
    pub async fn wait_event<T>(&self, what: &str, f: impl Fn(&Event) -> Option<T>) -> T {
        wait(what, || self.events.lock().unwrap().iter().find_map(&f)).await
    }

    /// Waits for the next offer the user is asked about.
    pub async fn offer(&self) -> (OfferId, Arc<Offer>) {
        self.offer_after(0).await
    }

    /// Waits for an offer beyond the first `n`.
    pub async fn offer_after(&self, n: usize) -> (OfferId, Arc<Offer>) {
        wait("an offer", || {
            self.consent
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    ConsentEvent::Pending { id, offer } => Some((*id, offer.clone())),
                    ConsentEvent::Closed { .. } => None,
                })
                .nth(n)
        })
        .await
    }

    /// How many offers were shown.
    pub fn offers_shown(&self) -> usize {
        self.consent
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, ConsentEvent::Pending { .. }))
            .count()
    }

    pub fn answer(&self, id: OfferId, accept: bool) {
        let d = if accept {
            Decision::Accept
        } else {
            Decision::Decline
        };
        assert!(self.ctx.consent().answer(id, d), "offer {id} not waiting");
    }

    /// Waits for a transfer to finish; returns its outcome and saved names.
    pub async fn finished(&self, id: TransferId) -> (Outcome, Vec<String>) {
        self.wait_event("the transfer to finish", |e| match e {
            Event::TransferFinished {
                transfer,
                outcome,
                saved,
            } if *transfer == id => Some((outcome.clone(), saved.clone())),
            _ => None,
        })
        .await
    }

    /// The first incoming transfer's id.
    pub async fn incoming(&self) -> TransferId {
        self.nth_incoming(0).await
    }

    pub async fn nth_incoming(&self, n: usize) -> TransferId {
        wait("an incoming transfer", || {
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

    /// Every event, as JSON: what the UI would get.
    pub fn events_json(&self) -> String {
        serde_json::to_string(&*self.events.lock().unwrap()).unwrap()
    }
}

/// Polls `f` until it yields, or fails the test after [`DEADLINE`].
pub async fn wait<T>(what: &str, f: impl Fn() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A file of `len` pseudo-random bytes in `dir`.
pub fn make_file(dir: &Path, name: &str, len: usize) -> PathBuf {
    let mut x: u32 = 0x1234_5678 ^ u32::try_from(len).unwrap_or(0);
    let data: Vec<u8> = (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x.to_le_bytes()[0]
        })
        .collect();
    let path = dir.join(name);
    #[allow(clippy::disallowed_methods)] // The scene: a file to send.
    std::fs::write(&path, data).unwrap();
    path
}

/// A sparse file of `len` zero bytes: large, and instant to make.
pub fn sparse_file(dir: &Path, name: &str, len: u64) -> PathBuf {
    let path = dir.join(name);
    #[allow(clippy::disallowed_methods)] // The scene: a file to send.
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(len).unwrap();
    path
}

/// An outgoing file as the hub would hand it over.
pub fn outgoing(path: &Path) -> sukkula_engine::adapter::Outgoing {
    let size = std::fs::metadata(path).unwrap().len();
    let name = sukkula_core::name::sanitize(&path.file_name().unwrap().to_string_lossy());
    sukkula_engine::adapter::Outgoing::File(sukkula_engine::adapter::OutgoingFile {
        path: path.to_path_buf(),
        name,
        size,
        mime: None,
    })
}

// ------------------------------------------------------------ raw peers

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts any server certificate: a raw peer is the attacker, and does not
/// care who it talks to.
#[derive(Debug)]
struct AnyServer(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AnyServer {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// A raw TLS client config, presenting identity `id`, or no certificate.
pub fn raw_client_config(id: Option<usize>) -> Arc<rustls::ClientConfig> {
    let p = provider();
    let b = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyServer(p.signature_verification_algorithms)));
    let mut config = match id {
        Some(i) => {
            let id = identity(i);
            b.with_client_auth_cert(
                vec![CertificateDer::from_pem_slice(id.certificate_pem.as_bytes()).unwrap()],
                PrivateKeyDer::from_pem_slice(id.private_key_pem.as_bytes()).unwrap(),
            )
            .unwrap()
        }
        None => b.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

/// A TCP connection to `port` on loopback, from `source` if given.
pub async fn tcp(port: u16, source: Option<Ipv4Addr>) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    if let Some(src) = source {
        socket.bind(SocketAddr::new(IpAddr::V4(src), 0)).unwrap();
    }
    socket
        .connect(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))
        .await
        .unwrap()
}

/// A TLS connection as identity `id` (or none).
pub async fn raw_tls(
    port: u16,
    id: Option<usize>,
    source: Option<Ipv4Addr>,
) -> std::io::Result<TlsStream<TcpStream>> {
    let tcp = tcp(port, source).await;
    TlsConnector::from(raw_client_config(id))
        .connect(ServerName::IpAddress(Ipv4Addr::LOCALHOST.into()), tcp)
        .await
}

/// An HTTP response as a raw peer sees it.
#[derive(Debug)]
pub struct RawResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl RawResponse {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// Writes `head` and `body`, then reads one response (up to 1 MiB).
pub async fn exchange<S>(stream: &mut S, head: &str, body: &[u8]) -> Option<RawResponse>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    stream.write_all(head.as_bytes()).await.ok()?;
    stream.write_all(body).await.ok()?;
    stream.flush().await.ok()?;
    read_response(stream).await
}

/// Reads one response, if one comes within [`DEADLINE`].
pub async fn read_response<S>(stream: &mut S) -> Option<RawResponse>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let read = async {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).await.ok()? == 0 {
                return None;
            }
            buf.push(byte[0]);
            if buf.len() > 64 * 1024 {
                return None;
            }
        }
        let head = String::from_utf8_lossy(&buf).into_owned();
        let status: u16 = head.split(' ').nth(1)?.parse().ok()?;
        let len: usize = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())?
            })
            .unwrap_or(0);
        let mut body = vec![0u8; len.min(1 << 20)];
        stream.read_exact(&mut body).await.ok()?;
        Some(RawResponse { status, body })
    };
    tokio::time::timeout(DEADLINE, read).await.ok().flatten()
}

/// A POST head with a `Content-Length`.
pub fn post(path: &str, len: usize) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {len}\r\n\r\n"
    )
}

/// One request on a fresh connection as identity `id`.
pub async fn request(port: u16, id: usize, path: &str, body: &[u8]) -> Option<RawResponse> {
    let mut s = raw_tls(port, Some(id), None).await.ok()?;
    exchange(&mut s, &post(path, body.len()), body).await
}

/// A prepare-upload body from `alias` with the given `files` JSON map.
pub fn offer_json(id: usize, alias: &str, files: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "info": {
            "alias": alias,
            "version": "2.2",
            "deviceModel": "Evil",
            "deviceType": "desktop",
            "fingerprint": identity(id).fingerprint,
            "port": 53317,
            "protocol": "https",
            "download": false
        },
        "files": files
    }))
    .unwrap()
}

/// One file entry of an offer.
pub fn file_json(id: &str, name: &str, size: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "id": id, "fileName": name, "size": size, "fileType": "application/octet-stream" })
}

pub const PREPARE: &str = "/api/localsend/v2/prepare-upload";
pub const REGISTER: &str = "/api/localsend/v2/register";

/// The upload path for a session's file.
pub fn upload_path(session: &str, file: &str, token: &str) -> String {
    format!("/api/localsend/v2/upload?sessionId={session}&fileId={file}&token={token}")
}
