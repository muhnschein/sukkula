//! F-LS1: multicast discovery, through the upstream crate's multicast
//! module; the HTTPS register exchange that answers it; and the HTTP
//! register fallback.
//!
//! Upstream's `discovery` module is not used: it answers every announcement
//! it hears -- from any address, at any rate, over plain HTTP when the
//! announcement says `http` -- by spawning a register request to wherever
//! the announcement points. Here an announcement is screened first
//! ([`screen`]): the source must be permitted and within its budget (S7,
//! [`Claims`]), the announced protocol must be HTTPS (F-LS2), the
//! fingerprint well-formed. Answers are few at a time and pinned to the
//! announced fingerprint, so nothing is sent to a host that does not hold
//! the announced certificate, and a peer only enters the table after that
//! handshake.
//!
//! # The HTTP register fallback
//!
//! Upstream's fallback, for networks that do not carry multicast, is a
//! scan: a register request to every host of each `/24` of the phone's
//! interfaces when multicast confirms nobody within a second. That is 254
//! TLS connections per interface to hosts that never said they speak
//! LocalSend -- a probe of every neighbour from the phone, well past what
//! S7's per-address budget is for. Ours registers only with LocalSend
//! servers it already knows of ([`fallback`]): peers found by multicast or
//! in an earlier discovery, peers that registered with our server over
//! HTTPS, and announcements that could not be answered at once. It runs
//! when discovery starts and every [`Options::announce_interval`] after,
//! over HTTPS only (F-LS2), pinned to the fingerprint each was known by
//! (F-LS3), to permitted addresses only (S7), each at most once a round and
//! a few rounds in all (`peers::Leads`). That covers multicast that gets
//! through in one direction only, answers lost to a busy moment, and peers
//! that stop being heard; a network with no multicast at all is covered
//! when the other side's own scan registers with our server. A phone that
//! only sends, on a network with no multicast, finds nobody it has not met:
//! that is the price of not scanning.
//!
//! The multicast socket runs while receiving (so senders looking for us get
//! an answer) or discovering (so we hear who is there), and announces when
//! either starts, and periodically while discovering.
//!
//! [`Options::announce_interval`]: super::Options::announce_interval

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use localsend::model::discovery::{DeviceType as LsDeviceType, MulticastMessageV2, ProtocolType};
use localsend::multicast::{
    self, DEFAULT_MULTICAST_GROUP, MulticastConfig, MulticastDevice, MulticastEvent,
    MulticastHandle,
};
use localsend::util::interface::InterfaceFilter;
use sukkula_core::limits::{DISCOVERY_BURST, DISCOVERY_WINDOW, RATE_LIMIT_ENTRIES};
use sukkula_core::reach::RateLimiter;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::client::Failure;
use super::identity::Identity;
use super::peers::{PEER_TTL, Sighting};
use super::{DEFAULT_PORT, Shared, canonical, client, lock, wire};

/// Announcements queued between the multicast socket and us. When full,
/// upstream stops reading the socket and the kernel drops datagrams.
const QUEUE: usize = 16;

/// Register answers in flight at once; announcements beyond wait for the
/// fallback's next round. The fallback's rounds have as many again.
const MAX_ANSWERS: usize = 4;

/// An answer, connect to response, takes at most this long.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(3);

/// How often discovery announces, and the fallback runs, while it runs.
pub(super) const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// How often stale peers are swept.
const SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// How long what a server showed in a handshake is trusted ([`Claims`]).
const SHOWN_TTL: Duration = PEER_TTL;

/// What we announce; a change restarts the socket with the new details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Announced {
    pub(super) alias: String,
    pub(super) model: Option<String>,
    pub(super) port: u16,
}

/// The running multicast socket.
pub(super) struct Multicast {
    handle: Arc<MulticastHandle>,
    stop: Option<oneshot::Sender<()>>,
    cancel: CancellationToken,
    consumer: tokio::task::JoinHandle<()>,
    announced: Announced,
}

