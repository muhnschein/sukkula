//! LocalSend v2 (F-LS1..F-LS4), over the upstream `localsend` crate.
//!
//! # What comes from upstream, and what does not
//!
//! From the protocol authors' crate (pinned, spec §6): the wire types
//! (`http::dto_v2`, `model`), so the JSON is theirs by construction; the
//! device certificate -- generation, the self-signature and validity check,
//! the fingerprint (`crypto::cert`); and UDP multicast (`multicast`).
//!
//! Not from upstream: the HTTPS server, the HTTPS client and the
//! announcement answering. Reading upstream at `e768240` shows that hostile
//! input reaches its parsing before an application hook could see it, and
//! that it offers no hook to put a check in front:
//!
//! | # | Upstream (e768240) | Effect | Here |
//! | --- | --- | --- | --- |
//! | L1 | `http::server` has no peer-address hook; `start_with_port` binds `0.0.0.0`/`[::]` and serves everyone | S7 cannot hold: any address is TLS-handshaked and parsed | [`server`] checks the address at accept, before the handshake |
//! | L2 | `CollectToJson` collects `register`, `prepare-upload` and `v3/nonce` bodies with no size limit (`internal::show` too) | Memory exhaustion from any peer, before consent, before any event | at most 64 KiB, idle timeout and deadline, then parse |
//! | L3 | No TLS handshake timeout, no connection limit; hyper-util's auto builder without a timer, so no header timeout | Slow-loris on handshake or head holds a task and a descriptor forever | handshake deadline, header timeout, 32 connections, 8 per address, connect rate per address |
//! | L4 | `http::client` reads response bodies whole (`json()`, `text()` in `into_error`) | A hostile receiver -- or any announcer, see L5 -- makes us buffer without limit | responses capped at 64 KiB |
//! | L5 | `discovery` answers every announcement, from any address, unthrottled, over plain HTTP if announced | Plain-HTTP traffic (F-LS2), unbounded outgoing connections, S7 | [`discovery::screen`] first; answers pinned, 4 at a time |
//! | L6 | PIN lockout after 3 failures per address, never lifted; no global bound | A LAN attacker has as many addresses as it likes | per address and in all, lifted after 5 minutes |
//! | L7 | The v2 server hands file names to the application raw; `util::filename` exists but is not applied | Every application must remember to | S1 in `Offer::validate`, S3 in the inbox, as for every protocol |
//!
//! So the adapter drives the same libraries upstream uses -- hyper, rustls
//! with ring -- itself, which puts every S-rule in front of the parser it
//! protects, and keeps upstream's wire format. When upstream grows the hooks
//! (L1..L3 are the ones that matter), the server can go back to it.
//!
//! # Lifecycle
//!
//! Receiving runs the HTTPS server; discovery runs announcements and the
//! peer table; the multicast socket runs while either does. Every task is
//! tracked, and every one selects on a token that the engine's shutdown
//! cancels. Stopping receiving with a transfer running lets that transfer
//! finish (the adapter contract) but serves only its sender; the listener
//! closes when it ends.

mod client;
mod discovery;
mod identity;
mod peers;
mod send;
mod server;
mod tls;
mod wire;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use localsend::http::dto_v2::{InfoResponseDtoV2, RegisterDtoV2, RegisterResponseDtoV2};
use localsend::model::discovery::{DeviceType as LsDeviceType, PROTOCOL_VERSION_V2, ProtocolType};
use localsend::multicast::MulticastHandle;
use sukkula_core::Protocol;
use sukkula_core::limits::{HANDSHAKE_TIMEOUT, NETWORK_IDLE_TIMEOUT, OFFER_TIMEOUT};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use self::discovery::{Announced, Multicast};
use self::identity::Identity;
use self::peers::{Peers, Sighting};
use self::server::Server;
use crate::adapter::{Adapter, BoxFuture, Outgoing};
use crate::api::{Direction, ErrorCode, ErrorInfo, Event, SendTarget, TransferId};
use crate::ctx::Ctx;

/// LocalSend's port, for HTTPS and for multicast.
pub const DEFAULT_PORT: u16 = 53317;

