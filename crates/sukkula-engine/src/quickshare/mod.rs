//! Quick Share over the LAN (F-QS), over the patched `rqs_lib` in
//! `third_party/` (upstream plus `third_party/rqs_lib.patches/`;
//! `docs/UPSTREAM-QUICKSHARE.md` explains every patch).
//!
//! rqs_lib, as patched, is a protocol library and nothing else: it keeps no
//! global state, spawns no task and writes no file. This adapter owns every
//! socket, task and timer and drives each connection one frame at a time:
//!
//! - **Receiving** (F-QS1, F-QS4) — [`receive`]. A TCP listener on a random
//!   port, announced over mDNS while visibility is Everyone; with Hidden
//!   there is neither listener nor announcement. A connection from an
//!   address [`Ctx::permits`] refuses, or over the per-IP rate
//!   ([`Ctx::allow_offer`]) or the connection caps, is closed before a byte
//!   of it is read (S7). The handshake up to the sender's introduction is
//!   bounded by [`HANDSHAKE_TIMEOUT`]; the introduction becomes a
//!   [`sukkula_core::offer::RawOffer`] with the handshake PIN (F-QS3) and
//!   goes through [`Ctx::offer`]. Until the user accepts, the connection is
//!   read only for keep-alives and cancellation: rqs_lib refuses any
//!   payload byte before acceptance (S5), and holds at most one frame of
//!   260 KiB and two sharing frames of 256 KiB meanwhile. Accepted files
//!   arrive as chunks and are written by `sukkula_core::inbox` through
//!   [`Ctx::begin_file`] (S1, S3), every call bounded by
//!   [`crate::ctx::idle_timeout`]; a text is shown with
//!   `Event::TextReceived` once accepted (F-C4).
//! - **Sending** (F-QS1) — [`send`]: files, or one text, to a peer found by
//!   discovery.
//! - **Discovery** — [`discovery`]: mDNS browsing, peers reported as
//!   `qs:<endpoint id>` with sanitised names, at most [`MAX_PEERS`], each
//!   source rate-limited ([`Ctx::allow_discovery`]).
//! - **The BLE nudge** (F-QS2) — [`ble`]: while discovery runs and
//!   `Settings.quickshare.ble_nudge` is on, the Quick Share "a device
//!   nearby is sharing" advertisement goes out through BlueZ on the system
//!   bus, so Android phones nearby start announcing their mDNS service.
//!   Best effort: without an adapter or BlueZ, LAN discovery still works.
//!
//! Every task ends on [`Ctx::shutdown_token`]; receiving and discovery each
//! run under a child token that `stop_*` cancels and then waits out (at
//! most 1.25 s each, inside the hub's 3 s), so that the mDNS daemon
//! threads and the D-Bus thread are gone when the engine stops.
//!
//! Under the test switch (`StartConfig.allow_loopback`, which the shell
//! never sets) mDNS runs on loopback only and the BLE nudge only on a bus
//! the test names, so a test run never announces on the host's LAN or
//! talks to its system bus.
//!
//! [`HANDSHAKE_TIMEOUT`]: sukkula_core::limits::HANDSHAKE_TIMEOUT
//! [`MAX_PEERS`]: sukkula_core::limits::MAX_PEERS

pub mod ble;
mod discovery;
#[doc(hidden)]
pub mod fuzzing;
mod receive;
mod send;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rqs_lib::hdl::AddrFilter;
use sukkula_core::Protocol;
use sukkula_core::config::Visibility;
use sukkula_core::limits::{HANDSHAKE_TIMEOUT, NETWORK_IDLE_TIMEOUT, OFFER_TIMEOUT};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adapter::{Adapter, BoxFuture, Outgoing};
use crate::api::{ErrorCode, ErrorInfo, SendTarget, TransferId};
use crate::ctx::Ctx;

/// Most inbound connections open at once, handshaking or transferring.
pub const MAX_INBOUND_CONNECTIONS: usize = 4;

/// Most inbound connections open at once from one address.
pub const MAX_CONNECTIONS_PER_IP: usize = 2;

/// How long stopping waits for a task to wind down before leaving it to
/// the engine's own shutdown. The hub gives an adapter 3 s to stop
/// discovery and then receiving; each gets less than half. The waits
/// inside are shorter still: 0.5 s each for the mDNS goodbye and daemon,
/// 0.5 s and a tick for the BLE nudge to unregister.
const STOP_WAIT: Duration = Duration::from_millis(1250);

/// How long to wait for a peer's TCP connection when sending.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a sender waits for the receiver's user to answer: our own
/// consent timeout and a margin, since the receiver's dialog runs its own.
const SEND_CONSENT_TIMEOUT: Duration = OFFER_TIMEOUT.saturating_add(Duration::from_secs(30));