impl Multicast {
    /// Binds the multicast socket on `port`. `None` when no interface can
    /// carry multicast: discovery then works through registrations and the
    /// fallback alone.
    pub(super) async fn start(
        shared: &Arc<Shared>,
        identity: &Arc<Identity>,
        port: u16,
        announced: Announced,
    ) -> Option<Multicast> {
        let (tx, rx) = mpsc::channel(QUEUE);
        let (stop_tx, stop_rx) = oneshot::channel();
        let config = MulticastConfig {
            group: DEFAULT_MULTICAST_GROUP,
            // IPv4 only, like the server.
            group_v6: None,
            port,
            interface_filter: InterfaceFilter::default(),
            device: MulticastDevice {
                alias: announced.alias.clone(),
                version: localsend::model::discovery::PROTOCOL_VERSION_V2.to_owned(),
                device_model: announced.model.clone(),
                device_type: Some(LsDeviceType::Mobile),
                fingerprint: identity.fingerprint().to_owned(),
                port: announced.port,
                protocol: ProtocolType::Https,
                download: false,
            },
            event_tx: tx,
        };
        let handle = match multicast::start(config, stop_rx).await {
            Ok(handle) => Arc::new(handle),
            Err(_) => {
                tracing::debug!("LocalSend multicast unavailable");
                return None;
            }
        };
        let cancel = shared.ctx.shutdown_token().child_token();
        let consumer = shared.tasks.spawn(consume(
            shared.clone(),
            identity.clone(),
            rx,
            cancel.clone(),
        ));
        Some(Multicast {
            handle,
            stop: Some(stop_tx),
            cancel,
            consumer,
            announced,
        })
    }

    /// What it announces.
    pub(super) fn announced(&self) -> &Announced {
        &self.announced
    }

    /// The handle announcements go out through.
    pub(super) fn handle(&self) -> Arc<MulticastHandle> {
        self.handle.clone()
    }

    /// Announces once, in a task that ends with the socket.
    pub(super) fn announce_in_background(&self, shared: &Shared) {
        let handle = self.handle.clone();
        let cancel = self.cancel.clone();
        shared.tasks.spawn(async move {
            tokio::select! {
                () = cancel.cancelled() => {}
                () = handle.announce() => {}
            }
        });
    }

    /// Closes the socket and ends every task it started.
    pub(super) async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.cancel.cancel();
        self.handle.wait_stopped().await;
        let _ = self.consumer.await;
    }
}

/// Where to answer an announcement, once it passed [`screen`].
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) addr: SocketAddr,
    pub(super) fingerprint: String,
}

/// S7 for answers to announcements, and what each server showed.
///
/// An announcement is a UDP datagram: its source address is whatever the
/// sender wrote, so it proves nothing, and neither does anything in it.
/// Answering one costs the address it names a TLS handshake. So:
///
/// - Answers are limited per address an announcement names, in a budget of
///   LocalSend's own. The engine's shared discovery budget
///   (`Ctx::allow_discovery`) is for requests that came over TCP and TLS --
///   `/register` and `/info` to our server, Quick Share's services -- and
///   forged datagrams spending it, as they once did, hid the address they
///   named from all of those. LocalSend's own port has a budget apart from
///   every other port's, so a flood naming other ports cannot use it up.
/// - A claim matching what the server at that address last showed in a
///   handshake is answered once per [`DISCOVERY_WINDOW`], budget or none. A
///   flood of forged claims naming a device's address can spend the budget,
///   but the first of them we answer shows us the device's certificate (a
///   mismatch, [`Failure::Mismatch`]), and from then on the device's own
///   announcements, which name that certificate, get through. Forging the
///   device's claim exactly only makes us answer the device.
pub(super) struct Claims {
    /// Answers to claims naming LocalSend's port, per address.
    default_port: RateLimiter,
    /// Answers to claims naming any other port, per address.
    other_ports: RateLimiter,
    /// What each server showed, at most [`RATE_LIMIT_ENTRIES`] of them.
    shown: HashMap<SocketAddr, Shown>,
}

/// A certificate a server showed in a handshake.
struct Shown {
    fingerprint: String,
    at: Instant,
    /// When a claim naming it was last answered.
    answered: Option<Instant>,
}

impl Default for Claims {
    fn default() -> Self {
        Claims {
            default_port: RateLimiter::new(DISCOVERY_BURST, DISCOVERY_WINDOW),
            other_ports: RateLimiter::new(DISCOVERY_BURST, DISCOVERY_WINDOW),
            shown: HashMap::new(),
        }
    }
}