/// How the adapter is set up. The engine uses [`Options::default`]; tests
/// use ephemeral ports and short timeouts. Not a user setting: nothing here
/// can make the adapter less strict than the spec -- every timeout is capped
/// at the spec's bound.
#[derive(Clone, Debug)]
pub struct Options {
    /// The HTTPS port; 0 picks a free one.
    pub port: u16,
    /// The multicast port, or `None` for no multicast at all.
    pub multicast_port: Option<u16>,
    /// Longest wait for a network read or write making no progress. Capped
    /// at [`NETWORK_IDLE_TIMEOUT`].
    pub idle_timeout: Duration,
    /// Longest connect, TLS handshake, request head or JSON body. Capped at
    /// [`HANDSHAKE_TIMEOUT`].
    pub handshake_timeout: Duration,
    /// Longest a sender waits for the receiving user to answer. Capped at
    /// [`OFFER_TIMEOUT`] plus [`NETWORK_IDLE_TIMEOUT`].
    pub prepare_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            port: DEFAULT_PORT,
            multicast_port: Some(DEFAULT_PORT),
            idle_timeout: NETWORK_IDLE_TIMEOUT,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            prepare_timeout: max_prepare_timeout(),
        }
    }
}

fn max_prepare_timeout() -> Duration {
    OFFER_TIMEOUT.saturating_add(NETWORK_IDLE_TIMEOUT)
}

impl Options {
    fn idle_timeout(&self) -> Duration {
        self.idle_timeout.min(NETWORK_IDLE_TIMEOUT)
    }

    fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout.min(HANDSHAKE_TIMEOUT)
    }

    fn prepare_timeout(&self) -> Duration {
        self.prepare_timeout.min(max_prepare_timeout())
    }
}

/// The adapter, as the hub uses it.
#[must_use]
pub fn adapter(ctx: Arc<Ctx>) -> Arc<dyn Adapter> {
    adapter_with(ctx, Options::default())
}

/// The adapter with other [`Options`], for tests.
#[must_use]
pub fn adapter_with(ctx: Arc<Ctx>, opts: Options) -> Arc<LocalSend> {
    Arc::new(LocalSend {
        shared: Arc::new(Shared {
            port: AtomicU16::new(if opts.port == 0 {
                DEFAULT_PORT
            } else {
                opts.port
            }),
            opts,
            ctx,
            tasks: TaskTracker::new(),
            receiving: AtomicBool::new(false),
            discovering: AtomicBool::new(false),
            peers: Mutex::new(Peers::default()),
            outgoing: Mutex::new(HashMap::new()),
            multicast: Mutex::new(None),
        }),
        identity: OnceCell::new(),
        state: AsyncMutex::new(State::default()),
    })
}

/// The LocalSend adapter.
pub struct LocalSend {
    shared: Arc<Shared>,
    identity: OnceCell<Arc<Identity>>,
    /// Serialises starting and stopping.
    state: AsyncMutex<State>,
}

#[derive(Default)]
struct State {
    server: Option<Server>,
    multicast: Option<Multicast>,
    discovery: Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
}

/// What the adapter's tasks share.
pub(crate) struct Shared {
    ctx: Arc<Ctx>,
    opts: Options,
    tasks: TaskTracker,
    receiving: AtomicBool,
    discovering: AtomicBool,
    /// The port we tell peers our server is on.
    port: AtomicU16,
    peers: Mutex<Peers>,
    /// Sends in flight, so a receiver's `POST /cancel` can reach them.
    outgoing: Mutex<HashMap<TransferId, OutgoingSend>>,
    /// The current multicast handle, for announcing.
    multicast: Mutex<Option<Arc<MulticastHandle>>>,
}

struct OutgoingSend {
    peer: IpAddr,
    session: Option<String>,
    cancelled_by_peer: bool,
}

impl Shared {
    fn receiving(&self) -> bool {
        self.receiving.load(Ordering::Acquire)
    }

