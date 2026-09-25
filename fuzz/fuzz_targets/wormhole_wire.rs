//! Magic Wormhole's v1 transfer messages from the peer -- offers, transit
//! hints, answers, the final transit ack -- through the adapter's parser,
//! its conversion to the offer the user is asked about, `Offer::validate`,
//! and the plan of where to connect that the peer's hints make
//! (`sukkula_engine::wormhole::fuzzing`; spec §7 "wormhole offers").
//!
//! Asserted, against the JSON read independently through
//! `serde_json::Value`:
//!
//! - an offer is exactly what the peer said: one text, or one file (a
//!   folder as its `.zip`) with the size it declared, kept signed and
//!   exact; what validates keeps every S-rule (`sukkula_fuzz::assert_offer`);
//!   an offer re-encoded parses to itself;
//! - transit hints are bounded -- at most 16 direct, 2 relays of at most 3
//!   endpoints -- deduplicated, and name only IP literals (direct) or DNS
//!   names (relays), never port 0;
//! - the plan tries at most 4 targets, our own relay first, a peer relay
//!   only if the peer offered relaying and it is not ours, and never makes
//!   us connect to loopback, an unspecified, multicast or broadcast
//!   address, however it is spelt (S7's spirit for a peer that picks the
//!   address, spec §5);
//! - a transit ack matches only `{"ack": "ok", "sha256": <that digest>}`.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use std::net::IpAddr;

use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use sukkula_core::Protocol;
use sukkula_core::offer::Offer;
use sukkula_engine::wormhole::PEER_LABEL;
use sukkula_engine::wormhole::fuzzing::{
    self, DIRECT_HINTS, PeerMessage, RELAY_HINTS, TARGETS, Target, Transit,
};
use sukkula_fuzz::assert_offer;

/// The digest a transit ack is checked against.
const DIGEST: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

/// The default relay, which is ours unless the settings say otherwise.
const OUR_RELAY: (&str, u16) = ("transit.magic-wormhole.io", 4001);

fn is_dns_name(h: &str) -> bool {
    h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Addresses a peer must never make us connect to.
fn forbidden(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast()
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || v6.is_multicast(),
    }
}

fn check_transit(t: &Transit) {
    assert!(
        t.direct_hints.len() <= DIRECT_HINTS,
        "{} direct hints",
        t.direct_hints.len()
    );
    for (i, h) in t.direct_hints.iter().enumerate() {
        assert_ne!(h.1, 0, "a direct hint on port 0");
        assert!(!t.direct_hints[..i].contains(h), "a direct hint twice");
    }
    assert!(t.relays.len() <= RELAY_HINTS, "{} relays", t.relays.len());
    for r in &t.relays {
        assert!(
            !r.is_empty() && r.len() <= 3,
            "a relay with {} endpoints",
            r.len()
        );
        for (i, (host, port)) in r.iter().enumerate() {
            assert_ne!(*port, 0, "a relay on port 0");
            assert!(
                host.parse::<IpAddr>().is_ok() || is_dns_name(host),
                "relay host {host:?}"
            );
            assert!(
                !r[..i].contains(&(host.clone(), *port)),
                "a relay endpoint twice"
            );
        }
    }

    assert!(t.targets.len() <= TARGETS, "{} targets", t.targets.len());
    let ours = vec![(OUR_RELAY.0.to_owned(), OUR_RELAY.1)];
    assert_eq!(
        t.targets.first(),
        Some(&Target::Relay {
            endpoints: ours.clone(),
            ours: true
        }),
        "our relay is not tried first"
    );
    let mut theirs = 0;
    let mut seen_public = false;
    for target in &t.targets[1..] {
        match target {
            Target::Relay {
                endpoints,
                ours: false,
            } => {
                theirs += 1;
                assert!(t.relay, "a peer relay the peer never offered");
                assert!(t.relays.contains(endpoints), "a relay the peer never named");
                assert!(!endpoints.contains(&ours[0]), "our relay twice");
            }
            Target::Relay { ours: true, .. } => panic!("our relay twice"),
            Target::Direct(addr) => {
                assert!(t.direct, "a direct connection the peer never offered");
                assert!(
                    t.direct_hints.contains(&(addr.ip(), addr.port())),
                    "a direct target the peer never named"
                );
                assert!(!forbidden(addr.ip()), "made to connect to {addr}");
                // Private addresses first: they are likely the same LAN.
                let local = match addr.ip() {
                    IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
                    IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
                };
                if !local {
                    seen_public = true;
                } else {
                    assert!(!seen_public, "a private address after a public one");
                }
            }
        }
    }
    assert!(theirs <= 1, "{theirs} peer relays");
}

