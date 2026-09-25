//! The peers discovery found (F-LS1), at most [`MAX_PEERS`] of them, and the
//! leads its HTTP fallback registers with.
//!
//! A peer is keyed by its certificate fingerprint, which it has proven by
//! completing a TLS handshake with it -- announcements alone never put a peer
//! here. The addresses a table holds are the peers' own: we connected to
//! them, or they connected to us. So one address may hold at most
//! [`MAX_PEERS_PER_IP`] entries, and when the table is full a newcomer
//! replaces the stalest entry of the address holding the most, if that
//! address holds at least two more than the newcomer's ([`make_room`]).
//! One address -- one attacker minting certificates -- can then never push
//! out a peer at another address, and pushing out a peer at all takes
//! [`MAX_PEERS`] addresses. Plain first-come, as this once was, let one
//! address fill the table with certificates of its own and keep every
//! device that came later off the list.
//!
//! [`Leads`] are servers to register with in the fallback's rounds: peers
//! found before, peers that registered with us, announcements that could
//! not be answered at once. Leads a handshake proved rank above leads that
//! only an announcement named: those are unauthenticated datagrams, and a
//! flood of them must not push out a peer that proved itself.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use sukkula_core::Protocol;
use sukkula_core::limits::MAX_PEERS;

use super::wire;
use crate::api::{DeviceType, Peer};

/// How long a peer stays without being heard from again. Discovery
/// re-announces well within this, and every peer that answers is refreshed.
pub(super) const PEER_TTL: Duration = Duration::from_secs(100);

/// Peers listed from one address at once. More than one, for a computer
/// running LocalSend twice; few, so one address cannot fill the list.
pub(super) const MAX_PEERS_PER_IP: usize = 4;

/// Leads kept at once.
pub(super) const MAX_LEADS: usize = 32;

/// Leads kept for one address at once.
pub(super) const MAX_LEADS_PER_IP: usize = 4;

/// Rounds a lead is tried without an answer before it is dropped.
pub(super) const MAX_LEAD_TRIES: u8 = 3;

/// How long a lead is kept, answered or not: the phone moves between
/// networks, and an address on the last one means nothing on this one.
pub(super) const LEAD_TTL: Duration = Duration::from_secs(600);

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

/// What [`Peers::upsert`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Upsert {
    /// The peer is in the table now.
    pub(super) listed: bool,
    /// The peer to announce with `PeerFound`: new, or changed in what the
    /// UI shows.
    pub(super) found: Option<Peer>,
    /// The id of the peer it replaced, to announce with `PeerLost`.
    pub(super) lost: Option<String>,
}

impl Peers {
    /// Adds or refreshes a peer.
    pub(super) fn upsert(&mut self, s: &Sighting<'_>, now: Instant) -> Upsert {
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
            // A peer that moved keeps its entry: the certificate is the
            // identity, and only its holder can prove it from anywhere.
            entry.seen = now;
            entry.target = target;
            let found = (entry.peer != peer).then(|| peer.clone());
            entry.peer = peer;
            return Upsert {
                listed: true,
                found,
                lost: None,
            };
        }
        let victim = {
            let slots: Vec<Slot<'_, String>> = self
                .by_fingerprint
                .iter()
                .map(|(key, e)| Slot {
                    key,
                    ip: e.target.addr.ip(),
                    rank: 0,
                    seen: e.seen,
                })
                .collect();
            match make_room(&slots, s.addr.ip(), 0, MAX_PEERS, MAX_PEERS_PER_IP) {
                Room::Free => None,
                Room::Replace(key) => Some(key.clone()),
                Room::Full => return Upsert::default(),
            }
        };
        let lost = victim
            .and_then(|key| self.by_fingerprint.remove(&key))
            .map(|e| e.peer.id);
        self.by_fingerprint.insert(
            s.fingerprint.to_owned(),
            Entry {
                target,
                peer: peer.clone(),
                seen: now,
            },
        );
        Upsert {
            listed: true,
            found: Some(peer),
            lost,
        }
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

    /// Forgets everyone; returns their ids and where they were.
    pub(super) fn clear(&mut self) -> Vec<(String, Target)> {
        self.by_fingerprint
            .drain()
            .map(|(_, e)| (e.peer.id, e.target))
            .collect()
    }