impl Claims {
    /// Whether to answer an announcement claiming that the server at
    /// `addr` holds `fingerprint`.
    pub(super) fn admit(&mut self, addr: SocketAddr, fingerprint: &str, now: Instant) -> bool {
        if let Some(shown) = self.shown.get_mut(&addr)
            && now.saturating_duration_since(shown.at) < SHOWN_TTL
            && shown.fingerprint == fingerprint
        {
            let recent = shown
                .answered
                .is_some_and(|t| now.saturating_duration_since(t) < DISCOVERY_WINDOW);
            if !recent {
                shown.answered = Some(now);
            }
            return !recent;
        }
        let budget = if addr.port() == DEFAULT_PORT {
            &mut self.default_port
        } else {
            &mut self.other_ports
        };
        budget.allow(addr.ip(), now)
    }

    /// The server at `addr` showed `fingerprint` in a handshake; `proven`
    /// when it went on to prove it, which answers it too.
    pub(super) fn showed(
        &mut self,
        addr: SocketAddr,
        fingerprint: &str,
        proven: bool,
        now: Instant,
    ) {
        if let Some(shown) = self.shown.get_mut(&addr)
            && shown.fingerprint == fingerprint
        {
            shown.at = now;
            if proven {
                shown.answered = Some(now);
            }
            return;
        }
        if !self.shown.contains_key(&addr) && self.shown.len() >= RATE_LIMIT_ENTRIES {
            // Forgetting costs a handshake more when the address is named
            // again; the budget still bounds how often.
            if let Some(stalest) = self
                .shown
                .iter()
                .min_by_key(|(_, s)| s.at)
                .map(|(addr, _)| *addr)
            {
                self.shown.remove(&stalest);
            }
        }
        self.shown.insert(
            addr,
            Shown {
                fingerprint: fingerprint.to_owned(),
                at: now,
                answered: proven.then_some(now),
            },
        );
    }

    /// Whether the server at `addr` lately showed a certificate other than
    /// `fingerprint`: registering with it, pinned to that, would only fail.
    pub(super) fn contradicts(&self, addr: SocketAddr, fingerprint: &str, now: Instant) -> bool {
        self.shown.get(&addr).is_some_and(|s| {
            now.saturating_duration_since(s.at) < SHOWN_TTL && s.fingerprint != fingerprint
        })
    }
}

/// S7 and F-LS2 on one announcement. The datagram has been parsed by
/// upstream by now -- at most 64 KiB of JSON, the only thing read before
/// this check; nothing is sent and nothing is kept for one that fails it.
pub(super) fn screen(
    shared: &Shared,
    own_fingerprint: &str,
    source: IpAddr,
    message: &MulticastMessageV2,
) -> Option<Candidate> {
    if !(shared.receiving() || shared.discovering()) {
        return None;
    }
    let ip = canonical(source);
    if !shared.ctx.permits(ip) {
        return None;
    }
    let (port, fingerprint) = answerable(own_fingerprint, message)?;
    let addr = SocketAddr::new(ip, port);
    // Last, so that only well-formed announcements spend a budget -- and
    // LocalSend's own, never the one TLS requests and Quick Share use.
    if !lock(&shared.claims).admit(addr, &fingerprint, Instant::now()) {
        return None;
    }
    Some(Candidate { addr, fingerprint })
}

/// The part of [`screen`] that looks at the announcement alone, so the
/// fuzz target reaches it without a context: F-LS2 (HTTPS only), a port,
/// and a well-formed fingerprint that is not ours. The port to answer on
/// and the fingerprint to pin the answer to, uppercase.
pub(super) fn answerable(
    own_fingerprint: &str,
    message: &MulticastMessageV2,
) -> Option<(u16, String)> {
    if message.protocol != ProtocolType::Https || message.port == 0 {
        return None;
    }
    let fingerprint = wire::fingerprint(&message.fingerprint)?;
    if fingerprint == own_fingerprint {
        return None;
    }
    Some((message.port, fingerprint))
}

