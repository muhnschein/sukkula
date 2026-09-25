//! A cache for DNS records.
//!
//! This is an internal implementation, not visible to the public API.

#[cfg(feature = "logging")]
use crate::log::{debug, trace};
use crate::{
    current_time_millis,
    dns_parser::{DnsAddress, DnsPointer, DnsRecordBox, DnsSrv, InterfaceId, RRType},
    service_info::{split_sub_domain, MyIntf},
    ScopedIp,
};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    ops::BitOr,
};

// What the cache holds is bounded. mDNS is open to anyone on the link and a
// response's records are the sender's to choose, so without these a
// neighbour grows the cache, and the daemon's timers with it, for as long as
// it keeps sending. With them, the records that one browsed type can reach --
// its instances, their SRV, TXT and NSEC records, the addresses and NSEC
// records of their hosts -- come to at most
// 64 * (1 + 2 + 2 + 2 + 2 * (8 + 2)) = 1728, under the total.

/// Most instances (PTR records) cached per browsed service type.
pub(crate) const MAX_INSTANCES_PER_TYPE: usize = 64;

/// Most instances of one service type cached from one source address. A new
/// one from a source at this many replaces the source's own oldest.
pub(crate) const MAX_INSTANCES_PER_SOURCE: usize = 4;

/// Most SRV, TXT or NSEC records cached per name.
const MAX_RECORDS_PER_NAME: usize = 2;

/// Most address (A and AAAA) records cached per host name.
const MAX_ADDRS_PER_HOST: usize = 8;

/// Most records cached in all.
pub(crate) const MAX_CACHED_RECORDS: usize = 2048;

/// Bitflags-style type for filtering by IP version.
#[derive(Clone, Copy)]
pub(crate) struct IpType(u8);

impl IpType {
    pub const V4: Self = Self(0b01);
    pub const V6: Self = Self(0b10);
    pub const BOTH: Self = Self(0b11);

    fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for IpType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// The result of removing records on a specific interface.
pub(crate) struct IntfRemovalResult {
    /// Map of ty_domain -> set of fully removed instance names (PTR gone).
    pub(crate) removed_instances: HashMap<String, HashSet<String>>,
    /// Set of instance names that lost records but still have PTR entries.
    pub(crate) modified_instances: HashSet<String>,
}

/// Associate a DnsRecord with the interface it was received on.
pub(crate) struct DnsRecordIntf {
    pub(crate) record: DnsRecordBox,
    pub(crate) src_intf: InterfaceId,
    /// The source address of the packet that first brought the record.
    pub(crate) src_addr: IpAddr,
}

/// A cache for all types of DNS records.
pub(crate) struct DnsCache {
    /// DnsPointer records indexed by ty_domain
    ptr: HashMap<String, Vec<DnsRecordIntf>>,

    /// DnsSrv records indexed by the fullname of an instance
    srv: HashMap<String, Vec<DnsRecordIntf>>,

    /// DnsTxt records indexed by the fullname of an instance
    txt: HashMap<String, Vec<DnsRecordIntf>>,

    /// DnsAddr records indexed by the hostname in lowercase.
    addr: HashMap<String, Vec<DnsRecordIntf>>,

    /// A reverse lookup table from "instance fullname" to "subtype PTR name"
    subtype: HashMap<String, String>,

    /// Negative responses:
    /// A map from "instance fullname" to DnsNSec.
    nsec: HashMap<String, Vec<DnsRecordIntf>>,
}

impl DnsCache {
    pub(crate) fn new() -> Self {
        Self {
            ptr: HashMap::new(),
            srv: HashMap::new(),
            txt: HashMap::new(),
            addr: HashMap::new(),
            subtype: HashMap::new(),
            nsec: HashMap::new(),
        }
    }

    pub(crate) fn all_ptr(&self) -> &HashMap<String, Vec<DnsRecordIntf>> {
        &self.ptr
    }

    /// Count all PTR records in the cache.
    pub(crate) fn ptr_count(&self) -> usize {
        self.ptr.values().map(|v| v.len()).sum()
    }

    pub(crate) fn srv_count(&self) -> usize {
        self.srv.values().map(|v| v.len()).sum()
    }

    pub(crate) fn txt_count(&self) -> usize {
        self.txt.values().map(|v| v.len()).sum()
    }

    pub(crate) fn addr_count(&self) -> usize {
        self.addr.values().map(|v| v.len()).sum()
    }

    pub(crate) fn nsec_count(&self) -> usize {
        self.nsec.values().map(|v| v.len()).sum()
    }

    pub(crate) fn subtype_count(&self) -> usize {
        self.subtype.len()
    }

    /// Count all records in the cache.
    pub(crate) fn total_records(&self) -> usize {
        self.ptr_count()
            + self.srv_count()
            + self.txt_count()
            + self.addr_count()
            + self.nsec_count()
    }

    /// The earliest time after `now` at which a cached record expires or
    /// falls due for refresh (RFC 6762 §5.2). The run loop sleeps no longer
    /// than this, in place of the two timers each record used to add on
    /// arrival and on every refresh: those were a heap that a neighbour
    /// re-sending the same records grew by 16 bytes a record a packet. A
    /// deadline already passed is not one: what it was for is done on the
    /// pass that finds it.
    pub(crate) fn next_deadline(&self, now: u64) -> Option<u64> {
        [&self.ptr, &self.srv, &self.txt, &self.addr, &self.nsec]
            .into_iter()
            .flat_map(|map| map.values().flatten())
            .flat_map(|r| {
                let record = r.record.get_record();
                [record.get_expire_time(), record.get_refresh_time()]
            })
            .filter(|&t| t > now)
            .min()
    }

    /// The instance names that PTR records point to.
    pub(crate) fn instance_names(&self) -> HashSet<String> {
        self.ptr
            .values()
            .flatten()
            .filter_map(|r| r.record.any().downcast_ref::<DnsPointer>())
            .map(|ptr| ptr.alias().to_string())
            .collect()
    }

    /// The host names, in lowercase, of the SRV records of `instances`.
    pub(crate) fn host_names(&self, instances: &HashSet<String>) -> HashSet<String> {
        instances
            .iter()
            .filter_map(|instance| self.srv.get(instance))
            .flatten()
            .filter_map(|r| r.record.any().downcast_ref::<DnsSrv>())
            .map(|srv| srv.host().to_lowercase())
            .collect()
    }

    pub(crate) fn get_ptr(&self, ty_domain: &str) -> Option<&Vec<DnsRecordIntf>> {
        self.ptr.get(ty_domain)
    }

    pub(crate) fn get_srv(&self, fullname: &str) -> Option<&Vec<DnsRecordIntf>> {
        self.srv.get(fullname)
    }

    pub(crate) fn get_txt(&self, fullname: &str) -> Option<&Vec<DnsRecordIntf>> {
        self.txt.get(fullname)
    }

    pub(crate) fn get_addr(&self, hostname: &str) -> Option<&Vec<DnsRecordIntf>> {
        self.addr.get(&hostname.to_lowercase())
    }

    /// A reverse lookup table from "instance fullname" to "subtype PTR name"
    pub(crate) fn get_subtype(&self, fullname: &str) -> Option<&String> {
        self.subtype.get(fullname)
    }