    /// Peers not heard from for `age`: where they are and the certificate
    /// to pin, for the fallback to register with again.
    pub(super) fn quiet(&self, now: Instant, age: Duration) -> Vec<(SocketAddr, String)> {
        self.by_fingerprint
            .values()
            .filter(|e| now.saturating_duration_since(e.seen) >= age)
            .map(|e| (e.target.addr, e.target.fingerprint.clone()))
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

/// A LocalSend server the fallback registers with.
#[derive(Debug)]
struct Lead {
    /// A handshake proved the fingerprint: a peer found before, or one that
    /// registered with us. Otherwise only an announcement named it.
    proven: bool,
    /// Rounds it was tried in without an answer.
    tries: u8,
    /// When it was first kept; it goes [`LEAD_TTL`] after.
    since: Instant,
}

/// The leads, keyed by address and the fingerprint to pin there.
#[derive(Debug, Default)]
pub(super) struct Leads {
    by_key: HashMap<(SocketAddr, String), Lead>,
}

impl Leads {
    /// Keeps `fingerprint` at `addr` to register with. False when there is
    /// no room for it ([`make_room`]).
    pub(super) fn add(
        &mut self,
        addr: SocketAddr,
        fingerprint: &str,
        proven: bool,
        now: Instant,
    ) -> bool {
        let key = (addr, fingerprint.to_owned());
        if let Some(lead) = self.by_key.get_mut(&key) {
            // Not a fresh start: a lead named again keeps its age and its
            // failures, so repeating it cannot keep it alive.
            lead.proven |= proven;
            return true;
        }
        let victim = {
            let slots: Vec<Slot<'_, (SocketAddr, String)>> = self
                .by_key
                .iter()
                .map(|(key, lead)| Slot {
                    key,
                    ip: key.0.ip(),
                    rank: u8::from(lead.proven),
                    seen: lead.since,
                })
                .collect();
            let rank = u8::from(proven);
            match make_room(&slots, addr.ip(), rank, MAX_LEADS, MAX_LEADS_PER_IP) {
                Room::Free => None,
                Room::Replace(key) => Some(key.clone()),
                Room::Full => return false,
            }
        };
        if let Some(victim) = victim {
            self.by_key.remove(&victim);
        }
        self.by_key.insert(
            key,
            Lead {
                proven,
                tries: 0,
                since: now,
            },
        );
        true
    }

    /// Every lead for this round, after dropping those past [`LEAD_TTL`].
    pub(super) fn due(&mut self, now: Instant) -> Vec<(SocketAddr, String)> {
        self.by_key
            .retain(|_, lead| now.saturating_duration_since(lead.since) < LEAD_TTL);
        self.by_key.keys().cloned().collect()
    }

    /// The server at `addr` proved `fingerprint`, or showed another: either
    /// way there is nothing left to try there under that fingerprint.
    pub(super) fn settle(&mut self, addr: SocketAddr, fingerprint: &str) {
        self.by_key.remove(&(addr, fingerprint.to_owned()));
    }

    /// No answer this round; dropped after [`MAX_LEAD_TRIES`].
    pub(super) fn missed(&mut self, addr: SocketAddr, fingerprint: &str) {
        let key = (addr, fingerprint.to_owned());
        if let Some(lead) = self.by_key.get_mut(&key) {
            lead.tries = lead.tries.saturating_add(1);
            if lead.tries >= MAX_LEAD_TRIES {
                self.by_key.remove(&key);
            }
        }
    }

    /// How many leads are kept.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether this lead is kept.
    #[cfg(test)]
    pub(super) fn has(&self, addr: SocketAddr, fingerprint: &str) -> bool {
        self.by_key.contains_key(&(addr, fingerprint.to_owned()))
    }
}

/// One entry of a bounded table, as [`make_room`] weighs it.
struct Slot<'a, K> {
    key: &'a K,
    /// The address the entry is for: its own, never one it claimed.
    ip: IpAddr,
    /// How well it is proven; a newcomer never replaces a better one.
    rank: u8,
    /// Its age: the stalest goes first.
    seen: Instant,
}

/// Where a newcomer goes.
#[derive(Debug, PartialEq, Eq)]
enum Room<'a, K> {
    /// There is room.
    Free,
    /// It replaces this entry.
    Replace(&'a K),
    /// It is not admitted.
    Full,
}

/// Room for a newcomer from `ip` of `rank`, in a table of at most `cap`
/// entries and at most `per_ip` for one address.
///
/// - An address at its share replaces its own stalest entry -- never
///   someone else's, and never a better-proven one.
/// - A full table gives up its weakest, stalest entry of the address that
///   holds the most, when that entry ranks below the newcomer, or ranks the
///   same and its address holds at least two more entries than the
///   newcomer's. Otherwise the newcomer is refused: among addresses that
///   share the table evenly, first come stays, so a flood of fresh
///   certificates cannot push out the phone the user is about to send to.
fn make_room<'a, K>(
    slots: &[Slot<'a, K>],
    ip: IpAddr,
    rank: u8,
    cap: usize,
    per_ip: usize,
) -> Room<'a, K> {
    let own = slots.iter().filter(|s| s.ip == ip);
    let held = own.clone().count();
    if held >= per_ip {
        return own
            .filter(|s| s.rank <= rank)
            .min_by_key(|s| s.seen)
            .map_or(Room::Full, |s| Room::Replace(s.key));
    }
    if slots.len() < cap {
        return Room::Free;
    }
    let mut counts: HashMap<IpAddr, usize> = HashMap::new();
    for s in slots {
        let n = counts.entry(s.ip).or_insert(0);
        *n = n.saturating_add(1);
    }
    let count = |ip: &IpAddr| counts.get(ip).copied().unwrap_or(0);
    slots
        .iter()
        .filter(|s| s.rank < rank || (s.rank == rank && count(&s.ip) >= held.saturating_add(2)))
        .min_by(|a, b| {
            a.rank
                .cmp(&b.rank)
                .then_with(|| count(&b.ip).cmp(&count(&a.ip)))
                .then_with(|| a.seen.cmp(&b.seen))
        })
        .map_or(Room::Full, |s| Room::Replace(s.key))
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // Test scenes count and add times.
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn fp(i: usize) -> String {
        format!("{i:064X}")
    }

