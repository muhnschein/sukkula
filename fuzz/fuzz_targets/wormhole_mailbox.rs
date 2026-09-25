//! The Magic Wormhole mailbox guard: every message a mailbox server -- or,
//! on the default plain `ws://`, anyone on the path -- sends before the
//! library sees it, through the guard's filter and the budget of one
//! connection (`sukkula_engine::wormhole::fuzzing::server_message`; spec
//! §7, the module docs' W1-W5).
//!
//! The input is one connection's messages, one per line. The guard is held
//! to its promise from the library's side rather than its own: each message
//! it lets through is parsed again with a restatement of magic-wormhole
//! 0.8.1's own server-message types (`core/server_messages.rs`, the same
//! serde attributes), so a message the guard reads one way and the library
//! another -- a duplicate key, a tag spelt differently -- cannot slip by.
//! Asserted of every message the library would act on:
//!
//! - a `welcome` demanding hashcash asks for at most 20 bits over a
//!   resource of at most 256 bytes (W1: the library mints it in a loop
//!   that never yields);
//! - a peer `message` has a phase that is `pake`, `version` or a number
//!   (W2: `todo!()` otherwise) and a hex body; any but `pake` holds at
//!   least a nonce and a tag, 40 bytes (W3: `split_at(24)` otherwise), and
//!   a `pake` body at most 512 bytes;
//! - read the way the library reads them -- by arrival, not by phase:
//!   echoes of its own side (`fuzzing::LIBRARY_SIDE`) and phases it has
//!   taken before are skipped, the first new peer message is the PAKE, the
//!   second is decrypted as the version message, every later one must have
//!   a numeric phase -- the message in each place is one the library
//!   cannot panic on there (a short body second, or `pake` third, would);
//! - the connection's budget holds: at most 128 messages and 4 MiB, and
//!   once the guard refuses one the connection is closed, so nothing after
//!   it is read.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use std::collections::{HashMap, HashSet};

use libfuzzer_sys::fuzz_target;
use serde::{Deserialize, Deserializer};
use sukkula_engine::wormhole::fuzzing::{
    self, HASHCASH_BITS, LIBRARY_SIDE, SERVER_BYTES, SERVER_MESSAGES, ServerBudget,
};