    /// Returns the list of instances that has `host` as its hostname.
    pub(crate) fn get_instances_on_host(&self, host: &str) -> Vec<String> {
        self.srv
            .iter()
            .filter_map(|(instance, srv_list)| {
                if let Some(item) = srv_list.first() {
                    if let Some(dns_srv) = item.record.any().downcast_ref::<DnsSrv>() {
                        if dns_srv.host() == host {
                            return Some(instance.clone());
                        }
                    }
                }
                None
            })
            .collect()
    }

    /// Returns a hashmap of hostnames and their addresses for a given `host`.
    ///
    /// Note that the keys in the returned HashMap are the same hostname, with different cases
    /// of letters (e.g. "example.local.", "Example.local.", "EXAMPLE.local.").
    pub(crate) fn get_addresses_for_host(&self, host: &str) -> HashMap<String, HashSet<ScopedIp>> {
        let hostname_lower = host.to_lowercase();
        let mut result = HashMap::new();

        if let Some(records) = self.addr.get(&hostname_lower) {
            for record in records {
                if let Some(dns_addr) = record.record.any().downcast_ref::<DnsAddress>() {
                    let record_name = record.record.get_name().to_string();
                    let address = dns_addr.address();

                    // Use the entry API to insert or update the HashSet for the record_name
                    result
                        .entry(record_name)
                        .or_insert_with(HashSet::new)
                        .insert(address);
                }
            }
        }

        result
    }

    /// Returns a list of resource records (name, rr_type) that need to be queried in order to
    /// verify the `instance`.
    ///
    /// If `expire_at` is not None, the resource records' expire time will be updated.
    pub(crate) fn service_verify_queries(
        &mut self,
        instance: &str,
        expire_at: Option<u64>,
    ) -> Vec<(String, RRType)> {
        let Some(srv_vec) = self.srv.get_mut(instance) else {
            return Vec::new();
        };

        let mut query_vec = vec![(instance.to_string(), RRType::SRV)];

        for srv in srv_vec {
            if let Some(new_expire) = expire_at {
                srv.record.set_expire_sooner(new_expire);
            }

            let Some(srv_record) = srv.record.any().downcast_ref::<DnsSrv>() else {
                continue;
            };

            // Will verify addresses for the hostname.
            query_vec.push((srv_record.host().to_string(), RRType::A));
            query_vec.push((srv_record.host().to_string(), RRType::AAAA));

            if let Some(new_expire) = expire_at {
                if let Some(addrs) = self.addr.get_mut(srv_record.host()) {
                    for addr in addrs {
                        addr.record.set_expire_sooner(new_expire);
                    }
                }
            }
        }

        query_vec
    }

    /// Update a DNSRecord TTL if already exists, otherwise insert a new record.
    ///
    /// Returns `None` if `incoming` is invalid / unrecognized, or refused,
    /// otherwise returns (a new record, true) or (existing record with TTL
    /// updated, false).
    ///
    /// A new record is taken only if `is_for_us`, if its name holds fewer
    /// than its cap of records, and if `room` -- what is left of
    /// [`MAX_CACHED_RECORDS`] -- is not used up; a new PTR record must also
    /// have passed [`DnsCache::make_room_for_ptr`]. `src` is the source
    /// address of the packet it came in.
    ///
    /// If you need to add new timers for related records, push into `timers`.
    pub(crate) fn add_or_update(
        &mut self,
        intf: &MyIntf,
        incoming: DnsRecordBox,
        timers: &mut Vec<u64>,
        is_for_us: bool,
        src: IpAddr,
        room: &mut usize,
    ) -> Option<(&DnsRecordIntf, bool)> {
        let entry_name = incoming.get_name().to_string();

        // Look up the existing records without creating an entry yet.
        let entry_name_lower = entry_name.to_lowercase();
        let existing = match incoming.get_type() {
            RRType::PTR => self.ptr.get(&entry_name),
            RRType::SRV => self.srv.get(&entry_name),
            RRType::TXT => self.txt.get(&entry_name),
            RRType::A | RRType::AAAA => self.addr.get(&entry_name_lower),
            RRType::NSEC => self.nsec.get(&entry_name),
            _ => return None,
        };
        let known = existing.map_or(false, |records| {
            records.iter().any(|r| r.record.matches(incoming.as_ref()))
        });

        if !known {
            // `is_for_us` is decided per record by the daemon: a new record
            // is cached only if it answers something asked. A record already
            // cached is always refreshed, which keeps the cache coherent and
            // does not grow it.
            if !is_for_us {
                trace!(
                    "add_or_update: dropping new record not for us: {} (type {:?})",
                    incoming.get_name(),
                    incoming.get_type()
                );
                return None;
            }
            let cap = match incoming.get_type() {
                RRType::PTR => MAX_INSTANCES_PER_TYPE,
                RRType::A | RRType::AAAA => MAX_ADDRS_PER_HOST,
                _ => MAX_RECORDS_PER_NAME,
            };
            let held = existing.map_or(0, Vec::len);
            // A new unique record (the cache-flush bit set) says it and its
            // companions are the whole set of the name (RFC 6762 §10.2): at
            // the cap, it takes the place of the oldest, which the flush
            // below would have let go within a second anyway. PTR records
            // are shared; their room is `make_room_for_ptr`'s alone.
            let replaces =
                held >= cap && incoming.get_cache_flush() && incoming.get_type() != RRType::PTR;
            if (held >= cap && !replaces) || (*room == 0 && !replaces) {
                debug!(
                    "add_or_update: dropping new record (type {:?}): no room for it",
                    incoming.get_type()
                );
                return None;
            }
            if replaces {
                let records = match incoming.get_type() {
                    RRType::PTR => self.ptr.get_mut(&entry_name),
                    RRType::SRV => self.srv.get_mut(&entry_name),
                    RRType::TXT => self.txt.get_mut(&entry_name),
                    RRType::A | RRType::AAAA => self.addr.get_mut(&entry_name_lower),
                    RRType::NSEC => self.nsec.get_mut(&entry_name),
                    _ => None,
                };
                if let Some(records) = records {
                    // The furthest back of the oldest: new ones go in front.
                    let oldest = records
                        .iter()
                        .enumerate()
                        .min_by(|(i, a), (j, b)| {
                            let created = |r: &DnsRecordIntf| r.record.get_record().get_created();
                            created(a).cmp(&created(b)).then(j.cmp(i))
                        })
                        .map(|(i, _)| i);
                    if let Some(i) = oldest {
                        records.remove(i);
                        *room = room.saturating_add(1);
                    }
                }
            }
        }

        // If it is PTR with subtype, store a mapping from the instance fullname
        // to the subtype in this cache.
        if incoming.get_type() == RRType::PTR && is_for_us {
            let (_, subtype_opt) = split_sub_domain(&entry_name);
            if let Some(subtype) = subtype_opt {
                if let Some(ptr) = incoming.any().downcast_ref::<DnsPointer>() {
                    if !self.subtype.contains_key(ptr.alias()) {
                        self.subtype
                            .insert(ptr.alias().to_string(), subtype.to_string());
                    }
                }
            }
        }

        // We want to process this `incoming`. For convenience, repeats the lookup
        // above but doing `entry().or_default()` to create an empty Vec as needed.
        let record_vec = match incoming.get_type() {
            RRType::PTR => self.ptr.entry(entry_name).or_default(),
            RRType::SRV => self.srv.entry(entry_name).or_default(),
            RRType::TXT => self.txt.entry(entry_name).or_default(),
            RRType::A | RRType::AAAA => self.addr.entry(entry_name_lower).or_default(),
            RRType::NSEC => self.nsec.entry(entry_name).or_default(),
            _ => return None,
        };

        if incoming.get_cache_flush() {
            apply_cache_flush(&incoming, record_vec, timers);
        }

        // update TTL for existing record or create a new record.
        let (idx, updated) = match record_vec
            .iter_mut()
            .enumerate()
            .find(|(_idx, r)| r.record.matches(incoming.as_ref()))
        {
            Some((i, r)) => {
                // It is possible that this record was just updated in cache_flush
                // processing. That's okay. We can still reset here.
                r.record.reset_ttl(incoming.as_ref());
                (i, false)
            }
            None => {
                let new_record = DnsRecordIntf {
                    record: incoming,
                    src_intf: intf.into(),
                    src_addr: src,
                };
                record_vec.insert(0, new_record); // A new record.
                *room = room.saturating_sub(1);
                (0, true)
            }
        };

        Some((record_vec.get(idx).unwrap(), updated))
    }