    fn discovering(&self) -> bool {
        self.discovering.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn set_discovering(&self, on: bool) {
        self.discovering.store(on, Ordering::Release);
    }

    fn port(&self) -> u16 {
        self.port.load(Ordering::Acquire)
    }

    fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::Release);
    }

    fn multicast(&self) -> Option<Arc<MulticastHandle>> {
        lock(&self.multicast).clone()
    }

    fn model(&self) -> Option<String> {
        let m = self.ctx.device_model();
        (!m.is_empty()).then(|| m.to_owned())
    }

    /// What we tell peers about ourselves when we register or offer (F-C7).
    fn register_dto(&self, identity: &Identity) -> RegisterDtoV2 {
        RegisterDtoV2 {
            alias: self.ctx.device_name(),
            version: PROTOCOL_VERSION_V2.to_owned(),
            device_model: self.model(),
            device_type: Some(LsDeviceType::Mobile),
            fingerprint: identity.fingerprint().to_owned(),
            port: self.port(),
            protocol: ProtocolType::Https,
            download: false,
        }
    }

    fn register_response(&self, identity: &Identity) -> RegisterResponseDtoV2 {
        RegisterResponseDtoV2 {
            alias: self.ctx.device_name(),
            version: PROTOCOL_VERSION_V2.to_owned(),
            device_model: self.model(),
            device_type: Some(LsDeviceType::Mobile),
            fingerprint: identity.fingerprint().to_owned(),
            download: false,
        }
    }

    fn info_dto(&self, identity: &Identity) -> InfoResponseDtoV2 {
        InfoResponseDtoV2 {
            alias: self.ctx.device_name(),
            version: PROTOCOL_VERSION_V2.to_owned(),
            device_model: self.model(),
            device_type: Some(LsDeviceType::Mobile),
            fingerprint: identity.fingerprint().to_owned(),
            download: false,
        }
    }

    fn announced(&self) -> Announced {
        Announced {
            alias: self.ctx.device_name(),
            model: self.model(),
            port: self.port(),
        }
    }

    /// A peer proved its fingerprint; list it while discovering.
    fn saw_peer(&self, sighting: &Sighting<'_>) {
        if !self.discovering() {
            return;
        }
        let found = lock(&self.peers).upsert(sighting, Instant::now());
        if let Some(peer) = found {
            self.ctx.emit(Event::PeerFound { peer });
        }
    }

    fn expire_peers(&self) {
        let lost = lock(&self.peers).expire(Instant::now());
        for peer in lost {
            self.ctx.emit(Event::PeerLost { peer });
        }
    }

    fn clear_peers(&self) {
        let lost = lock(&self.peers).clear();
        for peer in lost {
            self.ctx.emit(Event::PeerLost { peer });
        }
    }

    fn outgoing_begin(&self, id: TransferId, peer: IpAddr) {
        lock(&self.outgoing).insert(
            id,
            OutgoingSend {
                peer,
                session: None,
                cancelled_by_peer: false,
            },
        );
    }

    fn outgoing_session(&self, id: TransferId, session: &str) {
        if let Some(o) = lock(&self.outgoing).get_mut(&id) {
            o.session = Some(session.to_owned());
        }
    }

    /// Forgets a send; true when the peer cancelled it.
    fn outgoing_end(&self, id: TransferId) -> bool {
        lock(&self.outgoing)
            .remove(&id)
            .is_some_and(|o| o.cancelled_by_peer)
    }

    /// A receiver at `peer` cancelled its session `session`: cancel the send
    /// that session belongs to, if it is one of ours to that address.
    fn remote_cancel(&self, peer: IpAddr, session: &str) {
        let id = {
            let mut outgoing = lock(&self.outgoing);
            let found = outgoing.iter_mut().find(|(_, o)| {
                o.peer == peer
                    && o.session
                        .as_deref()
                        .is_some_and(|s| wire::same_secret(s, session))
            });
            found.map(|(id, o)| {
                o.cancelled_by_peer = true;
                *id
            })
        };
        if let Some(id) = id {
            self.ctx.transfers().cancel(id);
        }
    }
}

/// An IPv4-mapped IPv6 address as the IPv4 address it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