fn check_offer(raw: &sukkula_core::offer::RawOffer, v: Option<&Value>) {
    assert_eq!(raw.protocol, Protocol::Wormhole);
    assert_eq!(raw.sender, PEER_LABEL);
    assert_eq!(raw.model, None);
    assert_eq!(raw.pin, None);
    assert!(
        (raw.text.is_some() && raw.files.is_empty())
            || (raw.text.is_none() && raw.files.len() == 1),
        "not one text or one file"
    );
    if let Some(v) = v {
        let offer = &v["offer"];
        if let Some(t) = &raw.text {
            assert_eq!(offer["message"].as_str(), Some(t.as_str()));
        } else {
            let f = &raw.files[0];
            let (name, size) = if offer.get("file").is_some() {
                (
                    offer["file"]["filename"].as_str().map(str::to_owned),
                    &offer["file"]["filesize"],
                )
            } else {
                (
                    offer["directory"]["dirname"]
                        .as_str()
                        .map(|d| format!("{d}.zip")),
                    &offer["directory"]["zipsize"],
                )
            };
            assert_eq!(name.as_deref(), Some(f.name.as_str()), "the name changed");
            if let Some(n) = size.as_i64() {
                assert_eq!(f.size, i128::from(n), "the size changed");
            } else if let Some(n) = size.as_u64() {
                assert_eq!(f.size, i128::from(n), "the size changed");
            } else {
                // Only a float beyond any 64-bit integer is taken, and then
                // as the far end of the range, so it is refused for its size.
                assert!(
                    f.size == i128::MAX || f.size == i128::MIN,
                    "size {}",
                    f.size
                );
            }
        }
    }
    // What the peer said again, as a sender writes it, parses to the same
    // (a folder's `.zip` as the file it is).
    let again = match (&raw.text, raw.files.first()) {
        (Some(t), _) => Some(json!({"offer": {"message": t}})),
        (None, Some(f)) => u64::try_from(f.size)
            .ok()
            .map(|s| json!({"offer": {"file": {"filename": f.name, "filesize": s}}})),
        (None, None) => None,
    };
    if let Some(again) = again {
        let bytes = serde_json::to_vec(&again).expect("serialises");
        match fuzzing::peer_message(&bytes) {
            Ok(PeerMessage::Offer(Some(r))) => assert_eq!(&r, raw, "re-encoding changed the offer"),
            other => panic!("a re-encoded offer parsed as {other:?}"),
        }
    }
    let size = raw.files.first().map(|f| f.size);
    if let Ok(o) = Offer::validate(raw.clone()) {
        assert_offer(&o);
        assert_eq!(size.map(|_| i128::from(o.files[0].size)), size);
    }
}

fuzz_target!(|data: &[u8]| {
    if fuzzing::transit_ack_matches(data, DIGEST) {
        let v: Value = serde_json::from_slice(data).expect("a matching ack is JSON");
        assert_eq!(v["ack"], "ok");
        assert!(
            v["sha256"]
                .as_str()
                .is_some_and(|h| h.eq_ignore_ascii_case(DIGEST)),
            "an ack for another digest matched"
        );
    }
    let Ok(msg) = fuzzing::peer_message(data) else {
        return;
    };
    // `Value` refuses numbers that overflow an f64, which the parser takes
    // as the far end of the range; only what `Value` can hold is compared.
    let v = serde_json::from_slice::<Value>(data).ok();
    if let Some(v) = &v {
        let o = v.as_object().expect("a message is an object");
        assert_eq!(o.len(), 1, "a message with {} keys", o.len());
    }
    match &msg {
        PeerMessage::Transit(t) => check_transit(t),
        PeerMessage::Offer(Some(raw)) => check_offer(raw, v.as_ref()),
        PeerMessage::Offer(None) => {}
        PeerMessage::MessageAck(ok) | PeerMessage::FileAck(ok) => {
            if let Some(v) = &v {
                let ack = v["answer"].as_object().expect("an answer is an object");
                assert_eq!(ack.len(), 1);
                assert_eq!(*ok, ack.values().all(|a| a == "ok"));
            }
        }
        PeerMessage::OtherAnswer | PeerMessage::Error | PeerMessage::Other => {}
    }
    // Parsing is a function of the bytes.
    assert_eq!(fuzzing::peer_message(data).ok().as_ref(), Some(&msg));
});
