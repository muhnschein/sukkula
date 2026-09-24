//! Hostile-input sweep over every parser in this module, after clove's
//! `tests/hostile.rs`: a valid sample of each input is damaged thousands of
//! ways with a deterministic PRNG and fed back in. The contract is to parse
//! or refuse -- never panic -- and whatever is accepted must still keep the
//! promise its checker makes: a server message the mailbox guard lets
//! through has a phase and a body the library cannot panic on, a peer
//! message that parses re-encodes to itself, a code that parses fits the
//! strict grammar.
//!
//! Deterministic on purpose: a failure reproduces from its seed.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::as_conversions
)]

use proptest::prelude::*;
use serde_json::{Value, json};

use super::code;
use super::mailbox::{Budget, check_server_message};
use super::wire::{self, OfferMsg, PeerMsg};

/// Mutations per seed.
const ROUNDS: usize = 3_000;

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }
}

/// Damages `input` the ways a hostile source would: a flipped bit, an
/// interesting byte, a cut, an insertion, a spliced-out or doubled run.
fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    const INTERESTING: [u8; 16] = [
        0x00, 0x01, 0x7F, 0x80, 0xFF, b'0', b'9', b'-', b'"', b'\\', b'{', b'}', b'[', b']', b':',
        b'e',
    ];
    let mut out = input.to_vec();
    for _ in 0..=rng.below(3) {
        match rng.below(6) {
            0 if !out.is_empty() => {
                let at = rng.below(out.len());
                out[at] ^= 1 << rng.below(8);
            }
            1 if !out.is_empty() => {
                let at = rng.below(out.len());
                out[at] = INTERESTING[rng.below(INTERESTING.len())];
            }
            2 if !out.is_empty() => {
                let keep = rng.below(out.len());
                out.truncate(keep);
            }
            3 => {
                let at = rng.below(out.len() + 1);
                out.insert(at, INTERESTING[rng.below(INTERESTING.len())]);
            }
            4 if out.len() > 2 => {
                let a = rng.below(out.len());
                let b = (a + rng.below(16)).min(out.len());
                out.drain(a..b);
            }
            _ if !out.is_empty() => {
                let a = rng.below(out.len());
                let b = (a + rng.below(16)).min(out.len());
                let run = out[a..b].to_vec();
                out.splice(a..a, run);
            }
            _ => out.push(INTERESTING[rng.below(INTERESTING.len())]),
        }
    }
    out
}

fn peer_seeds() -> Vec<Vec<u8>> {
    [
        json!({"transit": {"abilities-v1": [{"type": "direct-tcp-v1"}, {"type": "relay-v1"}],
            "hints-v1": [{"type": "direct-tcp-v1", "hostname": "192.168.1.5", "port": 4001, "priority": 0.5},
                {"type": "relay-v1", "name": "r", "hints": [{"type": "direct-tcp-v1", "hostname": "transit.example", "port": 4001}]}]}}),
        json!({"offer": {"file": {"filename": "photo.jpg", "filesize": 123_456}}}),
        json!({"offer": {"directory": {"dirname": "d", "mode": "zipfile/deflated", "zipsize": 10, "numbytes": 20, "numfiles": 2}}}),
        json!({"offer": {"message": "hello"}}),
        json!({"answer": {"file_ack": "ok"}}),
        json!({"answer": {"message_ack": "ok"}}),
        json!({"error": "transfer rejected"}),
    ]
    .iter()
    .map(|v| serde_json::to_vec(v).unwrap())
    .collect()
}

fn server_seeds() -> Vec<Vec<u8>> {
    let body = "ab".repeat(48);
    [
        json!({"type": "welcome", "welcome": {"motd": "hi", "permission-required": {"none": null, "hashcash": {"bits": 6, "resource": "r"}}}}),
        json!({"type": "message", "side": "0123456789", "phase": "0", "body": body, "id": "1"}),
        json!({"type": "message", "side": "0123456789", "phase": "version", "body": body}),
        json!({"type": "message", "side": "0123456789", "phase": "pake", "body": "7b7d"}),
        json!({"type": "nameplates", "nameplates": [{"id": "7"}]}),
        json!({"type": "claimed", "mailbox": "m"}),
        json!({"type": "ack", "id": null}),
    ]
    .iter()
    .map(|v| serde_json::to_vec(v).unwrap())
    .collect()
}