impl LocalSend {
    /// Our identity, loaded or generated on first use -- off the async
    /// threads, since generating an RSA key takes a while.
    async fn identity(&self) -> Result<Arc<Identity>, ErrorInfo> {
        self.identity
            .get_or_try_init(|| async {
                let store = self.shared.ctx.store().clone();
                tokio::task::spawn_blocking(move || identity::load_or_create(&store))
                    .await
                    .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "key generation failed"))?
                    .map(Arc::new)
            })
            .await
            .cloned()
    }

    /// Our fingerprint, as peers see it.
    ///
    /// # Errors
    ///
    /// The identity could not be loaded or made.
    pub async fn fingerprint(&self) -> Result<String, ErrorInfo> {
        Ok(self.identity().await?.fingerprint().to_owned())
    }

    /// How many of the adapter's tasks are running: listeners, connections,
    /// sessions, sends, discovery. For tests that check everything ends.
    #[must_use]
    pub fn tasks_running(&self) -> usize {
        self.shared.tasks.len()
    }

    /// The HTTPS port while receiving.
    pub async fn port(&self) -> Option<u16> {
        let state = self.state.lock().await;
        state
            .server
            .as_ref()
            .filter(|s| !s.stopped())
            .map(Server::port)
    }

    /// Asks the LocalSend device at `addr` who it is and lists it as a peer:
    /// the register exchange discovery does for an announcement, for a known
    /// address (LocalSend's "favourites"). The certificate is trusted on
    /// first use and pinned from then on. Returns the peer id.
    ///
    /// # Errors
    ///
    /// Not discovering, the address is not permitted (S7), or the device did
    /// not answer as LocalSend.
    pub async fn discover_at(&self, addr: SocketAddr) -> Result<String, ErrorInfo> {
        if !self.shared.discovering() {
            return Err(ErrorInfo::new(ErrorCode::Unavailable, "not discovering"));
        }
        let identity = self.identity().await?;
        let (fingerprint, says) = client::introduce(&self.shared, &identity, addr, None)
            .await
            .map_err(client::Failure::into_error)?;
        if fingerprint == identity.fingerprint() {
            return Err(ErrorInfo::new(ErrorCode::NotFound, "that is this device"));
        }
        self.shared.saw_peer(&Sighting {
            fingerprint: &fingerprint,
            addr,
            alias: &says.alias,
            model: says.model.as_deref(),
            device_type: wire::device_type(says.device_type.as_ref()),
        });
        Ok(wire::peer_id(&fingerprint))
    }

    /// Starts or restarts the multicast socket if it should run, and stops
    /// it if it should not.
    async fn update_multicast(&self, state: &mut State, identity: &Arc<Identity>) {
        let wanted = self.shared.receiving() || self.shared.discovering();
        let announced = self.shared.announced();
        let current = state
            .multicast
            .as_ref()
            .is_some_and(|m| *m.announced() == announced);
        if wanted && current {
            return;
        }
        if let Some(old) = state.multicast.take() {
            *lock(&self.shared.multicast) = None;
            old.stop().await;
        }
        if !wanted {
            return;
        }
        let Some(port) = self.shared.opts.multicast_port else {
            return;
        };
        if let Some(m) = Multicast::start(&self.shared, identity, port, announced).await {
            *lock(&self.shared.multicast) = Some(m.handle());
            state.multicast = Some(m);
        }
    }
}

impl Adapter for LocalSend {
    fn protocol(&self) -> Protocol {
        Protocol::LocalSend
    }

    fn start_receiving(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        Box::pin(async move {
            let identity = self.identity().await?;
            let mut state = self.state.lock().await;
            match state.server.as_ref() {
                Some(server) if !server.stopped() => server.resume(),
                _ => {
                    if let Some(old) = state.server.take() {
                        old.wait().await;
                    }
                    state.server =
                        Some(Server::start(self.shared.clone(), identity.clone()).await?);
                }
            }
            self.shared.receiving.store(true, Ordering::Release);
            self.update_multicast(&mut state, &identity).await;
            // Senders looking right now hear of us at once.
            if let Some(m) = state.multicast.as_ref() {
                m.announce_in_background(&self.shared);
            }
            Ok(())
        })
    }

