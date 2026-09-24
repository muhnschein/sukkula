//! F-LS1: multicast discovery, through the upstream crate's multicast
//! module, and the HTTPS register exchange that answers it.
//!
//! Upstream's `discovery` module is not used: it answers every announcement
//! it hears -- from any address, at any rate, over plain HTTP when the
//! announcement says `http` -- by spawning a register request to wherever
//! the announcement points. Here an announcement is screened first
//! ([`screen`]): the source must be permitted and within its rate (S7), the
//! announced protocol must be HTTPS (F-LS2), the fingerprint well-formed.
//! Answers are few at a time and pinned to the announced fingerprint, so
//! nothing is sent to a host that does not hold the announced certificate,
//! and a peer only enters the table after that handshake.
//!
//! The multicast socket runs while receiving (so senders looking for us get
//! an answer) or discovering (so we hear who is there), and announces when
//! either starts, and periodically while discovering.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use localsend::model::discovery::{DeviceType as LsDeviceType, MulticastMessageV2, ProtocolType};
use localsend::multicast::{
    self, DEFAULT_MULTICAST_GROUP, MulticastConfig, MulticastDevice, MulticastEvent,
    MulticastHandle,
};
use localsend::util::interface::InterfaceFilter;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::identity::Identity;
use super::peers::Sighting;
use super::{Shared, canonical, client, wire};

/// Announcements queued between the multicast socket and us. When full,
/// upstream stops reading the socket and the kernel drops datagrams.
const QUEUE: usize = 16;

/// Register answers in flight at once; announcements beyond are ignored.
const MAX_ANSWERS: usize = 4;

/// An answer, connect to response, takes at most this long.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(3);

/// How often discovery announces while it runs.
pub(super) const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// How often stale peers are swept.
const SWEEP_INTERVAL: Duration = Duration::from_secs(10);

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
    /// carry multicast: discovery then works through registrations alone.
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
    // Last, so that only well-formed announcements spend the address's
    // budget.
    if !shared.ctx.allow_discovery(ip) {
        return None;
    }
    Some(Candidate {
        addr: SocketAddr::new(ip, port),
        fingerprint,
    })
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
            continue;
        };
        let task_shared = shared.clone();
        let task_identity = identity.clone();
        let task_cancel = cancel.clone();
        shared.tasks.spawn(async move {
            let _permit = permit;
            tokio::select! {
                () = task_cancel.cancelled() => {}
                () = answer(&task_shared, &task_identity, candidate) => {}
            }
        });
    }
}

/// Introduces ourselves to an announcer, pinned to its announced
/// fingerprint, and lists it if it proves that fingerprint.
async fn answer(shared: &Arc<Shared>, identity: &Identity, candidate: Candidate) {
    let introduced = tokio::time::timeout(
        ANSWER_TIMEOUT,
        client::introduce(
            shared,
            identity,
            candidate.addr,
            Some(&candidate.fingerprint),
        ),
    )
    .await;
    match introduced {
        Ok(Ok((fingerprint, says))) => shared.saw_peer(&Sighting {
            fingerprint: &fingerprint,
            addr: candidate.addr,
            alias: &says.alias,
            model: says.model.as_deref(),
            device_type: wire::device_type(says.device_type.as_ref()),
        }),
        _ => tracing::debug!("an announcing LocalSend peer did not answer"),
    }
}

/// While discovering: announce now and every [`ANNOUNCE_INTERVAL`], and
/// sweep peers that went quiet.
pub(super) async fn run(shared: Arc<Shared>, cancel: CancellationToken) {
    let mut announce = tokio::time::interval(ANNOUNCE_INTERVAL);
    let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = announce.tick() => {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localsend::tests::shared_for_tests;

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
        assert_eq!(
            answered,
            usize::try_from(sukkula_core::limits::DISCOVERY_BURST).unwrap()
        );
        // Another address has its own budget.
        assert!(screen(&shared, &own, "192.168.1.31".parse().unwrap(), &ok).is_some());
    }
}
