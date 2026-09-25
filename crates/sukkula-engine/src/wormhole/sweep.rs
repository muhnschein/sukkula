//! Hostile-input sweep over every parser in this module, after clove's
//! `tests/hostile.rs`: a valid sample of each input is damaged thousands of
//! ways with a deterministic PRNG and fed back in. The contract is to parse
//! or refuse -- never panic -- and whatever is accepted must still keep the
//! promise its checker makes: what the mailbox guard lets through, read the
//! way the library reads it -- in arrival order, not by phase -- reaches
//! none of its panics, a peer message that parses re-encodes to itself, a
//! code that parses fits the strict grammar.
//!
//! Deterministic on purpose: a failure reproduces from its seed.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::as_conversions
)]

use std::collections::HashSet;

use proptest::prelude::*;
use serde_json::{Value, json};

use super::code;
use super::mailbox::{Conversation, check_server_message};
use super::wire::{self, OfferMsg, PeerMsg};

/// The side the library binds as in these sweeps.
const OURS: &str = "0a1b2c3d4e";

/// A connection on which the library has bound as [`OURS`].
fn bound() -> Conversation {
    let mut c = Conversation::default();
    c.note_library_message(&json!({"type": "bind", "appid": "a", "side": OURS}).to_string());
    c
}

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

/// magic-wormhole 0.8.1 reading what the guard lets through, restated from
/// `core.rs` and `core/rendezvous.rs` rather than from the guard: it skips
/// echoes of its own side and any phase it has taken before (by phase
/// alone, whatever the side); it takes the first new peer message as the
/// PAKE (parsed as JSON, which cannot panic), the second as the version
/// message (`decrypt`, so `split_at(24)`), and each later one in `receive`
/// (`todo!()` on a phase that is not a `u64`, then `decrypt`). A welcome's
/// hashcash is minted in a loop that never yields.
#[derive(Default)]
struct Library {
    processed: HashSet<String>,
    taken: usize,
}

/// A nonce: what `decrypt_data` splits off without looking.
const NONCE_BYTES: usize = 24;

impl Library {
    /// The library with `prefix` already read.
    fn after(prefix: &[String]) -> Library {
        let mut l = Library::default();
        for m in prefix {
            l.read(m);
        }
        l
    }

    /// Reads `text` as the library would, asserting that it reaches none
    /// of the library's panics and no unbounded mint.
    fn read(&mut self, text: &str) {
        let v: Value = serde_json::from_str(text).expect("let through, so JSON");
        match v.get("type").and_then(Value::as_str) {
            Some("welcome") => {
                let hc = v.pointer("/welcome/permission-required/hashcash");
                if let Some(hc) = hc.filter(|h| !h.is_null()) {
                    assert!(hc["bits"].as_u64().is_some_and(|b| b <= 20), "{text}");
                    assert!(hc["resource"].as_str().is_some_and(|r| r.len() <= 256));
                }
                return;
            }
            Some("message") => {}
            _ => return,
        }
        let side = v["side"].as_str().expect("a side");
        let phase = v["phase"].as_str().expect("a phase");
        let body = v["body"].as_str().expect("a body");
        assert!(body.len().is_multiple_of(2) && body.bytes().all(|b| b.is_ascii_hexdigit()));
        if side == OURS || !self.processed.insert(phase.to_owned()) {
            return;
        }
        let place = self.taken;
        self.taken += 1;
        let bytes = body.len() / 2;
        match place {
            0 => {}
            1 => assert!(
                bytes >= NONCE_BYTES,
                "a {bytes}-byte body taken as the version message reaches split_at (W3)"
            ),
            _ => {
                assert!(
                    phase.parse::<u64>().is_ok(),
                    "phase {phase:?} reaches todo!() in receive (W2)"
                );
                assert!(
                    bytes >= NONCE_BYTES,
                    "a {bytes}-byte body reaches split_at (W3)"
                );
            }
        }
    }
}

fn peer(phase: &str, body: &str) -> String {
    json!({"type": "message", "side": "0123456789", "phase": phase, "body": body}).to_string()
}

