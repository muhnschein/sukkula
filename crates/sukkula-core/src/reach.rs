//! S7: who the LAN protocols answer, and how often.
//!
//! LocalSend and Quick Share listen on the LAN, and a listener on the LAN is
//! a listener on whatever network the phone happens to be on. Only private,
//! link-local and unique-local peers are answered ([`ReachPolicy::permits`]);
//! a peer arriving from a public address -- a carrier-grade NAT, a
//! misconfigured hotspot, an IPv6 prefix routed from anywhere -- is dropped
//! before a byte of it is parsed. Discovery replies and offers are
//! additionally rate-limited per source ([`RateLimiter`]).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use crate::limits::RATE_LIMIT_ENTRIES;

/// Which peer addresses are answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReachPolicy {
    /// Also answer loopback. Tests only: on a phone, loopback is other apps.
    pub allow_loopback: bool,
}

impl ReachPolicy {
    /// Whether a peer at `ip` may be answered.
    #[must_use]
    pub fn permits(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self.permits_v4(v4),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => self.permits_v4(v4),
                None => self.permits_v6(v6),
            },
        }
    }

    fn permits_v4(&self, ip: Ipv4Addr) -> bool {
        if ip.is_loopback() {
            return self.allow_loopback;
        }
        // RFC 1918 and RFC 3927. Not 100.64/10: carrier-grade NAT is shared
        // with strangers by design.
        ip.is_private() || ip.is_link_local()
    }

    fn permits_v6(&self, ip: Ipv6Addr) -> bool {
        if ip.is_loopback() {
            return self.allow_loopback;
        }
        let first = ip.segments().first().copied().unwrap_or(0);
        // fc00::/7 unique local, fe80::/10 link-local.
        (first & 0xFE00) == 0xFC00 || (first & 0xFFC0) == 0xFE80
    }
}

/// A token bucket per source address, with bounded memory.
#[derive(Debug)]
pub struct RateLimiter {
    burst: u32,
    window: Duration,
    buckets: HashMap<IpAddr, Bucket>,
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: u32,
    refilled: Instant,
}

impl RateLimiter {
    /// Allows `burst` events per `window` per address.
    #[must_use]
    pub fn new(burst: u32, window: Duration) -> Self {
        RateLimiter {
            burst: burst.max(1),
            window,
            buckets: HashMap::new(),
        }
    }

