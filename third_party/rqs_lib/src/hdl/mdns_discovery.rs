use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use mdns_sd::{IfKind, IfPredicate, MDNS_PORT, ServiceDaemon, ServiceEvent, SourcePredicate};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::DeviceType;
use crate::utils::{is_not_self_ip, parse_mdns_endpoint_info, parse_mdns_name};

/// Most services remembered at once. mDNS is open to anyone on the link,
/// and every announcement used to add an entry, without limit.
pub const MAX_DISCOVERED_ENDPOINTS: usize = 64;

/// Most services remembered from one source: the address of the packets
/// that announced them, as mdns-sd reports it -- not the address an
/// announcement names, which is the announcer's to choose. Past this, and
/// when the cache is full, the rule in [`make_room`] applies.
pub const MAX_ENDPOINTS_PER_SOURCE: usize = 4;

/// How long stopping waits for the mDNS daemon thread to acknowledge.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

/// Which addresses a service may be reached at, which interfaces the
/// daemon listens on, and which sources it reads packets from at all: the
/// embedding application's reach policy.
pub type AddrFilter = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

/// The reach policy as mdns-sd's source filter (S7): a packet from an
/// address it refuses is dropped unread, neither answered nor cached --
/// which, for a legacy-unicast query, also means no reply sent there.
pub(crate) fn source_filter(filter: &AddrFilter) -> SourcePredicate {
    let filter = filter.clone();
    SourcePredicate::new(move |ip| filter(*ip))
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EndpointInfo {
    pub fullname: String,
    pub id: String,
    /// The 4-byte endpoint id from the service instance name.
    pub endpoint_id: Option<[u8; 4]>,
    pub name: Option<String>,
    pub ip: Option<String>,
    pub port: Option<String>,
    pub rtype: Option<DeviceType>,
    /// `Some(true)` when found, `Some(false)` when it went away.
    pub present: Option<bool>,
    /// Where the announcement came from: the source address of its
    /// packets. Unlike `ip`, which the announcer writes as it likes, this is
    /// what to count or rate-limit an announcer by.
    pub source: Option<IpAddr>,
}

/// A service remembered: what was reported, where it came from, and when
/// it was first seen (a count, for telling the oldest).
struct Remembered {
    info: EndpointInfo,
    source: Option<IpAddr>,
    seq: u64,
}

/// Which remembered service a new one from `source` is to replace:
/// `Ok(None)` when there is room, `Err(())` when it is refused.
///
/// A source keeps at most [`MAX_ENDPOINTS_PER_SOURCE`]: a new one replaces
/// its own oldest. When the cache holds [`MAX_DISCOVERED_ENDPOINTS`], a
/// newcomer replaces the oldest service of the source holding the most, if
/// that source would still hold more than the newcomer's; otherwise it is
/// refused. So whoever fills the cache first cannot keep a device with
/// fewer services out -- first come used to be first served, for as long
/// as it kept re-announcing -- and only as many distinct sources as there
/// are places could. mdns-sd's cache below, as patched, keeps the same
/// rule.
fn make_room(
    cache: &HashMap<String, Remembered>,
    source: Option<IpAddr>,
) -> Result<Option<String>, ()> {
    let from_source = cache.values().filter(|r| r.source == source).count();
    let evict_from: Vec<Option<IpAddr>> = if from_source >= MAX_ENDPOINTS_PER_SOURCE {
        vec![source]
    } else if cache.len() >= MAX_DISCOVERED_ENDPOINTS {
        let mut per_source: HashMap<Option<IpAddr>, usize> = HashMap::new();
        for r in cache.values() {
            let n = per_source.entry(r.source).or_default();
            *n = n.saturating_add(1);
        }
        let most = per_source.values().copied().max().unwrap_or(0);
        if most <= from_source.saturating_add(1) {
            return Err(());
        }
        // Of the sources holding the most, the one with the oldest service.
        per_source
            .into_iter()
            .filter(|(_, n)| *n == most)
            .map(|(s, _)| s)
            .collect()
    } else {
        return Ok(None);
    };
    cache
        .iter()
        .filter(|(_, r)| evict_from.contains(&r.source))
        .min_by_key(|(_, r)| r.seq)
        .map(|(name, _)| Some(name.clone()))
        .ok_or(())
}

pub struct MDnsDiscovery {
    daemon: ServiceDaemon,
    sender: broadcast::Sender<EndpointInfo>,
    filter: AddrFilter,
}

impl MDnsDiscovery {
    /// Browses on the interfaces whose address `filter` accepts, reads only
    /// packets from sources it accepts, and reports only services at
    /// addresses it accepts.
    pub fn new(
        sender: broadcast::Sender<EndpointInfo>,
        filter: AddrFilter,
    ) -> Result<Self, anyhow::Error> {
        Self::with_port(MDNS_PORT, sender, filter)
    }

    /// [`MDnsDiscovery::new`] on another UDP port than mDNS's own: for
    /// tests, so that they never browse the network they run on.
    #[doc(hidden)]
    pub fn with_port(
        mdns_port: u16,
        sender: broadcast::Sender<EndpointInfo>,
        filter: AddrFilter,
    ) -> Result<Self, anyhow::Error> {
        let intf_filter = filter.clone();
        // Made first, so that an error below drops it and stops its thread.
        let discovery = Self {
            daemon: ServiceDaemon::new_with_source_filter(mdns_port, source_filter(&filter))?,
            sender,
            filter,
        };
        discovery.daemon.disable_interface(IfKind::All)?;
        discovery
            .daemon
            .enable_interface(IfKind::Predicate(IfPredicate::new(move |intf| {
                intf.ip().is_ipv4() && intf_filter(intf.ip())
            })))?;
        Ok(discovery)
    }

    pub async fn run(self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!("MDnsDiscovery: service starting");

        let receiver = self.daemon.browse(super::SERVICE_TYPE)?;

        // By full name. Bounded, and fair to sources: see `make_room`.
        let mut cache: HashMap<String, Remembered> = HashMap::new();
        let mut seq: u64 = 0;

        loop {
            tokio::select! {
                _ = ctk.cancelled() => {
                    info!("MDnsDiscovery: tracker cancelled, breaking");
                    break;
                }
                r = receiver.recv_async() => {
                    let event = match r {
                        Ok(event) => event,
                        Err(err) => {
                            // The daemon is gone; the channel will never
                            // yield again. (Logging and looping, as before,
                            // spun a core at 100 %.)
                            error!("MDnsDiscovery: error: {}", err);
                            break;
                        }
                    };
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let fullname = info.get_fullname().to_string();
                            let source = info.get_source();
                            // S7 once more: mdns-sd reads no packet from a
                            // source the policy refuses.
                            if source.is_some_and(|src| !(self.filter)(src)) {
                                continue;
                            }
                            let known = cache.get(&fullname).map(|r| r.seq);
                            if known.is_none() && make_room(&cache, source).is_err() {
                                debug!("ServiceResolved: no room for another service");
                                continue;
                            }
                            let port = info.get_port();

                            // The first address the policy accepts. Nothing
                            // connects to it here: probing every announced
                            // address meant opening TCP connections to hosts
                            // any peer named, one at a time, each for as long
                            // as the OS connect timeout.
                            let Some(ip) = info
                                .get_addresses_v4()
                                .into_iter()
                                .find(|ip| (self.filter)(IpAddr::V4(*ip)))
                            else {
                                continue;
                            };

                            // Check that the IP is not a "self IP"
                            if !is_not_self_ip(&ip) {
                                continue;
                            }

                            // Decode the "n" text properties
                            let Some(n) = info.get_property("n") else {
                                continue;
                            };

                            // Parse the endpoint info
                            let Ok((dt, dn)) = parse_mdns_endpoint_info(n.val_str()) else {
                                continue;
                            };

                            let ei = EndpointInfo {
                                fullname: fullname.clone(),
                                id: format!("{ip}:{port}"),
                                endpoint_id: parse_mdns_name(&fullname),
                                name: Some(dn),
                                ip: Some(ip.to_string()),
                                port: Some(port.to_string()),
                                rtype: Some(dt),
                                present: Some(true),
                                source,
                            };
                            debug!("ServiceResolved: a {:?} at {:?}", ei.rtype, ei.id);
                            let first_seen = match known {
                                Some(first_seen) => first_seen,
                                None => {
                                    // Checked above, and nothing changed since.
                                    if let Ok(Some(old)) = make_room(&cache, source) {
                                        if let Some(r) = cache.remove(&old) {
                                            debug!("ServiceResolved: one replaced to make room");
                                            let _ = self.sender.send(gone(r));
                                        }
                                    }
                                    seq = seq.wrapping_add(1);
                                    seq
                                }
                            };
                            cache.insert(
                                fullname,
                                Remembered {
                                    info: ei.clone(),
                                    source,
                                    seq: first_seen,
                                },
                            );
                            let _ = self.sender.send(ei);
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            trace!("ServiceRemoved");
                            if let Some(r) = cache.remove(&fullname) {
                                debug!("ServiceRemoved: forgetting a service");
                                let _ = self.sender.send(gone(r));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        let _ = self.daemon.stop_browse(super::SERVICE_TYPE);
        shutdown_daemon(&self.daemon).await;
        Ok(())
    }
}

impl Drop for MDnsDiscovery {
    fn drop(&mut self) {
        // The daemon thread outlives its handle unless told to stop; a second
        // shutdown after run() is harmless.
        let _ = self.daemon.shutdown();
    }
}

/// What is reported for a service that went away.
fn gone(r: Remembered) -> EndpointInfo {
    EndpointInfo {
        fullname: r.info.fullname,
        id: r.info.id,
        endpoint_id: r.info.endpoint_id,
        present: Some(false),
        source: r.source,
        ..Default::default()
    }
}

/// Stops the daemon thread and waits, briefly, until it has.
pub(crate) async fn shutdown_daemon(daemon: &ServiceDaemon) {
    if let Ok(status) = daemon.shutdown() {
        let _ = tokio::time::timeout(SHUTDOWN_WAIT, status.recv_async()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(i: usize) -> Option<IpAddr> {
        Some(IpAddr::from([
            192,
            168,
            (i / 250) as u8,
            (i % 250) as u8 + 1,
        ]))
    }

    /// Adds `name` from `source` the way `run` does; false when refused.
    fn add(
        cache: &mut HashMap<String, Remembered>,
        seq: &mut u64,
        name: &str,
        source: Option<IpAddr>,
    ) -> bool {
        match make_room(cache, source) {
            Err(()) => return false,
            Ok(Some(old)) => {
                cache.remove(&old);
            }
            Ok(None) => {}
        }
        *seq += 1;
        let info = EndpointInfo {
            fullname: name.to_string(),
            ..Default::default()
        };
        cache.insert(
            name.to_string(),
            Remembered {
                info,
                source,
                seq: *seq,
            },
        );
        true
    }

    #[test]
    fn one_source_keeps_a_few_and_its_newest() {
        let (mut cache, mut seq) = (HashMap::new(), 0);
        for i in 0..MAX_ENDPOINTS_PER_SOURCE + 3 {
            assert!(add(&mut cache, &mut seq, &format!("s{i}"), src(0)));
        }
        assert_eq!(cache.len(), MAX_ENDPOINTS_PER_SOURCE);
        assert!(cache.contains_key(&format!("s{}", MAX_ENDPOINTS_PER_SOURCE + 2)));
        assert!(!cache.contains_key("s0"));
    }

    #[test]
    fn a_full_cache_takes_a_source_with_fewer_but_not_one_that_takes_a_last_place() {
        let (mut cache, mut seq) = (HashMap::new(), 0);
        let sources = MAX_DISCOVERED_ENDPOINTS / MAX_ENDPOINTS_PER_SOURCE;
        for s in 0..sources {
            for i in 0..MAX_ENDPOINTS_PER_SOURCE {
                assert!(add(&mut cache, &mut seq, &format!("s{s}i{i}"), src(s)));
            }
        }
        assert_eq!(cache.len(), MAX_DISCOVERED_ENDPOINTS);
        assert!(add(&mut cache, &mut seq, "honest", src(999)));
        assert_eq!(cache.len(), MAX_DISCOVERED_ENDPOINTS);
        // The oldest of those holding the most went; everyone still has some.
        assert!(!cache.contains_key("s0i0"));
        assert!(cache.contains_key("s0i1"));

        let (mut cache, mut seq) = (HashMap::new(), 0);
        for s in 0..MAX_DISCOVERED_ENDPOINTS {
            assert!(add(&mut cache, &mut seq, &format!("s{s}"), src(s)));
        }
        assert!(!add(&mut cache, &mut seq, "late", src(999)));
        assert_eq!(cache.len(), MAX_DISCOVERED_ENDPOINTS);
    }
}