/// The waits, for tests to shorten. Every one is at most its limit in
/// `sukkula_core::limits` in the engine's own build.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// From accepting a connection to the sender's introduction.
    pub handshake: Duration,
    /// Any one network read or write making no progress (S6).
    pub idle: Duration,
    /// Connecting to a peer to send.
    pub connect: Duration,
    /// Waiting for the receiver's user when sending.
    pub send_consent: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            handshake: HANDSHAKE_TIMEOUT,
            idle: NETWORK_IDLE_TIMEOUT,
            connect: CONNECT_TIMEOUT,
            send_consent: SEND_CONSENT_TIMEOUT,
        }
    }
}

/// How the adapter reaches the network. Not a setting: the engine always
/// uses [`Options::default`]. Tests turn mDNS off and point the BLE nudge
/// at a private bus.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Announce and browse over mDNS. Off in tests: multicast is real
    /// network.
    pub mdns: bool,
    /// Where the TCP listener binds. IPv4, as Quick Share peers resolve us
    /// over IPv4 only.
    pub listen: SocketAddr,
    /// The system bus for the BLE nudge; `None` for the environment's
    /// (and, with loopback allowed, for none: see `Shared::system_bus`).
    pub system_bus: Option<String>,
    /// The waits.
    pub timeouts: Timeouts,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mdns: true,
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            system_bus: None,
            timeouts: Timeouts::default(),
        }
    }
}

/// The adapter.
#[must_use]
pub fn adapter(ctx: Arc<Ctx>) -> Arc<dyn Adapter> {
    adapter_with(ctx, Options::default())
}

/// The adapter, with test options. The concrete type lets tests learn the
/// listening port and add peers without mDNS.
#[doc(hidden)]
#[must_use]
pub fn adapter_with(ctx: Arc<Ctx>, options: Options) -> Arc<QuickShareAdapter> {
    Arc::new(QuickShareAdapter {
        shared: Arc::new(Shared {
            ctx,
            options,
            peers: Mutex::new(discovery::Peers::default()),
            slots: Mutex::new(Slots::default()),
            own_endpoint: Mutex::new(None),
        }),
        receiving: tokio::sync::Mutex::new(None),
        discovering: tokio::sync::Mutex::new(None),
    })
}

/// State the tasks share.
pub(crate) struct Shared {
    ctx: Arc<Ctx>,
    options: Options,
    peers: Mutex<discovery::Peers>,
    slots: Mutex<Slots>,
    /// The endpoint id we announce, so discovery can skip ourselves.
    own_endpoint: Mutex<Option<[u8; 4]>>,
}

impl Shared {
    /// The reach policy (S7) as rqs_lib's mDNS code takes it: which
    /// interfaces mDNS runs on, and which announced addresses count.
    ///
    /// With loopback allowed -- the test switch, `StartConfig.allow_loopback`,
    /// which the shell never sets -- mDNS stays on loopback: a test run must
    /// not announce this machine as a Quick Share receiver to the LAN it
    /// happens to be on.
    fn addr_filter(&self) -> AddrFilter {
        let ctx = self.ctx.clone();
        if ctx.reach().allow_loopback {
            Arc::new(move |ip: IpAddr| ip.is_loopback() && ctx.permits(ip))
        } else {
            Arc::new(move |ip| ctx.permits(ip))
        }
    }

    /// The system bus for the BLE nudge: the one [`Options`] names, else
    /// the environment's -- except under the test switch, where a test
    /// that did not name a bus gets none rather than the host's.
    fn system_bus(&self) -> Option<Option<String>> {
        match &self.options.system_bus {
            Some(bus) => Some(Some(bus.clone())),
            None if self.ctx.reach().allow_loopback => None,
            None => Some(None),
        }
    }
}

/// The Quick Share adapter. Public only for tests; the engine sees it as
/// `dyn Adapter`.
#[doc(hidden)]
pub struct QuickShareAdapter {
    shared: Arc<Shared>,
    receiving: tokio::sync::Mutex<Option<Running>>,
    discovering: tokio::sync::Mutex<Option<Running>>,
}

/// Something started, and how to stop it.
struct Running {
    token: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    /// The listening port, when receiving.
    port: Option<u16>,
}

impl Running {
    /// Cancels, then waits for the tasks to end, briefly. A task still
    /// running after that is cancelled anyway and ends on its own.
    async fn stop(self) {
        self.token.cancel();
        let all = async {
            for t in self.tasks {
                let _ = t.await;
            }
        };
        let _ = tokio::time::timeout(STOP_WAIT, all).await;
    }
}

impl QuickShareAdapter {
    /// The port the listener is on, while receiving.
    #[doc(hidden)]
    pub async fn local_port(&self) -> Option<u16> {
        self.receiving.lock().await.as_ref().and_then(|r| r.port)
    }