    /// Makes room for `incoming`, a new PTR record from `src` for a browsed
    /// type, before [`DnsCache::add_or_update`] takes it.
    ///
    /// A source keeps at most [`MAX_INSTANCES_PER_SOURCE`] instances of a
    /// type: a new one replaces its own oldest. When the type holds
    /// [`MAX_INSTANCES_PER_TYPE`], a newcomer replaces the oldest instance of
    /// the source that holds the most, as long as that source would still
    /// hold more than the newcomer's; otherwise it is refused. So whoever
    /// fills the cache first cannot keep a device with fewer instances out:
    /// only as many distinct sources as there are places could.
    ///
    /// A goodbye (TTL 1 or less) for an instance not cached is refused: it
    /// would take a place, maybe someone else's, for a second, to say
    /// nothing.
    ///
    /// Returns `Err(())` when `incoming` is to be refused, else the name of
    /// the instance it replaced, if any, for the listeners to be told.
    pub(crate) fn make_room_for_ptr(
        &mut self,
        incoming: &DnsRecordBox,
        src: IpAddr,
    ) -> Result<Option<String>, ()> {
        let expiring = incoming.get_record().get_ttl() <= 1;
        let Some(records) = self.ptr.get_mut(incoming.get_name()) else {
            return if expiring { Err(()) } else { Ok(None) };
        };
        if records.iter().any(|r| r.record.matches(incoming.as_ref())) {
            return Ok(None); // A refresh takes no room.
        }
        if expiring {
            return Err(());
        }
        let from_src = records.iter().filter(|r| r.src_addr == src).count();
        let evict_from: Vec<IpAddr> = if from_src >= MAX_INSTANCES_PER_SOURCE {
            vec![src]
        } else if records.len() >= MAX_INSTANCES_PER_TYPE {
            let mut per_source: HashMap<IpAddr, usize> = HashMap::new();
            for r in records.iter() {
                *per_source.entry(r.src_addr).or_default() += 1;
            }
            let most = per_source.values().copied().max().unwrap_or(0);
            if most <= from_src + 1 {
                return Err(());
            }
            // Of the sources holding the most, the one with the oldest.
            per_source
                .into_iter()
                .filter(|(_, n)| *n == most)
                .map(|(s, _)| s)
                .collect()
        } else {
            return Ok(None);
        };
        // New records go in at the front, so among records made in the same
        // millisecond the one furthest back is the oldest.
        let oldest = records
            .iter()
            .enumerate()
            .filter(|(_, r)| evict_from.contains(&r.src_addr))
            .min_by(|(i, a), (j, b)| {
                let created = |r: &DnsRecordIntf| r.record.get_record().get_created();
                created(a).cmp(&created(b)).then(j.cmp(i))
            })
            .map(|(i, _)| i);
        let Some(i) = oldest else {
            return Err(());
        };
        let evicted = records.remove(i);
        Ok(evicted
            .record
            .any()
            .downcast_ref::<DnsPointer>()
            .map(|ptr| ptr.alias().to_string()))
    }

    /// Forgets what hangs off `instance`, whose PTR record was just replaced
    /// to make room: its SRV, TXT and NSEC records, its subtype, and the
    /// addresses of its host, unless another instance's SRV record names
    /// that host or `is_resolving` wants it. They used to stay until the
    /// next sweep, and a neighbour churning through instances filled the
    /// cache with them in between, so that nobody's new records fitted.
    pub(crate) fn forget_instance(&mut self, instance: &str, is_resolving: impl Fn(&str) -> bool) {
        let hosts: Vec<String> = self
            .srv
            .remove(instance)
            .into_iter()
            .flatten()
            .filter_map(|r| {
                r.record
                    .any()
                    .downcast_ref::<DnsSrv>()
                    .map(|srv| srv.host().to_lowercase())
            })
            .collect();
        self.txt.remove(instance);
        self.nsec.remove(instance);
        self.subtype.remove(instance);
        if hosts.is_empty() {
            return;
        }
        let still_named = self.host_names(&self.instance_names());
        for host in hosts {
            if !still_named.contains(&host) && !is_resolving(&host) {
                self.addr.remove(&host);
            }
        }
    }

    /// Drops what nothing needs any more: expired SRV, TXT, NSEC and
    /// address records wherever they are (expired PTR records are
    /// [`DnsCache::evict_expired_services`]'s, which reports them), and,
    /// unless `keep_all`, every record that no browsed type or hostname
    /// resolution leads to -- PTR records of types no longer browsed, SRV,
    /// TXT and NSEC records of instances no PTR record points to, addresses
    /// of hosts no such SRV record names. Before, such records were kept
    /// for good: expiry only followed PTR records.
    pub(crate) fn sweep(
        &mut self,
        now: u64,
        keep_all: bool,
        is_browsed: impl Fn(&str) -> bool,
        is_resolving: impl Fn(&str) -> bool,
    ) {
        for map in [&mut self.srv, &mut self.txt, &mut self.nsec, &mut self.addr] {
            map.retain(|_, records| {
                records.retain(|r| !r.record.get_record().is_expired(now));
                !records.is_empty()
            });
        }
        self.ptr
            .retain(|ty, records| !records.is_empty() && (keep_all || is_browsed(ty)));
        let instances = self.instance_names();
        self.subtype
            .retain(|instance, _| instances.contains(instance));
        if keep_all {
            return;
        }
        self.srv.retain(|name, _| instances.contains(name));
        self.txt.retain(|name, _| instances.contains(name));
        let hosts = self.host_names(&instances);
        self.addr
            .retain(|host, _| hosts.contains(host) || is_resolving(host));
        self.nsec.retain(|name, _| {
            let lower = name.to_lowercase();
            instances.contains(name) || hosts.contains(&lower) || is_resolving(&lower)
        });
    }

    /// Remove a record from the cache if exists, otherwise no-op
    pub(crate) fn remove(&mut self, record: &DnsRecordBox) -> bool {
        let mut found = false;
        let record_name = record.get_name();
        let record_vec = match record.get_type() {
            RRType::PTR => self.ptr.get_mut(record_name),
            RRType::SRV => self.srv.get_mut(record_name),
            RRType::TXT => self.txt.get_mut(record_name),
            RRType::A | RRType::AAAA => self.addr.get_mut(record_name),
            _ => return found,
        };
        if let Some(record_vec) = record_vec {
            record_vec.retain(|x| match x.record.matches(record.as_ref()) {
                true => {
                    found = true;
                    false
                }
                false => true,
            });
        }
        found
    }

