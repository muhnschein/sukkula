//! Finding peers to send to: mDNS browsing through rqs_lib, and the table
//! of what was found.
//!
//! Everything a peer announces is hostile: the name goes through S2, the
//! address through the reach policy (S7, applied by mdns-sd to the sources
//! it reads, by rqs_lib to the addresses and interfaces, and again here),
//! each source is rate-limited by Quick Share's own budget, and the table
//! holds at most [`MAX_PEERS`], [`MAX_PEERS_PER_SOURCE`] from one source.
//!
//! A source is where the announcements came from -- the packets' source
//! address, as mdns-sd reports it -- not the address they name, which the
//! announcer writes as it likes: keyed on that, as this once was, one host
//! could spend another's budget, or fill the table under addresses that
//! are not its own. And the table was first come, first served: 64
//! instance names from one host kept every device that came later off the
//! list. Now a newcomer at a full table takes the place of the oldest peer
//! of the source listing the most, if that source keeps more than the
//! newcomer's then has ([`make_room`], the rule rqs_lib's cache, mdns-sd's
//! under it and LocalSend's table keep too), and a listed id is its
//! source's: the same endpoint id announced from elsewhere does not move
//! it.
//!
//! A peer's id is `qs:` and its 4-byte endpoint id: letters and digits as
//! they are, anything else in hex, so the id is always plain ASCII.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use rqs_lib::{EndpointInfo, MDnsDiscovery};
use sukkula_core::Protocol;
use sukkula_core::limits::{MAX_ALIAS_CHARS, MAX_PEERS};
use sukkula_core::offer::UNKNOWN_SENDER;
use sukkula_core::text;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{Running, Shared, ble, lock};
use crate::api::{DeviceType, ErrorCode, ErrorInfo, Event, Peer};

/// Discovery events queued between rqs_lib and this module. When the queue
/// overflows the oldest are lost, never memory.
const EVENT_QUEUE: usize = 64;

/// Peers listed from one source at once.
pub(super) const MAX_PEERS_PER_SOURCE: usize = 4;

/// A peer that can be sent to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PeerEntry {
    pub(super) peer: Peer,
    pub(super) addr: SocketAddr,
}

/// A listed peer, where it was announced from, and when it was first
/// listed (a count, for telling the oldest).
struct Listed {
    entry: PeerEntry,
    source: IpAddr,
    seq: u64,
}

/// The peers found, by id.
#[derive(Default)]
pub(super) struct Peers {
    by_id: HashMap<String, Listed>,
    seq: u64,
}

/// Starts browsing, and the BLE nudge when it is on (F-QS2).
pub(super) fn start(shared: &Arc<Shared>) -> Result<Running, ErrorInfo> {
    let token = shared.ctx.shutdown_token().child_token();
    // Peers from an earlier round may have gone; they are found again if not.
    let old: Vec<String> = lock(&shared.peers)
        .by_id
        .drain()
        .map(|(id, _)| id)
        .collect();
    for id in old {
        shared.ctx.emit(Event::PeerLost { peer: id });
    }

    let mut tasks = Vec::new();
    if shared.options.mdns {
        let (tx, rx) = broadcast::channel(EVENT_QUEUE);
        let discovery = MDnsDiscovery::with_port(shared.mdns_port, tx, shared.addr_filter())
            .map_err(|_| ErrorInfo::new(ErrorCode::Network, "cannot browse over mDNS"))?;
        let t = token.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = discovery.run(t).await {
                tracing::debug!(error = %e, "quickshare: mDNS browsing ended");
            }
        }));
        tasks.push(tokio::spawn(consume(shared.clone(), rx, token.clone())));
    }
    if shared.ctx.settings().quickshare.ble_nudge {
        match shared.system_bus() {
            Some(bus) => tasks.push(ble::spawn(bus, token.clone())),
            None => tracing::debug!("quickshare: no system bus named in test mode; no BLE nudge"),
        }
    }
    Ok(Running {
        token,
        tasks,
        port: None,
    })
}