async fn consume(
    shared: Arc<Shared>,
    identity: Arc<Identity>,
    mut rx: mpsc::Receiver<MulticastEvent>,
    cancel: CancellationToken,
) {
    let permits = Arc::new(Semaphore::new(MAX_ANSWERS));
    loop {
        let event = tokio::select! {
            () = cancel.cancelled() => break,
            event = rx.recv() => event,
        };
        let Some(event) = event else { break };
        let MulticastEvent::Discovered { ip, message, .. } = event else {
            continue;
        };
        let Some(candidate) = screen(&shared, identity.fingerprint(), ip, &message) else {
            continue;
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            // Busy: the fallback's next round tries it.
            let now = Instant::now();
            lock(&shared.leads).add(candidate.addr, &candidate.fingerprint, false, now);
            continue;
        };
        let task_shared = shared.clone();
        let task_identity = identity.clone();
        let task_cancel = cancel.clone();
        shared.tasks.spawn(async move {
            let _permit = permit;
            let Candidate { addr, fingerprint } = candidate;
            tokio::select! {
                () = task_cancel.cancelled() => {}
                answered = reach(&task_shared, &task_identity, addr, &fingerprint) => {
                    if answered == Answered::Silent {
                        // No answer at all: the fallback's next round tries
                        // again.
                        let now = Instant::now();
                        lock(&task_shared.leads).add(addr, &fingerprint, false, now);
                    }
                }
            }
        });
    }
}

/// How a server answered [`reach`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answered {
    /// It proved the fingerprint.
    Proved,
    /// It showed another certificate: nothing was sent.
    Other,
    /// Nothing usable: no connection, a timeout, an error status.
    Silent,
}

/// Introduces ourselves to the server at `addr`, pinned to `fingerprint`,
/// and lists it if it proves that fingerprint. What it showed is kept
/// ([`Claims`]) either way.
async fn reach(
    shared: &Arc<Shared>,
    identity: &Identity,
    addr: SocketAddr,
    fingerprint: &str,
) -> Answered {
    let introduced = tokio::time::timeout(
        ANSWER_TIMEOUT,
        client::introduce(shared, identity, addr, Some(fingerprint)),
    )
    .await;
    let now = Instant::now();
    match introduced {
        Ok(Ok((proven, says))) => {
            lock(&shared.claims).showed(addr, &proven, true, now);
            shared.saw_peer(&Sighting {
                fingerprint: &proven,
                addr,
                alias: &says.alias,
                model: says.model.as_deref(),
                device_type: wire::device_type(says.device_type.as_ref()),
            });
            Answered::Proved
        }
        Ok(Err(Failure::Mismatch(shown))) => {
            if let Some(shown) = shown {
                lock(&shared.claims).showed(addr, &shown, false, now);
            }
            lock(&shared.leads).settle(addr, fingerprint);
            tracing::debug!("a LocalSend server showed another certificate than it was known by");
            Answered::Other
        }
        _ => {
            tracing::debug!("a LocalSend server did not answer");
            Answered::Silent
        }
    }
}