/// Where in the key exchange a single message is tried: before the PAKE,
/// before the version message, and after both.
fn prefixes() -> Vec<Vec<String>> {
    let sealed = "ab".repeat(48);
    vec![
        vec![],
        vec![peer("pake", "7b7d")],
        vec![peer("pake", "7b7d"), peer("version", &sealed)],
    ]
}

/// Feeds `messages` to the guard and, for as long as it lets them through,
/// to the library; the guard closes the connection at its first refusal.
fn converse(messages: &[String]) -> usize {
    let mut guard = bound();
    let mut library = Library::default();
    for (n, m) in messages.iter().enumerate() {
        if check_server_message(m, &mut guard).is_err() {
            return n;
        }
        library.read(m);
    }
    messages.len()
}

/// Every message the guard lets through in any of the [`prefixes`]
/// positions is safe for the library there.
fn keeps_the_promise_anywhere(text: &str) {
    for prefix in prefixes() {
        let mut guard = bound();
        for m in &prefix {
            check_server_message(m, &mut guard).unwrap();
        }
        if check_server_message(text, &mut guard).is_ok() {
            Library::after(&prefix).read(text);
        }
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
            keeps_the_promise_anywhere(&text);
        }
    }
}

/// A whole exchange as the library sees it on the receiving side: our
/// echoes in between, the sender's transit and offer after the key
/// exchange.
fn exchange() -> Vec<String> {
    let sealed = "ab".repeat(48);
    let pake = sukkula_core::hex::encode(
        format!(
            r#"{{"pake_v1":"53{}"}}"#,
            "58".to_owned() + &"66".repeat(31)
        )
        .as_bytes(),
    );
    let echo = |phase: &str, body: &str| {
        json!({"type": "message", "side": OURS, "phase": phase, "body": body}).to_string()
    };
    vec![
        json!({"type": "welcome", "welcome": {}}).to_string(),
        json!({"type": "ack", "id": null}).to_string(),
        peer("pake", &pake),
        echo("pake", &pake),
        echo("version", &sealed),
        peer("version", &sealed),
        peer("0", &sealed),
        peer("1", &sealed),
        echo("0", &sealed),
        peer("2", &sealed),
    ]
}

#[test]
fn an_honest_exchange_passes_the_guard_whole() {
    let x = exchange();
    assert_eq!(converse(&x), x.len());
}

#[test]
fn reordered_repeated_and_respoken_exchanges_keep_the_guards_promise() {
    // The guard is judged on sequences, as the library reads them: the
    // honest exchange with messages swapped, repeated, dropped, moved to
    // another side or phase, or cut short, several at a time.
    let base = exchange();
    let phases = ["pake", "version", "0", "1", "7", "dilate-1"];
    let mut rng = Rng(0x0DE5_0000);
    let mut refused_early = 0usize;
    for _ in 0..ROUNDS {
        let mut msgs: Vec<Value> = base
            .iter()
            .map(|m| serde_json::from_str(m).unwrap())
            .collect();
        for _ in 0..=rng.below(3) {
            let n = msgs.len();
            let (a, b) = (rng.below(n), rng.below(n));
            match rng.below(6) {
                0 => msgs.swap(a, b),
                1 => {
                    let m = msgs[a].clone();
                    msgs.insert(b, m);
                }
                2 if n > 1 => {
                    msgs.remove(a);
                }
                3 => {
                    let side = ["0123456789", OURS, "third"][rng.below(3)];
                    msgs[a]["side"] = json!(side);
                }
                4 => msgs[a]["phase"] = json!(phases[rng.below(phases.len())]),
                _ => {
                    if let Some(body) = msgs[a]["body"].as_str() {
                        let keep = rng.below(body.len() / 2 + 1) * 2;
                        msgs[a]["body"] = json!(body.get(..keep).unwrap_or_default().to_owned());
                    }
                }
            }
        }
        let texts: Vec<String> = msgs.iter().map(Value::to_string).collect();
        if converse(&texts) < texts.len() {
            refused_early += 1;
        }
    }
    // The sweep reaches refusals, not only honest runs.
    assert!(refused_early > ROUNDS / 4, "{refused_early}");
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
            keeps_the_promise_anywhere(s);
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