    /// Adds a peer as if discovery had found it at `addr`, and returns its
    /// id. For tests, which have no mDNS.
    #[doc(hidden)]
    #[must_use]
    pub fn insert_peer(
        &self,
        endpoint_id: [u8; 4],
        addr: SocketAddr,
        name: &str,
    ) -> Option<String> {
        discovery::insert(
            &self.shared,
            endpoint_id,
            addr,
            name,
            rqs_lib::DeviceType::Phone,
        )
    }

    async fn start_receiving_inner(&self) -> Result<(), ErrorInfo> {
        let mut receiving = self.receiving.lock().await;
        if let Some(r) = receiving.take() {
            // A restart: settings may have changed.
            r.stop().await;
        }
        let settings = self.shared.ctx.settings();
        if settings.quickshare.visibility == Visibility::Hidden {
            // F-QS4: hidden means nobody can find this phone, so nothing
            // listens either: a peer that remembers the port gets nothing.
            *lock(&self.shared.own_endpoint) = None;
            return Ok(());
        }
        let running = receive::start(&self.shared).await?;
        *receiving = Some(running);
        Ok(())
    }

    async fn stop_receiving_inner(&self) {
        if let Some(r) = self.receiving.lock().await.take() {
            r.stop().await;
        }
        *lock(&self.shared.own_endpoint) = None;
    }

    async fn start_discovery_inner(&self) -> Result<(), ErrorInfo> {
        let mut discovering = self.discovering.lock().await;
        if discovering.is_some() {
            return Ok(());
        }
        *discovering = Some(discovery::start(&self.shared)?);
        Ok(())
    }

    async fn stop_discovery_inner(&self) {
        if let Some(d) = self.discovering.lock().await.take() {
            d.stop().await;
        }
    }
}

impl Adapter for QuickShareAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::QuickShare
    }

    fn start_receiving(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        Box::pin(self.start_receiving_inner())
    }

    fn stop_receiving(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.stop_receiving_inner())
    }

    fn start_discovery(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        Box::pin(self.start_discovery_inner())
    }

    fn stop_discovery(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.stop_discovery_inner())
    }

    fn send(
        &self,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        Box::pin(async move {
            let SendTarget::QuickShare { peer } = target else {
                return Err(ErrorInfo::new(
                    ErrorCode::BadCommand,
                    "not a Quick Share target",
                ));
            };
            send::start(&self.shared, &peer, items)
        })
    }
}

/// Inbound connection slots: at most [`MAX_INBOUND_CONNECTIONS`] in all
/// and [`MAX_CONNECTIONS_PER_IP`] per address.
#[derive(Default)]
struct Slots {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// One inbound connection's slot, given back on drop.
pub(crate) struct Slot {
    shared: Arc<Shared>,
    ip: IpAddr,
}

impl Slot {
    /// A slot for a connection from `ip`, if one is free.
    fn take(shared: &Arc<Shared>, ip: IpAddr) -> Option<Slot> {
        let mut slots = lock(&shared.slots);
        let from_ip = slots.per_ip.get(&ip).copied().unwrap_or(0);
        if slots.total >= MAX_INBOUND_CONNECTIONS || from_ip >= MAX_CONNECTIONS_PER_IP {
            return None;
        }
        slots.total = slots.total.saturating_add(1);
        slots.per_ip.insert(ip, from_ip.saturating_add(1));
        Some(Slot {
            shared: shared.clone(),
            ip,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut slots = lock(&self.shared.slots);
        slots.total = slots.total.saturating_sub(1);
        let left = slots
            .per_ip
            .get(&self.ip)
            .copied()
            .unwrap_or(0)
            .saturating_sub(1);
        if left == 0 {
            slots.per_ip.remove(&self.ip);
        } else {
            slots.per_ip.insert(self.ip, left);
        }
    }
}

/// A random endpoint id: four ASCII letters and digits, as Quick Share
/// devices use. Not a secret; it only has to differ between devices.
fn new_endpoint_id() -> [u8; 4] {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let raw = rqs_lib::utils::gen_random(4);
    let mut id = [b'A'; 4];
    for (slot, b) in id.iter_mut().zip(raw) {
        *slot = usize::from(b)
            .checked_rem(ALPHABET.len())
            .and_then(|i| ALPHABET.get(i))
            .copied()
            .unwrap_or(b'A');
    }
    id
}

/// Runs `fut`, failing with [`ErrorCode::Network`] after `limit` (S6).
async fn within<T, E>(
    limit: Duration,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, ErrorInfo> {
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err(ErrorInfo::new(ErrorCode::Network, "the connection failed")),
        Err(_) => Err(ErrorInfo::new(
            ErrorCode::Network,
            "the peer stopped responding",
        )),
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