async fn consume(
    shared: Arc<Shared>,
    mut rx: broadcast::Receiver<EndpointInfo>,
    token: CancellationToken,
) {
    loop {
        let event = tokio::select! {
            () = token.cancelled() => return,
            e = rx.recv() => e,
        };
        match event {
            Ok(info) => found_or_lost(&shared, info),
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn found_or_lost(shared: &Arc<Shared>, info: EndpointInfo) {
    // A service without an endpoint id in its name is not a Quick Share
    // device we can name; one with ours is ourselves.
    let Some(endpoint_id) = info.endpoint_id else {
        return;
    };
    if *lock(&shared.own_endpoint) == Some(endpoint_id) {
        return;
    }
    let id = peer_id(endpoint_id);
    let source = info.source.map(canonical);
    if info.present != Some(true) {
        // Only the source that listed a peer takes it off the list.
        let removed = {
            let mut peers = lock(&shared.peers);
            let theirs = peers
                .by_id
                .get(&id)
                .is_some_and(|l| Some(l.source) == source);
            theirs && peers.by_id.remove(&id).is_some()
        };
        if removed {
            shared.ctx.emit(Event::PeerLost { peer: id });
        }
        return;
    }
    let ip = info.ip.as_deref().and_then(|s| s.parse::<Ipv4Addr>().ok());
    let port = info.port.as_deref().and_then(|s| s.parse::<u16>().ok());
    let (Some(ip), Some(port), Some(source)) = (ip, port, source) else {
        return;
    };
    if port == 0 {
        return;
    }
    // S7: the policy again (mdns-sd and rqs_lib applied it too), and the
    // rate per source, so one host flooding announcements costs bounded
    // work -- counted on Quick Share's budget and the packets' source.
    if !shared.ctx.permits(source) || !lock(&shared.announcements).allow(source, Instant::now()) {
        return;
    }
    let name = info.name.unwrap_or_default();
    let device_type = info.rtype.unwrap_or(rqs_lib::DeviceType::Unknown);
    let _ = insert(
        shared,
        endpoint_id,
        SocketAddr::new(IpAddr::V4(ip), port),
        source,
        &name,
        device_type,
    );
}

/// An IPv4-mapped IPv6 address as the IPv4 address it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

/// Adds or updates a peer announced from `source` and reports it, and
/// reports lost the one it replaced, if any ([`make_room`]). `None` when
/// it is refused: outside the reach policy, no room, or its id listed from
/// another source.
pub(super) fn insert(
    shared: &Arc<Shared>,
    endpoint_id: [u8; 4],
    addr: SocketAddr,
    source: IpAddr,
    raw_name: &str,
    device_type: rqs_lib::DeviceType,
) -> Option<String> {
    if !shared.ctx.permits(addr.ip()) || !shared.ctx.permits(source) {
        return None;
    }
    let id = peer_id(endpoint_id);
    let entry = PeerEntry {
        peer: listed(endpoint_id, raw_name, device_type),
        addr,
    };
    let (changed, replaced) = {
        let mut peers = lock(&shared.peers);
        let (seq, replaced) = match peers.by_id.get(&id) {
            // An id is its source's. The same endpoint id announced from
            // elsewhere -- under another instance name -- would otherwise
            // move a phone's entry to an address the announcer chose.
            Some(held) if held.source != source => return None,
            Some(held) => (held.seq, None),
            None => {
                let replaced = make_room(&peers.by_id, source).ok()?;
                if let Some(old) = &replaced {
                    peers.by_id.remove(old);
                }
                peers.seq = peers.seq.wrapping_add(1);
                (peers.seq, replaced)
            }
        };
        let before = peers.by_id.insert(
            id.clone(),
            Listed {
                entry: entry.clone(),
                source,
                seq,
            },
        );
        (before.map(|l| l.entry).as_ref() != Some(&entry), replaced)
    };
    if let Some(old) = replaced {
        shared.ctx.emit(Event::PeerLost { peer: old });
    }
    if changed {
        shared.ctx.emit(Event::PeerFound { peer: entry.peer });
    }
    Some(id)
}

/// Which listed peer a new one from `source` is to replace: `Ok(None)`
/// when there is room, `Err(())` when it is refused. A source lists at
/// most [`MAX_PEERS_PER_SOURCE`], a new one replacing its own oldest; at
/// [`MAX_PEERS`], a newcomer replaces the oldest peer of the sources
/// listing the most, if they list at least two more than the newcomer's --
/// so pushing anyone out takes as many sources as there are places.
fn make_room(listed: &HashMap<String, Listed>, source: IpAddr) -> Result<Option<String>, ()> {
    let own = listed.values().filter(|l| l.source == source).count();
    let from: Vec<IpAddr> = if own >= MAX_PEERS_PER_SOURCE {
        vec![source]
    } else if listed.len() >= MAX_PEERS {
        let mut counts: HashMap<IpAddr, usize> = HashMap::new();
        for l in listed.values() {
            let n = counts.entry(l.source).or_insert(0);
            *n = n.saturating_add(1);
        }
        let most = counts.values().copied().max().unwrap_or(0);
        if most < own.saturating_add(2) {
            return Err(());
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n == most)
            .map(|(s, _)| s)
            .collect()
    } else {
        return Ok(None);
    };
    listed
        .iter()
        .filter(|(_, l)| from.contains(&l.source))
        .min_by_key(|(_, l)| l.seq)
        .map(|(id, _)| Some(id.clone()))
        .ok_or(())
}

/// The peer the UI lists for an announcement: the id, the name after S2
/// (never empty), the icon.
pub(super) fn listed(
    endpoint_id: [u8; 4],
    raw_name: &str,
    device_type: rqs_lib::DeviceType,
) -> Peer {
    let name = text::display(raw_name, MAX_ALIAS_CHARS);
    Peer {
        id: peer_id(endpoint_id),
        protocol: Protocol::QuickShare,
        name: if name.is_empty() {
            UNKNOWN_SENDER.to_owned()
        } else {
            name
        },
        model: None,
        device_type: match device_type {
            rqs_lib::DeviceType::Phone => DeviceType::Phone,
            rqs_lib::DeviceType::Tablet => DeviceType::Tablet,
            rqs_lib::DeviceType::Laptop => DeviceType::Computer,
            rqs_lib::DeviceType::Unknown => DeviceType::Unknown,
        },
    }
}

/// The peer with id `id`, if it is known.
pub(super) fn lookup(shared: &Shared, id: &str) -> Option<PeerEntry> {
    lock(&shared.peers).by_id.get(id).map(|l| l.entry.clone())
}

/// `qs:` and the endpoint id: as is when it is letters and digits (as
/// Quick Share makes them), in hex otherwise.
fn peer_id(endpoint_id: [u8; 4]) -> String {
    if endpoint_id.iter().all(u8::is_ascii_alphanumeric) {
        let s: String = endpoint_id.iter().map(|b| char::from(*b)).collect();
        format!("qs:{s}")
    } else {
        format!("qs:{}", sukkula_core::hex::encode(&endpoint_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_ids_are_plain_ascii() {
        assert_eq!(peer_id(*b"Ab3z"), "qs:Ab3z");
        assert_eq!(peer_id([0, b'/', 0xff, b'\n']), "qs:002fff0a");
        assert_eq!(peer_id(*b"a b."), "qs:6120622e");
    }
}