    /// Iterates all ADDR records and remove ones that expired.
    /// Returns the expired ones in a map of names and addresses.
    pub(crate) fn evict_expired_addr(&mut self, now: u64) -> HashMap<String, HashSet<ScopedIp>> {
        let mut removed = HashMap::new();

        self.addr.retain(|_, records| {
            records.retain(|addr| {
                let expired = addr.record.get_record().is_expired(now);
                if expired {
                    if let Some(addr_record) = addr.record.any().downcast_ref::<DnsAddress>() {
                        trace!("evict expired ADDR: {:?}", addr_record);
                        removed
                            .entry(addr.record.get_name().to_string())
                            .or_insert_with(HashSet::new)
                            .insert(addr_record.address());
                    }
                }
                !expired
            });

            !records.is_empty()
        });

        removed
    }

    /// Evicts expired PTR and SRV, TXT records for each ty_domain in the cache, and
    /// returns the set of expired instance names for each ty_domain.
    ///
    /// An instance in the returned set indicates its PTR and/or SRV record has expired.
    pub(crate) fn evict_expired_services(&mut self, now: u64) -> HashMap<String, HashSet<String>> {
        let mut expired_instances = HashMap::new();

        // Check all ty_domain in the cache by following all PTR records, regardless
        // if the ty_domain is actively queried or not.
        for (ty_domain, ptr_records) in self.ptr.iter_mut() {
            for ptr in ptr_records.iter() {
                if let Some(dns_ptr) = ptr.record.any().downcast_ref::<DnsPointer>() {
                    let instance_name = dns_ptr.alias();

                    // evict expired SRV records of this instance
                    if let Some(srv_records) = self.srv.get_mut(instance_name) {
                        srv_records.retain(|srv| {
                            let expired = srv.record.get_record().is_expired(now);
                            !expired
                        });

                        if srv_records.is_empty() {
                            debug!("expired SRV for {}: {:?}", ty_domain, instance_name);
                            expired_instances
                                .entry(ty_domain.to_string())
                                .or_insert_with(HashSet::new)
                                .insert(instance_name.to_string());

                            // don't keep empty value for this key.
                            self.srv.remove(instance_name);
                        }
                    }

                    // evict expired TXT records of this instance
                    if let Some(txt_records) = self.txt.get_mut(instance_name) {
                        txt_records.retain(|txt| !txt.record.get_record().is_expired(now))
                    }
                }
            }

            // evict expired PTR records
            ptr_records.retain(|x| {
                let expired = x.record.get_record().is_expired(now);
                if expired {
                    if let Some(dns_ptr) = x.record.any().downcast_ref::<DnsPointer>() {
                        trace!("expired PTR: domain:{ty_domain} record: {:?}", dns_ptr);
                        expired_instances
                            .entry(ty_domain.to_string())
                            .or_insert_with(HashSet::new)
                            .insert(dns_ptr.alias().to_string());
                    }
                }
                !expired
            });
        }

        expired_instances
    }

    /// Removes all records of a service type: PTR, SRV, TXT records and any ADDR records
    /// that are not referenced by any SRV record.
    pub(crate) fn remove_service_type(&mut self, ty_domain: &str) {
        let Some(ptr_records) = self.ptr.get_mut(ty_domain) else {
            return;
        };

        let mut hosts = HashSet::new();

        for ptr in ptr_records.iter() {
            if let Some(dns_ptr) = ptr.record.any().downcast_ref::<DnsPointer>() {
                let instance_name = dns_ptr.alias();

                // collect all hostnames from SRV records of this instance
                if let Some(srv_records) = self.srv.get_mut(instance_name) {
                    for srv in srv_records.iter() {
                        if let Some(dns_srv) = srv.record.any().downcast_ref::<DnsSrv>() {
                            hosts.insert(dns_srv.host().to_lowercase());
                        }
                    }
                }

                // remove all SRV records of this instance
                self.srv.remove(instance_name);

                // remove all TXT records of this instance
                self.txt.remove(instance_name);
            }
        }

        self.ptr.remove(ty_domain);

        // Check all hostnames in `hosts`: for each hostname, check if any SRV record
        // has `hostname` as its host. If no such SRV, remove the ADDR records of this hostname.
        for host in hosts {
            let mut has_srv = false;
            for srv_records in self.srv.values() {
                for srv in srv_records.iter() {
                    if let Some(dns_srv) = srv.record.any().downcast_ref::<DnsSrv>() {
                        if dns_srv.host().to_lowercase() == host {
                            has_srv = true;
                            break;
                        }
                    }
                }
                if has_srv {
                    break;
                }
            }

            if !has_srv {
                self.addr.remove(&host);
            }
        }
    }

    /// Checks refresh due for PTR records of `ty_domain`.
    /// Returns all updated refresh time.
    pub(crate) fn refresh_due_ptr(&mut self, ty_domain: &str) -> HashSet<u64> {
        let now = current_time_millis();

        // Check all PTR records for this ty_domain.
        self.ptr
            .get_mut(ty_domain)
            .into_iter()
            .flatten()
            .filter_map(|record| record.record.updated_refresh_time(now))
            .collect()
    }

    /// Returns a tuple of:
    /// 1. the map of instance names together with RRType(s) that are due for refresh
    ///    its SRV or TXT records.
    /// 2. the set of new timers that are due for refresh.
    pub(crate) fn refresh_due_srv_txt(
        &mut self,
        ty_domain: &str,
    ) -> (HashMap<String, Vec<RRType>>, HashSet<u64>) {
        let now = current_time_millis();

        let instances: Vec<_> = self
            .ptr
            .get(ty_domain)
            .into_iter()
            .flatten()
            .filter(|record| !record.record.get_record().is_expired(now))
            .filter_map(|record| {
                record
                    .record
                    .any()
                    .downcast_ref::<DnsPointer>()
                    .map(|ptr| ptr.alias())
            })
            .collect();

        let mut refresh_due: HashMap<String, Vec<RRType>> = HashMap::new();
        let mut new_timers = HashSet::new();
        for instance in instances {
            // Check SRV records.
            let refresh_timers: HashSet<u64> = self
                .srv
                .get_mut(instance)
                .into_iter()
                .flatten()
                .filter_map(|record| record.record.updated_refresh_time(now))
                .collect();

            if !refresh_timers.is_empty() {
                refresh_due
                    .entry(instance.to_string())
                    .and_modify(|v| v.push(RRType::SRV))
                    .or_insert(vec![RRType::SRV]);
                new_timers.extend(refresh_timers);
            }

            // Check TXT records.
            let refresh_timers: HashSet<u64> = self
                .txt
                .get_mut(instance)
                .into_iter()
                .flatten()
                .filter_map(|record| record.record.updated_refresh_time(now))
                .collect();

            if !refresh_timers.is_empty() {
                refresh_due
                    .entry(instance.to_string())
                    .and_modify(|v| v.push(RRType::TXT))
                    .or_insert(vec![RRType::TXT]);
                new_timers.extend(refresh_timers);
            }
        }

        (refresh_due, new_timers)
    }