/// One round of the HTTP register fallback (see the module doc): every
/// peer not heard from for a round, and every lead, once, pinned.
async fn fallback(shared: Arc<Shared>, identity: Arc<Identity>, cancel: CancellationToken) {
    let now = Instant::now();
    let round = shared.opts.announce_interval();
    let quiet = lock(&shared.peers).quiet(now, round);
    let leads = lock(&shared.leads).due(now);
    let mut targets: Vec<(SocketAddr, String)> =
        Vec::with_capacity(quiet.len().saturating_add(leads.len()));
    let mut refuted = Vec::new();
    {
        let claims = lock(&shared.claims);
        for target in quiet.into_iter().chain(leads) {
            if claims.contradicts(target.0, &target.1, now) {
                refuted.push(target);
            } else if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
    {
        let mut leads = lock(&shared.leads);
        for (addr, fingerprint) in &refuted {
            leads.settle(*addr, fingerprint);
        }
    }
    let permits = Arc::new(Semaphore::new(MAX_ANSWERS));
    let mut running = Vec::with_capacity(targets.len());
    for (addr, fingerprint) in targets {
        let permit = tokio::select! {
            () = cancel.cancelled() => break,
            permit = permits.clone().acquire_owned() => permit,
        };
        let Ok(permit) = permit else { break };
        let task_shared = shared.clone();
        let task_identity = identity.clone();
        let task_cancel = cancel.clone();
        running.push(shared.tasks.spawn(async move {
            let _permit = permit;
            tokio::select! {
                () = task_cancel.cancelled() => {}
                answered = reach(&task_shared, &task_identity, addr, &fingerprint) => {
                    if answered == Answered::Silent {
                        lock(&task_shared.leads).missed(addr, &fingerprint);
                    }
                }
            }
        }));
    }
    for task in running {
        let _ = task.await;
    }
}

/// While discovering: announce now and every round, run the fallback beside
/// each announcement (one round at a time), and sweep peers that went
/// quiet.
pub(super) async fn run(shared: Arc<Shared>, identity: Arc<Identity>, cancel: CancellationToken) {
    let mut announce = tokio::time::interval(shared.opts.announce_interval());
    let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
    let mut round: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = announce.tick() => {
                if round.as_ref().is_none_or(tokio::task::JoinHandle::is_finished) {
                    round = Some(shared.tasks.spawn(fallback(
                        shared.clone(),
                        identity.clone(),
                        cancel.clone(),
                    )));
                }
                if let Some(handle) = shared.multicast() {
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        () = handle.announce() => {}
                    }
                }
            }
            _ = sweep.tick() => shared.expire_peers(),
        }
    }
    if let Some(round) = round {
        let _ = round.await;
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // Test scenes add times and ports.
mod tests {
    use super::*;
    use crate::adapter::Adapter;
    use crate::localsend::tests::{seeded_adapter, shared_for_tests};

    fn message(protocol: ProtocolType, fingerprint: &str, port: u16) -> MulticastMessageV2 {
        MulticastMessageV2 {
            alias: "x".into(),
            version: "2.2".into(),
            device_model: None,
            device_type: None,
            fingerprint: fingerprint.into(),
            port,
            protocol,
            download: false,
        }
    }

    fn junk(i: usize, port: u16) -> MulticastMessageV2 {
        message(ProtocolType::Https, &format!("{i:064X}"), port)
    }

    fn burst() -> usize {
        usize::try_from(DISCOVERY_BURST).unwrap()
    }

    #[test]
    fn announcements_are_screened_before_anything_is_sent() {
        let (_dir, shared) = shared_for_tests(false);
        let own = "AA".repeat(32);
        let peer = "bb".repeat(32);
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let ok = message(ProtocolType::Https, &peer, 53317);

        // Neither receiving nor discovering: nothing is answered.
        assert_eq!(screen(&shared, &own, lan, &ok), None);
        shared.set_discovering(true);

        let found = screen(&shared, &own, lan, &ok).unwrap();
        assert_eq!(found.addr, "192.168.1.20:53317".parse().unwrap());
        assert_eq!(found.fingerprint, "BB".repeat(32));

        // S7: public, CGNAT, loopback (not allowed here) and v4-mapped public.
        for bad in [
            "8.8.8.8",
            "100.64.1.1",
            "127.0.0.1",
            "::ffff:8.8.8.8",
            "2001:db8::1",
        ] {
            assert_eq!(
                screen(&shared, &own, bad.parse().unwrap(), &ok),
                None,
                "{bad}"
            );
        }
        // A v4-mapped LAN address is the LAN address.
        let mapped = screen(&shared, &own, "::ffff:192.168.1.21".parse().unwrap(), &ok).unwrap();
        assert_eq!(mapped.addr.ip(), "192.168.1.21".parse::<IpAddr>().unwrap());

        // F-LS2: plain HTTP peers are never answered.
        assert_eq!(
            screen(
                &shared,
                &own,
                lan,
                &message(ProtocolType::Http, &peer, 53317)
            ),
            None
        );
        // Nonsense fingerprints, our own, port 0.
        assert_eq!(
            screen(
                &shared,
                &own,
                lan,
                &message(ProtocolType::Https, "random string", 1)
            ),
            None
        );
        assert_eq!(
            screen(&shared, &own, lan, &message(ProtocolType::Https, &own, 1)),
            None
        );
        assert_eq!(
            screen(&shared, &own, lan, &message(ProtocolType::Https, &peer, 0)),
            None
        );
    }

    #[test]
    fn announcements_are_rate_limited_per_address() {
        let (_dir, shared) = shared_for_tests(false);
        shared.set_discovering(true);
        let own = "AA".repeat(32);
        let ok = message(ProtocolType::Https, &"BB".repeat(32), 53317);
        let a: IpAddr = "192.168.1.30".parse().unwrap();
        let answered = (0..100)
            .filter(|_| screen(&shared, &own, a, &ok).is_some())
            .count();
        assert_eq!(answered, burst());
        // Another address has its own budget.
        assert!(screen(&shared, &own, "192.168.1.31".parse().unwrap(), &ok).is_some());
    }

    /// Datagrams forged with a device's address as their source once spent
    /// the engine's shared budget for that address, and so hid it: its TLS
    /// registration got 429, its Quick Share service was dropped, and its
    /// own announcements were screened out.
    #[test]
    fn forged_announcements_cannot_hide_the_address_they_name() {
        let (_dir, shared) = shared_for_tests(false);
        shared.set_discovering(true);
        let own = "AA".repeat(32);
        let device: IpAddr = "192.168.1.20".parse().unwrap();
        let real = "BB".repeat(32);
        let forged = (0..100)
            .filter(|i| screen(&shared, &own, device, &junk(*i, DEFAULT_PORT)).is_some())
            .count();
        assert_eq!(forged, burst(), "still limited per address (S7)");
        // The budget the device's own TLS requests, and Quick Share, are
        // held to is untouched.
        assert!(shared.ctx.allow_discovery(device));

        // The first forgery answered showed us the device's certificate...
        let now = Instant::now();
        let at = SocketAddr::new(device, DEFAULT_PORT);
        lock(&shared.claims).showed(at, &real, false, now);
        // ...so its own announcement gets through, budget or none, once
        // per window, forgeries before and after notwithstanding.
        let own_claim = message(ProtocolType::Https, &real, DEFAULT_PORT);
        assert!(screen(&shared, &own, device, &own_claim).is_some());
        for i in 100..200 {
            assert!(screen(&shared, &own, device, &junk(i, DEFAULT_PORT)).is_none());
        }
        assert!(
            screen(&shared, &own, device, &own_claim).is_none(),
            "once per window"
        );
        let mut claims = lock(&shared.claims);
        let later = now + DISCOVERY_WINDOW + Duration::from_secs(1);
        assert!(claims.admit(at, &real, later));
        assert!(claims.contradicts(at, &"CC".repeat(32), later));
        assert!(!claims.contradicts(at, &real, later));
        // What a server showed is trusted only so long.
        assert!(!claims.contradicts(at, &"CC".repeat(32), now + SHOWN_TTL));
    }

    #[test]
    fn claims_naming_other_ports_cannot_use_up_localsends_own() {
        let (_dir, shared) = shared_for_tests(false);
        shared.set_discovering(true);
        let own = "AA".repeat(32);
        let device: IpAddr = "192.168.1.20".parse().unwrap();
        let forged = (0..100u16)
            .filter(|i| screen(&shared, &own, device, &junk(usize::from(*i), 1000 + i)).is_some())
            .count();
        assert_eq!(forged, burst());
        let own_claim = message(ProtocolType::Https, &"BB".repeat(32), DEFAULT_PORT);
        assert!(screen(&shared, &own, device, &own_claim).is_some());
    }

    #[test]
    fn what_servers_showed_is_bounded() {
        let mut claims = Claims::default();
        let t0 = Instant::now();
        for i in 0..2000u32 {
            let addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::from(0x0A00_0000 | i)), 1);
            claims.showed(addr, "X", false, t0);
        }
        assert!(claims.shown.len() <= RATE_LIMIT_ENTRIES);
    }

    /// The whole path, over loopback TLS: a forged claim naming a device's
    /// address is answered, the device shows its certificate, and then the
    /// device's own claim is answered and lists it although the forgeries
    /// used the budget up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_forgery_answered_shows_the_device_it_named() {
        let (_d1, device) = seeded_adapter(1);
        device.start_receiving().await.unwrap();
        let port = device.port().await.unwrap();
        let real = device.fingerprint().await.unwrap();

        let (_d2, us) = seeded_adapter(0);
        let identity = us.identity().await.unwrap();
        let shared = us.shared.clone();
        shared.set_discovering(true);
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let addr = SocketAddr::new(loopback, port);
        let own = identity.fingerprint().to_owned();

        let first = screen(&shared, &own, loopback, &junk(0, port)).unwrap();
        assert_eq!(
            reach(&shared, &identity, first.addr, &first.fingerprint).await,
            Answered::Other
        );
        assert!(
            lock(&shared.peers)
                .get(&wire::peer_id(&first.fingerprint))
                .is_none()
        );
        while screen(&shared, &own, loopback, &junk(1, port)).is_some() {}
        let own_claim = screen(
            &shared,
            &own,
            loopback,
            &message(ProtocolType::Https, &real, port),
        )
        .unwrap();
        assert_eq!(own_claim.addr, addr);
        assert_eq!(
            reach(&shared, &identity, addr, &own_claim.fingerprint).await,
            Answered::Proved
        );
        assert_eq!(
            lock(&shared.peers).get(&wire::peer_id(&real)).unwrap().addr,
            addr
        );
        device.stop_receiving().await;
    }
}