    /// Takes one token for `ip`. False means drop the request.
    pub fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        if !self.buckets.contains_key(&ip) && self.buckets.len() >= RATE_LIMIT_ENTRIES {
            self.evict(now);
        }
        let burst = self.burst;
        let window = self.window;
        let bucket = self.buckets.entry(ip).or_insert(Bucket {
            tokens: burst,
            refilled: now,
        });
        let elapsed = now.saturating_duration_since(bucket.refilled);
        if elapsed >= window {
            bucket.tokens = burst;
            bucket.refilled = now;
        }
        if bucket.tokens == 0 {
            return false;
        }
        bucket.tokens = bucket.tokens.saturating_sub(1);
        true
    }

    /// How many addresses are remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether no address is remembered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    fn evict(&mut self, now: Instant) {
        let window = self.window;
        // An entry whose window is over would be refilled on its next use
        // anyway; forgetting it changes nothing.
        self.buckets
            .retain(|_, b| now.saturating_duration_since(b.refilled) < window);
        if self.buckets.len() >= RATE_LIMIT_ENTRIES {
            // Everyone is fresh: forget the address with the most tokens
            // left, the stalest among equals. Forgetting an address only
            // hands it a full bucket, which costs least for one that had
            // most of its bucket anyway. The address being refused right
            // now has none left, so it is forgotten last: a flood of new
            // (spoofed) sources cannot launder it. Forgetting the stalest
            // outright, as this once did, forgot exactly that address, since
            // it was the first to arrive.
            if let Some(victim) = self
                .buckets
                .iter()
                .max_by(|(_, a), (_, b)| {
                    a.tokens
                        .cmp(&b.tokens)
                        .then_with(|| b.refilled.cmp(&a.refilled))
                })
                .map(|(ip, _)| *ip)
            {
                self.buckets.remove(&victim);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn private_link_local_and_ula_are_permitted() {
        let p = ReachPolicy::default();
        for a in [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.10",
            "169.254.3.4",
            "fe80::1",
            "fd12:3456::1",
            "fc00::1",
            "::ffff:192.168.0.2",
        ] {
            assert!(p.permits(ip(a)), "{a}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        let p = ReachPolicy::default();
        for a in [
            "8.8.8.8",
            "100.64.0.1",
            "172.32.0.1",
            "192.169.0.1",
            "127.0.0.1",
            "::1",
            "2001:db8::1",
            "::",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.167",
            "ff02::1",
            "::ffff:8.8.8.8",
            "fec0::1",
        ] {
            assert!(!p.permits(ip(a)), "{a}");
        }
    }

    #[test]
    fn loopback_only_when_asked() {
        let p = ReachPolicy {
            allow_loopback: true,
        };
        assert!(p.permits(ip("127.0.0.1")));
        assert!(p.permits(ip("::1")));
        assert!(p.permits(ip("::ffff:127.0.0.1")));
    }

    #[test]
    fn rate_limiting_refills_per_window() {
        let mut r = RateLimiter::new(2, Duration::from_secs(10));
        let t0 = Instant::now();
        let a = ip("192.168.1.2");
        assert!(r.allow(a, t0));
        assert!(r.allow(a, t0));
        assert!(!r.allow(a, t0));
        assert!(
            r.allow(ip("192.168.1.3"), t0),
            "another peer has its own bucket"
        );
        assert!(r.allow(a, t0 + Duration::from_secs(10)));
    }

    #[test]
    fn memory_is_bounded() {
        let mut r = RateLimiter::new(1, Duration::from_secs(10));
        let t0 = Instant::now();
        for i in 0..10_000u32 {
            let v4 = Ipv4Addr::from(0x0A00_0000 | i);
            r.allow(IpAddr::V4(v4), t0);
        }
        assert!(r.len() <= RATE_LIMIT_ENTRIES);
        assert!(!r.is_empty());
        assert!(RateLimiter::new(0, Duration::from_secs(1)).is_empty());
    }

    /// Before eviction preferred full buckets, an exhausted address was the
    /// first forgotten -- it was the oldest entry -- so a flood of new
    /// sources handed it a fresh bucket straight away.
    #[test]
    fn a_flood_of_new_sources_cannot_launder_an_exhausted_one() {
        let mut r = RateLimiter::new(3, Duration::from_secs(10));
        let t0 = Instant::now();
        let attacker = ip("192.168.1.66");
        while r.allow(attacker, t0) {}
        for i in 0..10_000u32 {
            let t = t0 + Duration::from_millis(u64::from(i / 1000));
            let spoofed = IpAddr::V4(Ipv4Addr::from(0x0A00_0000 | i));
            r.allow(spoofed, t);
            assert!(!r.allow(attacker, t), "after {i} spoofed sources");
            assert!(r.len() <= RATE_LIMIT_ENTRIES);
        }
        // Its window still ends on time.
        assert!(r.allow(attacker, t0 + Duration::from_secs(10)));
    }

    #[test]
    fn a_legitimate_peer_is_never_refused_by_eviction() {
        // Forgetting an address only ever hands it a full bucket.
        let mut r = RateLimiter::new(2, Duration::from_secs(10));
        let t0 = Instant::now();
        let peer = ip("fe80::1234");
        for i in 0..5_000u32 {
            let spoofed = IpAddr::V4(Ipv4Addr::from(0x0A00_0000 | i));
            r.allow(spoofed, t0);
            if i % 1000 == 0 {
                assert!(r.allow(peer, t0 + Duration::from_secs(u64::from(i / 1000) * 10)));
            }
        }
    }

    #[test]
    fn time_going_backwards_is_harmless() {
        let mut r = RateLimiter::new(1, Duration::from_secs(10));
        let t0 = Instant::now() + Duration::from_secs(100);
        let a = ip("10.0.0.1");
        assert!(r.allow(a, t0));
        assert!(!r.allow(a, t0.checked_sub(Duration::from_secs(50)).unwrap()));
    }

    #[test]
    fn range_edges() {
        let p = ReachPolicy::default();
        for (a, ok) in [
            ("9.255.255.255", false),
            ("10.0.0.0", true),
            ("10.255.255.255", true),
            ("11.0.0.0", false),
            ("172.15.255.255", false),
            ("172.16.0.0", true),
            ("172.31.255.255", true),
            ("172.32.0.0", false),
            ("192.167.255.255", false),
            ("192.168.0.0", true),
            ("192.168.255.255", true),
            ("169.253.255.255", false),
            ("169.254.0.0", true),
            ("169.254.255.255", true),
            ("169.255.0.0", false),
            ("100.127.255.255", false),
            ("192.0.2.1", false),
            ("198.18.0.1", false),
            ("240.0.0.1", false),
            ("fbff:ffff::1", false),
            ("fc00::", true),
            ("fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
            ("fe00::1", false),
            ("fe7f:ffff::1", false),
            ("fe80::", true),
            ("febf:ffff::1", true),
            ("fec0::", false),
            ("ff02::fb", false),
            // IPv4-mapped follows the IPv4 rule, both ways.
            ("::ffff:10.0.0.1", true),
            ("::ffff:169.254.1.1", true),
            ("::ffff:100.64.0.1", false),
            ("::ffff:224.0.0.167", false),
            // IPv4-compatible (deprecated), IPv4-translated, NAT64, 6to4,
            // Teredo, documentation: never on a LAN socket, all refused.
            ("::10.0.0.1", false),
            ("::192.168.1.1", false),
            ("::ffff:0:10.0.0.1", false),
            ("64:ff9b::10.0.0.1", false),
            ("2002:c0a8:101::1", false),
            ("2001:0:4136:e378::1", false),
            ("2001:db8::1", false),
            ("::2", false),
        ] {
            assert_eq!(p.permits(ip(a)), ok, "{a}");
        }
        // Loopback in every spelling, only when asked.
        let lo = ReachPolicy {
            allow_loopback: true,
        };
        for a in ["127.0.0.1", "127.255.255.254", "::1", "::ffff:127.0.0.2"] {
            assert!(!p.permits(ip(a)), "{a}");
            assert!(lo.permits(ip(a)), "{a}");
        }
        assert!(
            !lo.permits(ip("::127.0.0.1")),
            "IPv4-compatible is not loopback"
        );
        assert!(!lo.permits(ip("8.8.8.8")));
    }
}