    /// Returns the set of `host`, where refreshing the A / AAAA records is due
    /// for a `ty_domain`.
    pub(crate) fn refresh_due_hosts(&mut self, ty_domain: &str) -> (HashSet<String>, HashSet<u64>) {
        let now = current_time_millis();

        let instances: Vec<_> = self
            .ptr
            .get(ty_domain)
            .into_iter()
            .flatten()
            .filter(|record| !record.record.get_record().is_expired(now))
            .filter_map(|record| {
                record
                    .record
                    .any()
                    .downcast_ref::<DnsPointer>()
                    .map(|ptr| ptr.alias())
            })
            .collect();

        // Collect hostnames we have browsers for by SRV records.
        let mut hostnames_browsed = HashSet::new();
        for instance in instances {
            let hosts: HashSet<String> = self
                .srv
                .get(instance)
                .into_iter()
                .flatten()
                .filter_map(|record| {
                    record
                        .record
                        .any()
                        .downcast_ref::<DnsSrv>()
                        .map(|srv| srv.host().to_string())
                })
                .collect();

            hostnames_browsed.extend(hosts);
        }
        let mut refresh_due = HashSet::new();
        let mut new_timers = HashSet::new();
        for hostname in hostnames_browsed {
            let refresh_timers: HashSet<u64> = self
                .addr
                .get_mut(&hostname.to_lowercase())
                .into_iter()
                .flatten()
                .filter_map(|record| record.record.updated_refresh_time(now))
                .collect();

            if !refresh_timers.is_empty() {
                refresh_due.insert(hostname);
                new_timers.extend(refresh_timers);
            }
        }
        (refresh_due, new_timers)
    }

    /// Returns the set of A/AAAA records that are due for refresh for a `hostname`.
    ///
    /// For these records, their refresh time will be updated so that they will not refresh again.
    pub(crate) fn refresh_due_hostname_resolutions(
        &mut self,
        hostname: &str,
    ) -> HashSet<(String, ScopedIp)> {
        let now = current_time_millis();

        self.addr
            .get_mut(hostname)
            .into_iter()
            .flatten()
            .filter_map(|record| {
                let rec = record.record.get_record_mut();
                if rec.is_expired(now) || !rec.refresh_due(now) {
                    return None;
                }
                rec.refresh_no_more();

                Some((
                    hostname.to_owned(),
                    record
                        .record
                        .any()
                        .downcast_ref::<DnsAddress>()
                        .unwrap()
                        .address(),
                ))
            })
            .collect()
    }

    /// Returns a list of Known Answer for a given question of `name` with `qtype`.
    /// The timestamp `now` is passed in to check TTL.
    ///
    /// Reference:  RFC 6762 section 7.1
    pub(crate) fn get_known_answers<'a>(
        &'a self,
        name: &str,
        qtype: RRType,
        now: u64,
    ) -> Vec<&'a DnsRecordIntf> {
        let records_opt = match qtype {
            RRType::PTR => self.get_ptr(name),
            RRType::SRV => self.get_srv(name),
            RRType::A | RRType::AAAA => self.get_addr(name),
            RRType::TXT => self.get_txt(name),
            _ => None,
        };

        let records = match records_opt {
            Some(items) => items,
            None => return Vec::new(),
        };

        // From RFC 6762 section 7.1:
        // ..Generally, this applies only to Shared records, not Unique records,..
        //
        // ..a Multicast DNS querier SHOULD NOT include
        // records in the Known-Answer list whose remaining TTL is less than
        // half of their original TTL.
        records
            .iter()
            .filter(move |r| {
                !r.record.get_record().is_unique() && !r.record.get_record().halflife_passed(now)
            })
            .collect()
    }

    /// Removes cached address records on a disabled interface, filtered by IP version.
    /// Use `IpType::V4` for A records only, `IpType::V6` for AAAA only,
    /// or `IpType::V4 | IpType::V6` for both.
    pub(crate) fn remove_addrs_on_disabled_intf(
        &mut self,
        disabled_if_index: u32,
        ip_type: IpType,
    ) {
        for (host, records) in self.addr.iter_mut() {
            records.retain(|record| {
                let Some(dns_addr) = record.record.any().downcast_ref::<DnsAddress>() else {
                    return false; // invalid address record.
                };

                // Remove the record if it is on this interface and matches the IP version filter.
                if dns_addr.interface_id.index == disabled_if_index {
                    let rr_type = dns_addr.record.entry.ty;
                    let version_matches = (rr_type == RRType::A && ip_type.contains(IpType::V4))
                        || (rr_type == RRType::AAAA && ip_type.contains(IpType::V6));
                    if version_matches {
                        debug!(
                            "removing ADDR on disabled intf: {:?} host {host}",
                            dns_addr.interface_id.name
                        );
                        return false;
                    }
                }
                true
            });
        }
    }

    /// Removes all records that were received on `intf_id`.
    /// Returns a tuple of:
    /// 1. a map of fully removed instances per ty_domain (PTR gone).
    /// 2. a set of modified instances that lost records but still have PTR entries.
    pub(crate) fn remove_records_on_intf(&mut self, intf_id: InterfaceId) -> IntfRemovalResult {
        let mut removed_instances = HashMap::new();
        let mut modified_instances = HashSet::new();

        self.ptr.iter_mut().for_each(|(name, records)| {
            let mut instances_on_intf: HashSet<String> = HashSet::new();

            // Remove PTR records on `intf_id` and collect their instance names.
            records.retain(|r| {
                if r.src_intf == intf_id {
                    if let Some(dns_ptr) = r.record.any().downcast_ref::<DnsPointer>() {
                        trace!("removing PTR on intf {:?}: {:?}", intf_id, dns_ptr);
                        instances_on_intf.insert(dns_ptr.alias().to_string());
                    }
                    false
                } else {
                    true
                }
            });

            for instance in instances_on_intf {
                // if no more record for this instance on any intf, we shall fully remove it.
                if !records.iter().any(|r| {
                    if let Some(dns_ptr) = r.record.any().downcast_ref::<DnsPointer>() {
                        dns_ptr.alias() == instance
                    } else {
                        false
                    }
                }) {
                    removed_instances
                        .entry(name.to_string())
                        .or_insert_with(HashSet::new)
                        .insert(instance);
                }
            }
        });

        // Remove any PTR entry that no longer has records.
        self.ptr.retain(|_, records| !records.is_empty());

        // Clean up SRV and TXT records for fully removed instances.
        let all_removed: HashSet<&String> = removed_instances.values().flatten().collect();
        self.srv
            .retain(|instance, _| !all_removed.contains(instance));
        self.txt
            .retain(|instance, _| !all_removed.contains(instance));

        // Filter remaining SRV/TXT by intf_id
        self.srv.iter_mut().for_each(|(instance, records)| {
            let before = records.len();
            records.retain(|r| r.src_intf != intf_id);
            if records.len() != before {
                modified_instances.insert(instance.clone());
            }
        });
        self.srv.retain(|_, records| !records.is_empty());

        self.txt.iter_mut().for_each(|(instance, records)| {
            let before = records.len();
            records.retain(|r| r.src_intf != intf_id);
            if records.len() != before {
                modified_instances.insert(instance.clone());
            }
        });
        self.txt.retain(|_, records| !records.is_empty());

        // For ADDR records, track which hostnames lost records.
        let mut affected_hosts = HashSet::new();
        self.addr.iter_mut().for_each(|(host, records)| {
            let before = records.len();
            records.retain(|r| r.src_intf != intf_id);
            if records.len() != before {
                // These hosts are in lower case.
                affected_hosts.insert(host.clone());
            }
        });
        self.addr.retain(|_, records| !records.is_empty());

        // Map affected hosts back to instances via SRV hostname lookup.
        if !affected_hosts.is_empty() {
            for (instance, srv_records) in self.srv.iter() {
                for srv in srv_records {
                    if let Some(dns_srv) = srv.record.any().downcast_ref::<DnsSrv>() {
                        if affected_hosts.contains(&dns_srv.host().to_lowercase())
                            && !modified_instances.contains(instance)
                        {
                            modified_instances.insert(instance.clone());
                        }
                    }
                }
            }
        }

        self.nsec.values_mut().for_each(|records| {
            records.retain(|r| r.src_intf != intf_id);
        });
        self.nsec.retain(|_, records| !records.is_empty());

        IntfRemovalResult {
            removed_instances,
            modified_instances,
        }
    }
}