/// What `check_server_message` promises for a message it lets through.
fn assert_library_safe(text: &str) {
    let v: Value = serde_json::from_str(text).expect("accepted, so JSON");
    if v.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    let phase = v["phase"].as_str().expect("a phase");
    let body = v["body"].as_str().expect("a body");
    assert!(body.len().is_multiple_of(2) && body.bytes().all(|b| b.is_ascii_hexdigit()));
    match phase {
        "pake" => {}
        "version" => assert!(body.len() >= 80),
        n => {
            assert!(n.parse::<u64>().is_ok(), "phase {n:?} would reach todo!()");
            assert!(body.len() >= 80, "body would reach split_at");
        }
    }
    if let Some(hc) = v.pointer("/welcome/permission-required/hashcash") {
        assert!(hc["bits"].as_u64().is_some_and(|b| b <= 20));
    }
}

#[test]
fn peer_messages_survive_mutation() {
    for (i, seed) in peer_seeds().iter().enumerate() {
        let mut rng = Rng(0x5EED_0000 + i as u64);
        for _ in 0..ROUNDS {
            let m = mutate(&mut rng, seed);
            if let Ok(PeerMsg::Offer(OfferMsg::File { name, size })) = wire::parse(&m) {
                // What parsed re-encodes to the same offer.
                if let Ok(size) = u64::try_from(size) {
                    let again = wire::parse(&wire::offer_file(&name, size)).unwrap();
                    assert_eq!(
                        again,
                        PeerMsg::Offer(OfferMsg::File {
                            name,
                            size: i128::from(size)
                        })
                    );
                }
            }
            let _ = wire::transit_ack_matches(&m, "ab");
        }
    }
}

#[test]
fn server_messages_survive_mutation_and_keep_the_guards_promise() {
    for (i, seed) in server_seeds().iter().enumerate() {
        let mut rng = Rng(0xFACE_0000 + i as u64);
        for _ in 0..ROUNDS {
            let m = mutate(&mut rng, seed);
            let Ok(text) = String::from_utf8(m) else {
                continue;
            };
            if check_server_message(&text, &mut Budget::default()).is_ok() {
                assert_library_safe(&text);
            }
        }
    }
}

#[test]
fn codes_survive_mutation_and_keep_the_grammar() {
    let seeds = ["7-guitarist-revenge", "123-aardvark-adroitness-crucial"];
    for (i, seed) in seeds.iter().enumerate() {
        let mut rng = Rng(0xC0DE_0000 + i as u64);
        for _ in 0..ROUNDS {
            let m = mutate(&mut rng, seed.as_bytes());
            let Ok(text) = String::from_utf8(m) else {
                continue;
            };
            if let Ok(c) = code::parse(&text) {
                let s = c.to_string();
                let (n, words) = s.split_once('-').unwrap();
                assert!(!n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()));
                assert!(words.split('-').all(|w| {
                    !w.is_empty()
                        && w.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                }));
            }
        }
    }
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = wire::parse(&bytes);
        let _ = wire::transit_ack_matches(&bytes, "00");
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let _ = code::parse(s);
            if check_server_message(s, &mut Budget::default()).is_ok() {
                assert_library_safe(s);
            }
        }
    }

    #[test]
    fn any_json_size_is_kept_or_refused(n in any::<i64>(), f in any::<f64>()) {
        prop_assert_eq!(wire::declared_size(&json!(n)), Some(i128::from(n)));
        if let Some(s) = wire::declared_size(&json!(f)) {
            // Only integral floats beyond any 64-bit integer are kept, and
            // then only as the far end of the range.
            prop_assert!(s == i128::MAX || s == i128::MIN);
        }
    }
}