    fn stop_receiving(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            self.shared.receiving.store(false, Ordering::Release);
            if let Some(server) = state.server.as_ref()
                && server.drain()
                && let Some(server) = state.server.take()
            {
                server.wait().await;
            }
            if let Some(identity) = self.identity.get().cloned() {
                self.update_multicast(&mut state, &identity).await;
            }
        })
    }

    fn start_discovery(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        Box::pin(async move {
            let identity = self.identity().await?;
            let mut state = self.state.lock().await;
            self.shared.discovering.store(true, Ordering::Release);
            self.update_multicast(&mut state, &identity).await;
            if state.discovery.is_none() {
                let cancel = self.shared.ctx.shutdown_token().child_token();
                let task = self
                    .shared
                    .tasks
                    .spawn(discovery::run(self.shared.clone(), cancel.clone()));
                state.discovery = Some((cancel, task));
            }
            Ok(())
        })
    }

    fn stop_discovery(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            self.shared.discovering.store(false, Ordering::Release);
            if let Some((cancel, task)) = state.discovery.take() {
                cancel.cancel();
                let _ = task.await;
            }
            self.shared.clear_peers();
            if let Some(identity) = self.identity.get().cloned() {
                self.update_multicast(&mut state, &identity).await;
            }
        })
    }

    fn send(
        &self,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        Box::pin(async move {
            let SendTarget::LocalSend { peer } = target else {
                return Err(ErrorInfo::new(
                    ErrorCode::BadCommand,
                    "not a LocalSend target",
                ));
            };
            let target = lock(&self.shared.peers)
                .get(&peer)
                .ok_or_else(|| ErrorInfo::new(ErrorCode::NotFound, "no such peer"))?;
            let identity = self.identity().await?;
            let files: Vec<_> = items.iter().map(Outgoing::view).collect();
            let total = files.iter().fold(0u64, |acc, f| acc.saturating_add(f.size));
            let ctx = &self.shared.ctx;
            let transfer = ctx
                .transfers()
                .begin_views(
                    ctx,
                    Direction::Outgoing,
                    Protocol::LocalSend,
                    &target.name,
                    files,
                    total,
                )
                .ok_or_else(|| ErrorInfo::new(ErrorCode::TooLarge, "too many transfers running"))?;
            let id = transfer.id();
            self.shared.tasks.spawn(send::run(
                self.shared.clone(),
                identity,
                target,
                items,
                transfer,
            ));
            Ok(id)
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sukkula_core::config::Settings;
    use sukkula_core::consent::ConsentBroker;
    use sukkula_core::inbox::Inbox;
    use sukkula_core::reach::ReachPolicy;
    use sukkula_core::store::Store;

    /// A `Shared` over a throwaway context.
    pub(crate) fn shared_for_tests(allow_loopback: bool) -> (tempfile::TempDir, Arc<Shared>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("data")).unwrap();
        let inbox = Inbox::open(&dir.path().join("dl")).unwrap();
        let consent = ConsentBroker::new(Arc::new(|_| {}));
        let ctx = Arc::new(Ctx::new(
            Settings::default(),
            "Test".into(),
            store,
            inbox,
            consent,
            ReachPolicy { allow_loopback },
            Arc::new(|_| {}),
        ));
        let adapter = adapter_with(ctx, Options::default());
        (dir, adapter.shared.clone())
    }

    #[test]
    fn timeouts_never_exceed_the_spec() {
        let o = Options {
            idle_timeout: Duration::from_secs(3600),
            handshake_timeout: Duration::from_secs(3600),
            prepare_timeout: Duration::from_secs(3600),
            ..Options::default()
        };
        assert_eq!(o.idle_timeout(), NETWORK_IDLE_TIMEOUT);
        assert_eq!(o.handshake_timeout(), HANDSHAKE_TIMEOUT);
        assert_eq!(o.prepare_timeout(), max_prepare_timeout());
    }

    #[test]
    fn mapped_addresses_are_canonical() {
        assert_eq!(
            canonical("::ffff:10.0.0.1".parse().unwrap()),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            canonical("fe80::1".parse().unwrap()),
            "fe80::1".parse::<IpAddr>().unwrap()
        );
    }
}
