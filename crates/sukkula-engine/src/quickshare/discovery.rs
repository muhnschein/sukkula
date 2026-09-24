//! Finding peers to send to: mDNS browsing through rqs_lib, and the table
//! of what was found.
//!
//! Everything a peer announces is hostile: the name goes through S2, the
//! address through the reach policy (S7, applied by rqs_lib to the
//! addresses and interfaces, and again here), each source is rate-limited
//! ([`Ctx::allow_discovery`]), and the table holds at most [`MAX_PEERS`].
//! A peer's id is `qs:` and its 4-byte endpoint id: letters and digits as
//! they are, anything else in hex, so the id is always plain ASCII.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

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

/// A peer that can be sent to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PeerEntry {
    pub(super) peer: Peer,
    pub(super) addr: SocketAddr,
}

/// The peers found, by id.
#[derive(Default)]
pub(super) struct Peers {
    by_id: HashMap<String, PeerEntry>,
}

/// Starts browsing, and the BLE nudge when it is on (F-QS2).
pub(super) fn start(shared: &Arc<Shared>) -> Result<Running, ErrorInfo> {
    let token = shared.ctx.shutdown_token().child_token();
    // Peers from an earlier round may have gone; they are found again if not.
    let old: Vec<String> = lock(&shared.peers).by_id.drain().map(|(id, _)| id).collect();
    for id in old {
        shared.ctx.emit(Event::PeerLost { peer: id });
    }

    let mut tasks = Vec::new();
    if shared.options.mdns {
        let (tx, rx) = broadcast::channel(EVENT_QUEUE);
        let discovery = MDnsDiscovery::new(tx, shared.addr_filter())
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
        tasks.push(ble::spawn(shared.options.system_bus.clone(), token.clone()));
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
    if info.present != Some(true) {
        let removed = lock(&shared.peers).by_id.remove(&id).is_some();
        if removed {
            shared.ctx.emit(Event::PeerLost { peer: id });
        }
        return;
    }
    let ip = info.ip.as_deref().and_then(|s| s.parse::<Ipv4Addr>().ok());
    let port = info.port.as_deref().and_then(|s| s.parse::<u16>().ok());
    let (Some(ip), Some(port)) = (ip, port) else {
        return;
    };
    if port == 0 {
        return;
    }
    let ip = IpAddr::V4(ip);
    // S7: the policy again (rqs_lib applied it too), and the rate per
    // source, so one host flooding announcements costs bounded work.
    if !shared.ctx.allow_discovery(ip) {
        return;
    }
    let name = info.name.unwrap_or_default();
    let device_type = info.rtype.unwrap_or(rqs_lib::DeviceType::Unknown);
    let _ = insert(shared, endpoint_id, SocketAddr::new(ip, port), &name, device_type);
}

/// Adds or updates a peer and reports it. `None` when the table is full.
pub(super) fn insert(
    shared: &Arc<Shared>,
    endpoint_id: [u8; 4],
    addr: SocketAddr,
    raw_name: &str,
    device_type: rqs_lib::DeviceType,
) -> Option<String> {
    if !shared.ctx.permits(addr.ip()) {
        return None;
    }
    let id = peer_id(endpoint_id);
    let name = text::display(raw_name, MAX_ALIAS_CHARS);
    let entry = PeerEntry {
        peer: Peer {
            id: id.clone(),
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
        },
        addr,
    };
    let changed = {
        let mut peers = lock(&shared.peers);
        if !peers.by_id.contains_key(&id) && peers.by_id.len() >= MAX_PEERS {
            return None;
        }
        peers.by_id.insert(id.clone(), entry.clone()).as_ref() != Some(&entry)
    };
    if changed {
        shared.ctx.emit(Event::PeerFound { peer: entry.peer });
    }
    Some(id)
}

/// The peer with id `id`, if it is known.
pub(super) fn lookup(shared: &Shared, id: &str) -> Option<PeerEntry> {
    lock(&shared.peers).by_id.get(id).cloned()
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