    fn at(host: u8) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, host)), 53317)
    }

    fn sighting<'a>(fingerprint: &'a str, alias: &'a str) -> Sighting<'a> {
        from(fingerprint, alias, at(9))
    }

    fn from<'a>(fingerprint: &'a str, alias: &'a str, addr: SocketAddr) -> Sighting<'a> {
        Sighting {
            fingerprint,
            addr,
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
            .found
            .unwrap();
        assert_eq!(found.id, format!("ls:{f}"));
        assert_eq!(found.name, "Pekka");
        assert_eq!(found.model.as_deref(), Some("Model"));
        let again = peers.upsert(&sighting(&f, "Pekka"), t0);
        assert!(again.listed && again.found.is_none(), "unchanged");
        assert_eq!(
            peers
                .upsert(&sighting(&f, "Liisa"), t0)
                .found
                .map(|p| p.name),
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
    fn one_address_cannot_fill_the_table() {
        let mut peers = Peers::default();
        let t0 = Instant::now();
        let fps: Vec<String> = (0..MAX_PEERS + 10).map(fp).collect();
        // One address minting certificates keeps only its share, replacing
        // its own stalest each time.
        for (i, f) in fps.iter().enumerate() {
            let s = Duration::from_secs(u64::try_from(i).unwrap());
            let u = peers.upsert(&from(f, "x", at(66)), t0 + s);
            assert!(u.listed);
            if i >= MAX_PEERS_PER_IP {
                assert_eq!(u.lost, Some(format!("ls:{}", fps[i - MAX_PEERS_PER_IP])));
            }
        }
        assert_eq!(peers.len(), MAX_PEERS_PER_IP);
        // Everyone else still gets in.
        let honest = fp(1000);
        assert!(peers.upsert(&from(&honest, "Pekka", at(10)), t0).listed);
        assert_eq!(peers.clear().len(), MAX_PEERS_PER_IP + 1);
    }

    #[test]
    fn a_full_table_makes_room_from_the_address_holding_most() {
        let mut peers = Peers::default();
        let t0 = Instant::now();
        let mut n: usize = 0;
        // Sixteen addresses with four peers each: the table is full.
        for host in 0..16u8 {
            for _ in 0..MAX_PEERS_PER_IP {
                let s = Duration::from_millis(u64::try_from(n).unwrap());
                assert!(
                    peers
                        .upsert(&from(&fp(n), "x", at(100 + host)), t0 + s)
                        .listed
                );
                n += 1;
            }
        }
        assert_eq!(peers.len(), MAX_PEERS);
        // A newcomer from a fresh address replaces the stalest peer of the
        // addresses that hold most: the first one listed.
        let later = t0 + Duration::from_secs(60);
        let honest = fp(5000);
        let u = peers.upsert(&from(&honest, "Pekka", at(1)), later);
        assert!(u.listed);
        assert_eq!(u.lost, Some(format!("ls:{}", fp(0))));
        assert_eq!(peers.len(), MAX_PEERS);
        // Sixteen more newcomers each take a place from the addresses that
        // hold the most; the address holding one place keeps it.
        for i in 0..16usize {
            let u = peers.upsert(
                &from(&fp(6000 + i), "y", at(2 + u8::try_from(i).unwrap())),
                later,
            );
            assert!(u.listed, "{i}");
        }
        assert!(peers.get(&format!("ls:{honest}")).is_some());
    }

    #[test]
    fn among_equals_first_come_stays() {
        let mut peers = Peers::default();
        let t0 = Instant::now();
        // As many addresses as places, one peer each.
        for i in 0..MAX_PEERS {
            let host = u8::try_from(i).unwrap();
            assert!(peers.upsert(&from(&fp(i), "x", at(host)), t0).listed);
        }
        let u = peers.upsert(&from(&fp(999), "late", at(200)), t0);
        assert_eq!(u, Upsert::default(), "no one is pushed out");
        assert!(peers.get(&format!("ls:{}", fp(0))).is_some());
    }

    #[test]
    fn a_moved_peer_keeps_its_entry() {
        let mut peers = Peers::default();
        let t0 = Instant::now();
        let f = fp(7);
        peers.upsert(&from(&f, "x", at(1)), t0);
        let u = peers.upsert(&from(&f, "x", at(2)), t0);
        assert!(u.listed && u.found.is_none() && u.lost.is_none());
        assert_eq!(peers.get(&format!("ls:{f}")).unwrap().addr, at(2));
        assert_eq!(peers.quiet(t0 + PEER_TTL, PEER_TTL), vec![(at(2), f)]);
        assert!(peers.quiet(t0, PEER_TTL).is_empty());
    }

    #[test]
    fn announced_leads_never_push_out_proven_ones() {
        let mut leads = Leads::default();
        let t0 = Instant::now();
        // Proven leads, spread over addresses, fill every place.
        for i in 0..MAX_LEADS {
            let host = u8::try_from(i / 2).unwrap();
            assert!(leads.add(at(host), &fp(i), true, t0));
        }
        // A flood of announcements naming any address gets nowhere.
        for i in 0..1000 {
            let host = u8::try_from(i % 250).unwrap();
            assert!(!leads.add(at(host), &fp(10_000 + i), false, t0));
        }
        assert_eq!(leads.len(), MAX_LEADS);
        assert!(leads.has(at(0), &fp(0)));

        // Announced leads fill what proven ones leave, and give way to them.
        let mut leads = Leads::default();
        for i in 0..MAX_LEADS {
            let host = u8::try_from(i).unwrap();
            assert!(leads.add(at(host), &fp(i), false, t0));
        }
        assert!(leads.add(at(200), &fp(99), true, t0 + Duration::from_secs(1)));
        assert!(leads.has(at(200), &fp(99)));
        assert_eq!(leads.len(), MAX_LEADS);
    }

    #[test]
    fn one_address_gets_its_share_of_leads() {
        let mut leads = Leads::default();
        let t0 = Instant::now();
        for i in 0..100 {
            let s = Duration::from_millis(u64::try_from(i).unwrap());
            assert!(leads.add(at(66), &fp(i), false, t0 + s));
        }
        assert_eq!(leads.len(), MAX_LEADS_PER_IP);
        // A proven lead from that address takes a place from its announced
        // ones; announced ones then cannot take it back.
        assert!(leads.add(at(66), &fp(500), true, t0 + Duration::from_secs(1)));
        for i in 0..MAX_LEADS_PER_IP {
            leads.add(at(66), &fp(600 + i), false, t0 + Duration::from_secs(2));
        }
        assert!(leads.has(at(66), &fp(500)));
        assert!(
            leads.add(at(67), &fp(1), false, t0),
            "another address has room"
        );
    }

    #[test]
    fn leads_are_tried_a_few_times_and_expire() {
        let mut leads = Leads::default();
        let t0 = Instant::now();
        let f = fp(1);
        assert!(leads.add(at(1), &f, false, t0));
        for _ in 1..MAX_LEAD_TRIES {
            leads.missed(at(1), &f);
            // Named again, it keeps its failures.
            leads.add(at(1), &f, false, t0);
            assert!(leads.has(at(1), &f));
        }
        leads.missed(at(1), &f);
        assert!(
            !leads.has(at(1), &f),
            "dropped after {MAX_LEAD_TRIES} misses"
        );

        assert!(leads.add(at(2), &f, true, t0));
        assert_eq!(leads.due(t0 + LEAD_TTL / 2), vec![(at(2), f.clone())]);
        assert!(leads.due(t0 + LEAD_TTL).is_empty());

        assert!(leads.add(at(3), &f, false, t0));
        leads.settle(at(3), &f);
        assert_eq!(leads.len(), 0);
    }
}