// ---- magic-wormhole 0.8.1, core/server_messages.rs, as it deserialises ----

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
#[serde(tag = "type")]
#[allow(dead_code)]
enum InboundMessage {
    Welcome {
        welcome: WelcomeMessage,
    },
    Nameplates {
        #[serde(deserialize_with = "nameplates")]
        nameplates: Vec<String>,
    },
    Allocated {
        nameplate: String,
    },
    Claimed {
        mailbox: String,
    },
    Released,
    Message(EncryptedMessage),
    Closed,
    Ack,
    Pong {
        pong: u64,
    },
    Error {
        error: String,
        orig: Box<serde_json::Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct Nameplate {
    id: String,
}

fn nameplates<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    let v: Vec<Nameplate> = Deserialize::deserialize(de)?;
    Ok(v.into_iter().map(|n| n.id).collect())
}

#[derive(Deserialize, Debug, Default)]
#[allow(dead_code)]
struct WelcomeMessage {
    current_cli_version: Option<String>,
    motd: Option<String>,
    error: Option<String>,
    #[serde(rename = "permission-required")]
    permission_required: Option<PermissionRequired>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct PermissionRequired {
    #[serde(deserialize_with = "none_present")]
    none: bool,
    hashcash: Option<HashcashPermission>,
    #[serde(flatten)]
    other: HashMap<String, serde_json::Value>,
}

fn none_present<'de, D: Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
    let v: Option<serde_json::Map<String, serde_json::Value>> = Deserialize::deserialize(de)?;
    Ok(v.is_some())
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct HashcashPermission {
    bits: u32,
    resource: String,
}

#[derive(Deserialize, Debug)]
struct EncryptedMessage {
    side: String,
    phase: String,
    #[serde(deserialize_with = "hex_body")]
    body: Vec<u8>,
}

/// `hex::serde::deserialize` for `Vec<u8>`: an even number of hex digits,
/// either case.
fn hex_body<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
    let s: String = Deserialize::deserialize(de)?;
    if !s.len().is_multiple_of(2) {
        return Err(serde::de::Error::custom("odd length"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            s.get(i..i + 2)
                .and_then(|p| {
                    u8::from_str_radix(p, 16)
                        .ok()
                        .filter(|_| p.bytes().all(|b| b.is_ascii_hexdigit()))
                })
                .ok_or_else(|| serde::de::Error::custom("not hex"))
        })
        .collect()
}

// ---------------------------------------------------------------- the target

/// A nonce and a Poly1305 tag: the least a sealed body holds.
const MIN_SEALED_BYTES: usize = 24 + 16;

/// magic-wormhole 0.8.1 reading peer messages (`core.rs` `connect` and
/// `receive`, `core/rendezvous.rs` `MailboxMachine::receive_message`):
/// what it has taken, by phase alone, whatever the side.
#[derive(Default)]
struct Library {
    processed: HashSet<String>,
    taken: usize,
}

impl Library {
    /// The library's use of `msg`, asserting that it reaches none of its
    /// panics where it reads it.
    fn read(&mut self, msg: &EncryptedMessage) {
        // An echo of ours, or a phase already taken: skipped unread.
        if msg.side == LIBRARY_SIDE || !self.processed.insert(msg.phase.clone()) {
            return;
        }
        let place = self.taken;
        self.taken += 1;
        let bytes = msg.body.len();
        match place {
            // The PAKE: parsed as JSON, then SPAKE2; errors only.
            0 => assert!(bytes <= 512, "a {bytes}-byte body taken as the PAKE"),
            // The version message: `decrypt`, so `split_at(24)` (W3).
            1 => assert!(
                bytes >= MIN_SEALED_BYTES,
                "a {bytes}-byte body taken as the version message (W3)"
            ),
            // `receive`: `todo!()` on a phase that is not a number (W2),
            // then `decrypt`.
            _ => {
                assert!(
                    msg.phase.parse::<u64>().is_ok(),
                    "phase {:?} reaches todo!() in receive (W2)",
                    msg.phase
                );
                assert!(
                    bytes >= MIN_SEALED_BYTES,
                    "a {bytes}-byte body reaches split_at in receive (W3)"
                );
            }
        }
    }
}

fn library_safe(text: &str, library: &mut Library) {
    let Ok(m) = serde_json::from_str::<InboundMessage>(text) else {
        // The library refuses it too: an error, not a hang or a panic.
        return;
    };
    match m {
        InboundMessage::Welcome { welcome } => {
            if let Some(h) = welcome.permission_required.and_then(|p| p.hashcash) {
                assert!(
                    u64::from(h.bits) <= HASHCASH_BITS,
                    "hashcash of {} bits let through",
                    h.bits
                );
                assert!(
                    h.resource.len() <= 256,
                    "a hashcash resource of {} bytes",
                    h.resource.len()
                );
            }
        }
        InboundMessage::Message(msg) => {
            match msg.phase.as_str() {
                "pake" => assert!(msg.body.len() <= 512, "a {}-byte PAKE body", msg.body.len()),
                "version" => {
                    assert!(msg.body.len() >= MIN_SEALED_BYTES, "a short version body");
                }
                phase => {
                    assert!(
                        phase.parse::<u64>().is_ok(),
                        "phase {phase:?} would reach todo!()"
                    );
                    assert!(
                        msg.body.len() >= MIN_SEALED_BYTES,
                        "a {}-byte body would reach split_at",
                        msg.body.len()
                    );
                }
            }
            library.read(&msg);
        }
        _ => {}
    }
}

fuzz_target!(|data: &[u8]| {
    let mut budget = ServerBudget::default();
    let mut library = Library::default();
    let mut passed = 0usize;
    let mut bytes = 0usize;
    for line in data.split(|b| *b == b'\n') {
        // WebSocket text messages are UTF-8; tungstenite checks that first.
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        if fuzzing::server_message(text, &mut budget).is_err() {
            // The guard closes the connection: nothing further is read.
            return;
        }
        passed += 1;
        bytes += text.len();
        assert!(
            passed <= SERVER_MESSAGES,
            "{passed} messages on one connection"
        );
        assert!(bytes <= SERVER_BYTES, "{bytes} bytes on one connection");
        let v: serde_json::Value = serde_json::from_str(text).expect("let through, so JSON");
        assert!(v["type"].is_string(), "let through without a type");
        library_safe(text, &mut library);
    }
});
