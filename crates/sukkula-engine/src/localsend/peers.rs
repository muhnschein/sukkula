//! The peers discovery found (F-LS1), at most [`MAX_PEERS`] of them.
//!
//! A peer is keyed by its certificate fingerprint, which it has proven by
//! completing a TLS handshake with it -- announcements alone never put a peer
//! here. When the table is full a new peer is not admitted until a stale one
//! expires: evicting on arrival would let a flood of fresh certificates push
//! out the phone the user is about to send to.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use sukkula_core::Protocol;
use sukkula_core::limits::MAX_PEERS;

use super::wire;
use crate::api::{DeviceType, Peer};

/// How long a peer stays without being heard from again. Discovery
/// re-announces well within this, and every peer that answers is refreshed.
pub(super) const PEER_TTL: Duration = Duration::from_secs(100);

/// Everything needed to send to a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    /// Where its HTTPS server is.
    pub(super) addr: SocketAddr,
    /// The certificate it must present (F-LS3).
    pub(super) fingerprint: String,
    /// Its name, after S2, for the transfer list.
    pub(super) name: String,
}

#[derive(Debug)]
struct Entry {
    target: Target,
    peer: Peer,
    seen: Instant,
}

/// The table.
#[derive(Debug, Default)]
pub(super) struct Peers {
    by_fingerprint: HashMap<String, Entry>,
}

/// A peer as a discovery source reported it, before S2.
pub(super) struct Sighting<'a> {
    pub(super) fingerprint: &'a str,
    pub(super) addr: SocketAddr,
    pub(super) alias: &'a str,
    pub(super) model: Option<&'a str>,
    pub(super) device_type: DeviceType,
}

impl Peers {
    /// Adds or refreshes a peer. Returns the peer to announce with
    /// `PeerFound` when it is new or changed; `None` when nothing the UI
    /// shows changed, or when the table is full.
    pub(super) fn upsert(&mut self, s: &Sighting<'_>, now: Instant) -> Option<Peer> {
        let peer = Peer {
            id: wire::peer_id(s.fingerprint),
            protocol: Protocol::LocalSend,
            name: wire::alias(s.alias),
            model: wire::model(s.model),
            device_type: s.device_type,
        };
        let target = Target {
            addr: s.addr,
            fingerprint: s.fingerprint.to_owned(),
            name: peer.name.clone(),
        };
        if let Some(entry) = self.by_fingerprint.get_mut(s.fingerprint) {
            entry.seen = now;
            entry.target = target;
            if entry.peer == peer {
                return None;
            }
            entry.peer = peer.clone();
            return Some(peer);
        }
        if self.by_fingerprint.len() >= MAX_PEERS {
            return None;
        }
        self.by_fingerprint.insert(
            s.fingerprint.to_owned(),
            Entry {
                target,
                peer: peer.clone(),
                seen: now,
            },
        );
        Some(peer)
    }

    /// Removes peers not seen for [`PEER_TTL`]; returns their ids.
    pub(super) fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut lost = Vec::new();
        self.by_fingerprint.retain(|_, e| {
            let fresh = now.saturating_duration_since(e.seen) < PEER_TTL;
            if !fresh {
                lost.push(e.peer.id.clone());
            }
            fresh
        });
        lost
    }

    /// Forgets everyone; returns their ids.
    pub(super) fn clear(&mut self) -> Vec<String> {
        self.by_fingerprint
            .drain()
            .map(|(_, e)| e.peer.id)
            .collect()
    }

    /// The peer the UI calls `id`.
    pub(super) fn get(&self, id: &str) -> Option<Target> {
        let fingerprint = id.strip_prefix("ls:")?;
        self.by_fingerprint
            .get(fingerprint)
            .map(|e| e.target.clone())
    }

    /// How many peers are known.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.by_fingerprint.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(i: usize) -> String {
        format!("{i:064X}")
    }

    fn sighting<'a>(fingerprint: &'a str, alias: &'a str) -> Sighting<'a> {
        Sighting {
            fingerprint,
            addr: "192.168.1.9:53317".parse().unwrap(),
            alias,
            model: Some("\u{202E}Model"),
            device_type: DeviceType::Phone,
        }
    }

    #[test]
    fn peers_are_sanitised_refreshed_and_expire() {
        let mut peers = Peers::default();
        let t0 = Instant::now();
        let f = fp(1);
        let found = peers
            .upsert(&sighting(&f, "Pekka\u{200B}\u{202E}"), t0)
            .unwrap();
        assert_eq!(found.id, format!("ls:{f}"));
        assert_eq!(found.name, "Pekka");
        assert_eq!(found.model.as_deref(), Some("Model"));
        assert!(
            peers.upsert(&sighting(&f, "Pekka"), t0).is_none(),
            "unchanged"
        );
        assert_eq!(
            peers.upsert(&sighting(&f, "Liisa"), t0).map(|p| p.name),
            Some("Liisa".to_owned())
        );
        assert_eq!(peers.get(&found.id).unwrap().fingerprint, f);
        assert!(peers.get("ls:nope").is_none());
        assert!(peers.get(&f).is_none(), "ids carry the prefix");
        assert!(peers.expire(t0 + PEER_TTL / 2).is_empty());
        assert_eq!(peers.expire(t0 + PEER_TTL), vec![found.id]);
        assert_eq!(peers.len(), 0);
    }

    #[test]
    fn the_table_is_bounded_and_keeps_who_it_has() {
        let mut peers = Peers::default();
        let t0 = Instant::now();
        let fps: Vec<String> = (0..MAX_PEERS + 10).map(fp).collect();
        for f in &fps {
            peers.upsert(&sighting(f, "x"), t0);
        }
        assert_eq!(peers.len(), MAX_PEERS);
        assert!(peers.get(&format!("ls:{}", fps[0])).is_some());
        assert!(peers.get(&format!("ls:{}", fps[MAX_PEERS])).is_none());
        assert_eq!(peers.clear().len(), MAX_PEERS);
    }
}