/// When a record has the cache flush bit set, we need to update the expire time of
/// existing records of the same type and class per RFC 6762 Section 10.2.
fn apply_cache_flush(
    incoming: &DnsRecordBox,
    existing_records: &mut [DnsRecordIntf],
    timers: &mut Vec<u64>,
) {
    let now = current_time_millis();
    let class = incoming.get_class();
    let rtype = incoming.get_type();

    existing_records.iter_mut().for_each(|r| {
        // When cache flush is asked, we set expire date to 1 second in the future if:
        // - The record has the same rclass
        // - The record was created more than 1 second ago.
        // - The record expire is more than 1 second away.
        // Ref: RFC 6762 Section 10.2
        //
        // Note: when the updated record actually expires, it will trigger events properly.
        let mut should_flush = false;

        if class == r.record.get_class()
            && rtype == r.record.get_type()
            && now > r.record.get_created() + 1000
            && r.record.get_expire() > now + 1000
        {
            should_flush = true;

            // additional checks for address records.
            if rtype == RRType::A || rtype == RRType::AAAA {
                if let Some(addr) = r.record.any().downcast_ref::<DnsAddress>() {
                    if let Some(addr_b) = incoming.any().downcast_ref::<DnsAddress>() {
                        should_flush = addr.interface_id.index == addr_b.interface_id.index;
                    }
                }
            }
        }

        if should_flush {
            trace!("FLUSH one record: {:?}", &r.record);
            let new_expire = now + 1000;
            r.record.set_expire(new_expire);

            // Add a timer so the run loop will handle this expire.
            timers.push(new_expire);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dns_parser::{DnsAddress, DnsPointer, DnsRecordExt, DnsSrv, DnsTxt, RRType, CLASS_IN},
        service_info::MyIntf,
        MAX_PKT_DEFAULT,
    };
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr};

    const SRC: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

    fn make_intf(name: &str, index: u32) -> MyIntf {
        MyIntf {
            name: name.to_string(),
            index,
            addrs: HashSet::new(),
            max_packet_size_v4: MAX_PKT_DEFAULT,
            max_packet_size_v6: MAX_PKT_DEFAULT,
        }
    }

    /// Two interfaces discover the same service instance. All record types are
    /// stored per name (ty_domain / instance / hostname) as the map key. For
    /// PTR/SRV/TXT, `matches()` does not include interface_id, so a second
    /// insert from another interface just resets the TTL — only one Vec entry
    /// exists per logical record. For ADDR, `matches()` includes interface_id,
    /// so each interface produces a separate Vec entry under the same hostname.
    ///
    /// Setup: PTR, SRV, TXT come from intf_b. ADDR exists on both intf_a and
    /// intf_b (different IPs). Removing intf_a should leave PTR/SRV/TXT intact
    /// but remove addr_a, producing a `modified_instances` entry.
    #[test]
    fn test_modified_instance_when_intf_removed() {
        let ty_domain = "_http._tcp.local.";
        let instance = "my-svc._http._tcp.local.";
        let host = "myhost.local.";
        let addr_a: IpAddr = "192.168.1.1".parse().unwrap();
        let addr_b: IpAddr = "192.168.2.1".parse().unwrap();

        let intf_a = make_intf("en0", 1);
        let intf_b = make_intf("en1", 2);

        let mut cache = DnsCache::new();
        let mut timers = Vec::new();
        let mut room = MAX_CACHED_RECORDS;

        macro_rules! add {
            ($intf:expr, $record:expr) => {
                cache.add_or_update(&$intf, $record.boxed(), &mut timers, true, SRC, &mut room)
            };
        }

        // PTR, SRV, TXT from intf_b only — these survive when intf_a is removed.
        add!(
            intf_b,
            DnsPointer::new(ty_domain, RRType::PTR, CLASS_IN, 4500, instance.to_string())
        );
        add!(
            intf_b,
            DnsSrv::new(instance, CLASS_IN, 4500, 0, 0, 80, host.to_string())
        );
        add!(intf_b, DnsTxt::new(instance, CLASS_IN, 4500, vec![]));

        // ADDR: each interface contributes its own address (interface is part of identity).
        let intf_a_id = InterfaceId {
            name: "en0".to_string(),
            index: 1,
        };
        let intf_b_id = InterfaceId {
            name: "en1".to_string(),
            index: 2,
        };
        add!(
            intf_a,
            DnsAddress::new(host, RRType::A, CLASS_IN, 4500, addr_a, intf_a_id.clone())
        );
        add!(
            intf_b,
            DnsAddress::new(host, RRType::A, CLASS_IN, 4500, addr_b, intf_b_id)
        );

        // Remove interface A.
        let result = cache.remove_records_on_intf(intf_a_id);

        // PTR still exists via intf_b — instance is not fully removed.
        assert!(
            result.removed_instances.is_empty(),
            "expected no removed instances, got {:?}",
            result.removed_instances
        );

        // addr_a was removed, so the instance is in modified_instances.
        assert!(
            result.modified_instances.contains(instance),
            "expected {instance} in modified_instances, got {:?}",
            result.modified_instances
        );

        // Only addr_b remains in the cache.
        let addrs = cache.get_addresses_for_host(host);
        let all_ips: HashSet<IpAddr> = addrs
            .values()
            .flatten()
            .map(|scoped| scoped.to_ip_addr())
            .collect();
        assert_eq!(all_ips, HashSet::from([addr_b]));
    }

    /// A response record that is not for us (no querier) and has no existing
    /// cache entry must be dropped *without* leaving an empty `Vec` behind in
    /// the cache map. Otherwise the cache grows by one entry per distinct name
    /// seen on the network, driven by other hosts' unsolicited traffic.
    #[test]
    fn test_not_for_us_record_leaves_no_empty_entry() {
        let ty_domain = "_other._tcp.local.";
        let instance = "someone-else._other._tcp.local.";
        let host = "otherhost.local.";
        let addr: IpAddr = "192.168.1.9".parse().unwrap();

        let intf = make_intf("en0", 1);
        let intf_id = InterfaceId {
            name: "en0".to_string(),
            index: 1,
        };

        let mut cache = DnsCache::new();
        let mut timers = Vec::new();
        let mut room = MAX_CACHED_RECORDS;

        // Feed PTR / SRV / TXT / ADDR records with `is_for_us = false` and no
        // pre-existing entries. Each must be dropped.
        macro_rules! add_not_for_us {
            ($record:expr) => {{
                let result =
                    cache.add_or_update(&intf, $record.boxed(), &mut timers, false, SRC, &mut room);
                assert!(
                    result.is_none(),
                    "a not-for-us record should be dropped, got {:?}",
                    result.map(|(r, _)| r.record.get_name().to_string())
                );
            }};
        }

        add_not_for_us!(DnsPointer::new(
            ty_domain,
            RRType::PTR,
            CLASS_IN,
            4500,
            instance.to_string()
        ));
        add_not_for_us!(DnsSrv::new(
            instance,
            CLASS_IN,
            4500,
            0,
            0,
            80,
            host.to_string()
        ));
        add_not_for_us!(DnsTxt::new(instance, CLASS_IN, 4500, vec![]));
        add_not_for_us!(DnsAddress::new(
            host,
            RRType::A,
            CLASS_IN,
            4500,
            addr,
            intf_id
        ));

        // None of these should have created an entry (empty or otherwise).
        assert!(
            cache.ptr.is_empty(),
            "ptr map leaked: {:?}",
            cache.ptr.keys()
        );
        assert!(
            cache.srv.is_empty(),
            "srv map leaked: {:?}",
            cache.srv.keys()
        );
        assert!(
            cache.txt.is_empty(),
            "txt map leaked: {:?}",
            cache.txt.keys()
        );
        assert!(
            cache.addr.is_empty(),
            "addr map leaked: {:?}",
            cache.addr.keys()
        );
    }
}

/// Sukkula's tests of the bounds its patch adds (third_party/mdns-sd.patches).
#[cfg(test)]
mod bounded_cache_tests {
    use super::*;
    use crate::{
        dns_parser::{
            DnsAddress, DnsPointer, DnsRecordExt, DnsSrv, DnsTxt, RRType, CLASS_CACHE_FLUSH,
            CLASS_IN,
        },
        service_info::MyIntf,
        MAX_PKT_DEFAULT,
    };
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr};

    const TY: &str = "_FC9F5ED42C8A._tcp.local.";

    fn intf() -> MyIntf {
        MyIntf {
            name: "wlan0".to_string(),
            index: 3,
            addrs: HashSet::new(),
            max_packet_size_v4: MAX_PKT_DEFAULT,
            max_packet_size_v6: MAX_PKT_DEFAULT,
        }
    }

    fn src(n: u16) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(
            192,
            168,
            (n / 250) as u8,
            (n % 250) as u8 + 1,
        ))
    }

    fn ptr(instance: &str, ttl: u32) -> DnsRecordBox {
        DnsPointer::new(TY, RRType::PTR, CLASS_IN, ttl, instance.to_string()).boxed()
    }

    /// Offers a PTR record the way `handle_response` does: room first.
    fn offer_ptr(cache: &mut DnsCache, instance: &str, from: IpAddr) -> Result<Option<String>, ()> {
        let record = ptr(instance, 4500);
        let replaced = cache.make_room_for_ptr(&record, from)?;
        let mut room = MAX_CACHED_RECORDS.saturating_sub(cache.total_records());
        cache
            .add_or_update(&intf(), record, &mut Vec::new(), true, from, &mut room)
            .ok_or(())?;
        Ok(replaced)
    }

    fn instances(cache: &DnsCache) -> HashSet<String> {
        cache.instance_names()
    }

    #[test]
    fn a_new_record_needs_to_be_asked_for_and_to_have_room() {
        let mut cache = DnsCache::new();
        let mut timers = Vec::new();
        let instance = "a._FC9F5ED42C8A._tcp.local.";
        let host = "a.local.";
        let mut room = MAX_CACHED_RECORDS;
        let srv = |port| DnsSrv::new(instance, CLASS_IN, 120, 0, 0, port, host.to_string()).boxed();

        // Not asked for: dropped.
        assert!(cache
            .add_or_update(&intf(), srv(1), &mut timers, false, src(1), &mut room)
            .is_none());
        // Asked for: taken, up to the cap per name.
        for port in 1..=MAX_RECORDS_PER_NAME as u16 {
            assert!(cache
                .add_or_update(&intf(), srv(port), &mut timers, true, src(1), &mut room)
                .is_some());
        }
        assert!(cache
            .add_or_update(&intf(), srv(99), &mut timers, true, src(1), &mut room)
            .is_none());
        assert_eq!(cache.srv_count(), MAX_RECORDS_PER_NAME);
        assert_eq!(room, MAX_CACHED_RECORDS - MAX_RECORDS_PER_NAME);

        // A record already held is refreshed even when nothing is left and
        // nobody asked: that keeps the cache coherent and grows nothing.
        let mut none_left = 0;
        assert!(matches!(
            cache.add_or_update(&intf(), srv(1), &mut timers, false, src(1), &mut none_left),
            Some((_, false))
        ));
        // A new one is not taken once the total is used up.
        let other = DnsTxt::new(instance, CLASS_IN, 120, vec![1, b'x']).boxed();
        assert!(cache
            .add_or_update(&intf(), other, &mut timers, true, src(1), &mut none_left)
            .is_none());

        // Addresses: at most MAX_ADDRS_PER_HOST per host name.
        let id = InterfaceId {
            name: "wlan0".to_string(),
            index: 3,
        };
        for i in 0..=MAX_ADDRS_PER_HOST as u8 {
            let a = DnsAddress::new(
                host,
                RRType::A,
                CLASS_IN,
                120,
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, i)),
                id.clone(),
            )
            .boxed();
            let taken = cache
                .add_or_update(&intf(), a, &mut timers, true, src(1), &mut room)
                .is_some();
            assert_eq!(taken, usize::from(i) < MAX_ADDRS_PER_HOST, "address {i}");
        }
    }

    #[test]
    fn a_unique_record_at_the_cap_replaces_the_oldest() {
        let mut cache = DnsCache::new();
        let mut timers = Vec::new();
        let mut room = MAX_CACHED_RECORDS;
        let instance = "a._FC9F5ED42C8A._tcp.local.";
        let txt = |v: u8, class| DnsTxt::new(instance, class, 120, vec![1, v]).boxed();
        for v in 0..MAX_RECORDS_PER_NAME as u8 {
            cache.add_or_update(
                &intf(),
                txt(v, CLASS_IN),
                &mut timers,
                true,
                src(1),
                &mut room,
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // A shared record at the cap is refused...
        assert!(cache
            .add_or_update(
                &intf(),
                txt(50, CLASS_IN),
                &mut timers,
                true,
                src(1),
                &mut room
            )
            .is_none());
        // ... a unique one takes the oldest one's place.
        let new = txt(51, CLASS_IN | CLASS_CACHE_FLUSH);
        assert!(cache
            .add_or_update(&intf(), new, &mut timers, true, src(1), &mut room)
            .is_some());
        assert_eq!(cache.txt_count(), MAX_RECORDS_PER_NAME);
        let held: Vec<Vec<u8>> = cache.txt[instance]
            .iter()
            .filter_map(|r| r.record.any().downcast_ref::<DnsTxt>())
            .map(|t| t.text().to_vec())
            .collect();
        assert!(!held.contains(&vec![1, 0]), "the oldest is gone: {held:?}");
        assert!(held.contains(&vec![1, 51]));
    }

    #[test]
    fn one_source_holds_at_most_a_few_instances_of_a_type() {
        let mut cache = DnsCache::new();
        let one = src(1);
        for i in 0..MAX_INSTANCES_PER_SOURCE {
            assert_eq!(offer_ptr(&mut cache, &format!("i{i}.{TY}"), one), Ok(None));
        }
        // The next one from the same source replaces its own oldest.
        let replaced = offer_ptr(&mut cache, &format!("new.{TY}"), one).unwrap();
        assert!(replaced.is_some());
        assert_eq!(cache.ptr_count(), MAX_INSTANCES_PER_SOURCE);
        assert!(instances(&cache).contains(&format!("new.{TY}")));
        // A refresh takes no room and replaces nothing.
        assert_eq!(offer_ptr(&mut cache, &format!("new.{TY}"), one), Ok(None));
        assert_eq!(cache.ptr_count(), MAX_INSTANCES_PER_SOURCE);
    }

    #[test]
    fn a_full_type_still_takes_a_source_with_fewer_instances() {
        // Filled by as few sources as can fill it, each at its own cap.
        let mut cache = DnsCache::new();
        let sources = MAX_INSTANCES_PER_TYPE / MAX_INSTANCES_PER_SOURCE;
        for s in 0..sources {
            for i in 0..MAX_INSTANCES_PER_SOURCE {
                offer_ptr(&mut cache, &format!("s{s}i{i}.{TY}"), src(s as u16)).unwrap();
            }
        }
        assert_eq!(cache.ptr_count(), MAX_INSTANCES_PER_TYPE);
        // An honest device announcing one instance gets in, in the place of
        // an instance of a source holding the most.
        let honest = src(1000);
        let replaced = offer_ptr(&mut cache, &format!("honest.{TY}"), honest).unwrap();
        assert!(replaced.is_some());
        assert_eq!(cache.ptr_count(), MAX_INSTANCES_PER_TYPE);
        assert!(instances(&cache).contains(&format!("honest.{TY}")));

        // Filled by as many sources as there are places, one each: a
        // newcomer is refused, as it would take someone's only place.
        let mut cache = DnsCache::new();
        for s in 0..MAX_INSTANCES_PER_TYPE {
            offer_ptr(&mut cache, &format!("s{s}.{TY}"), src(s as u16)).unwrap();
        }
        assert_eq!(
            offer_ptr(&mut cache, &format!("late.{TY}"), honest),
            Err(())
        );
        assert_eq!(cache.ptr_count(), MAX_INSTANCES_PER_TYPE);
    }

    #[test]
    fn a_goodbye_for_an_instance_not_held_takes_no_place() {
        let mut cache = DnsCache::new();
        assert_eq!(
            cache.make_room_for_ptr(&ptr(&format!("x.{TY}"), 1), src(1)),
            Err(())
        );
        offer_ptr(&mut cache, &format!("x.{TY}"), src(1)).unwrap();
        // A goodbye for one held is let through, to expire it.
        assert_eq!(
            cache.make_room_for_ptr(&ptr(&format!("x.{TY}"), 1), src(1)),
            Ok(None)
        );
    }

    #[test]
    fn a_replaced_instance_takes_its_records_with_it() {
        let mut cache = DnsCache::new();
        let mut room = MAX_CACHED_RECORDS;
        let id = InterfaceId {
            name: "wlan0".to_string(),
            index: 3,
        };
        let one = src(1);
        let add = |cache: &mut DnsCache, room: &mut usize, i: usize, host: &str| {
            let instance = format!("i{i}.{TY}");
            offer_ptr(cache, &instance, one).unwrap();
            for r in [
                DnsSrv::new(&instance, CLASS_IN, 120, 0, 0, 9, host.to_string()).boxed(),
                DnsTxt::new(&instance, CLASS_IN, 120, vec![1, b'n']).boxed(),
                DnsAddress::new(
                    host,
                    RRType::A,
                    CLASS_IN,
                    120,
                    IpAddr::V4(Ipv4Addr::new(192, 168, 1, i as u8)),
                    id.clone(),
                )
                .boxed(),
            ] {
                cache.add_or_update(&intf(), r, &mut Vec::new(), true, one, room);
            }
        };
        // Two instances on one host, the rest on hosts of their own.
        add(&mut cache, &mut room, 0, "shared.local.");
        add(&mut cache, &mut room, 1, "shared.local.");
        for i in 2..MAX_INSTANCES_PER_SOURCE {
            add(&mut cache, &mut room, i, &format!("h{i}.local."));
        }
        let before = cache.total_records();
        // i0 goes: its SRV and TXT with it, but not the host i1 still names.
        let gone = cache.make_room_for_ptr(&ptr(&format!("new.{TY}"), 4500), one);
        assert_eq!(gone, Ok(Some(format!("i0.{TY}"))));
        cache.forget_instance(&format!("i0.{TY}"), |_| false);
        assert!(cache.get_srv(&format!("i0.{TY}")).is_none());
        assert!(cache.txt.get(&format!("i0.{TY}")).is_none());
        assert!(cache.addr.contains_key("shared.local."));
        assert_eq!(cache.total_records(), before - 3);
        // i2's host is its alone, and goes with it -- unless resolved.
        cache.forget_instance(&format!("i2.{TY}"), |host| host == "h2.local.");
        assert!(cache.addr.contains_key("h2.local."));
        cache.forget_instance(&format!("i3.{TY}"), |_| false);
        assert!(!cache.addr.contains_key("h3.local."));
    }

    #[test]
    fn the_next_deadline_is_the_earliest_one_still_ahead() {
        let mut cache = DnsCache::new();
        assert_eq!(cache.next_deadline(0), None);
        let mut room = MAX_CACHED_RECORDS;
        let short = DnsPointer::new(TY, RRType::PTR, CLASS_IN, 10, format!("a.{TY}")).boxed();
        let long = DnsPointer::new(TY, RRType::PTR, CLASS_IN, 100, format!("b.{TY}")).boxed();
        let created = short.get_record().get_created();
        cache.add_or_update(&intf(), short, &mut Vec::new(), true, src(1), &mut room);
        cache.add_or_update(&intf(), long, &mut Vec::new(), true, src(1), &mut room);
        // 80 % of the short TTL comes first; then its expiry.
        let refresh = cache.next_deadline(created).unwrap();
        assert!(
            (created + 7_990..=created + 8_010).contains(&refresh),
            "{refresh}"
        );
        let expiry = cache.next_deadline(refresh).unwrap();
        assert!(
            (created + 9_990..=created + 10_010).contains(&expiry),
            "{expiry}"
        );
        // Past both, the long record's refresh is next, not a deadline gone by.
        let next = cache.next_deadline(expiry).unwrap();
        assert!(next >= created + 79_990, "{next}");
    }

    #[test]
    fn records_nothing_leads_to_are_swept() {
        let mut cache = DnsCache::new();
        let mut room = MAX_CACHED_RECORDS;
        let mut timers = Vec::new();
        let instance = format!("a.{TY}");
        let host = "a.local.";
        let id = InterfaceId {
            name: "wlan0".to_string(),
            index: 3,
        };
        let records = [
            ptr(&instance, 4500),
            DnsSrv::new(&instance, CLASS_IN, 120, 0, 0, 9, host.to_string()).boxed(),
            DnsTxt::new(&instance, CLASS_IN, 4500, vec![1, b'n']).boxed(),
            DnsAddress::new(
                host,
                RRType::A,
                CLASS_IN,
                120,
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7)),
                id,
            )
            .boxed(),
        ];
        for r in records {
            cache.add_or_update(&intf(), r, &mut timers, true, src(1), &mut room);
        }
        let now = crate::current_time_millis();
        // While the type is browsed, everything stays.
        cache.sweep(now, false, |ty| ty == TY, |_| false);
        assert_eq!(cache.total_records(), 4);
        // Once it is not, nothing leads to any of it.
        cache.sweep(now, false, |_| false, |_| false);
        assert_eq!(cache.total_records(), 0);
    }
}
