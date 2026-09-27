//! croc's control messages: raw DEFLATE of a small JSON object, sealed
//! once there is a key, and the LAN probe's plain JSON
//! (`sukkula_engine::croc::fuzzing::{control, sealed_control, inflate}`).
//!
//! The relay passes these on, so before the key they are anyone's, and
//! after it the relay can still drop, reorder or replay them. Asserted,
//! against the frame read independently here:
//!
//! - nothing is read from a frame over 1 MiB, nor inflated past 1 MiB,
//!   and a stream is inflated only whole and under its cap -- a deflate
//!   bomb stops at the cap;
//! - an unsealed message is exactly its JSON: `t` names its kind (one of
//!   croc's nine, anything else ignored as Go ignores it), `v` its PAKE
//!   version (0 if absent), `m` its text, `b` and `b2` standard base64 of
//!   its bytes;
//! - under a key the fuzzer does not have, nothing opens;
//! - sealed under the key, any message reads back as it reads unsealed:
//!   the seal hides nothing and adds nothing;
//! - the LAN probe's message is taken only under 16 KiB, with its `Kind`,
//!   `Bytes` and `Version` as the JSON says.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use sukkula_engine::croc::fuzzing::{self, CONTROL_BYTES, PAKE_BYTES, Read};
use sukkula_fuzz::{as_struct, value_of};

/// croc's message kinds, and the engine's names for them.
const KINDS: [(&str, &str); 9] = [
    ("pake", "Pake"),
    ("pake-confirm", "PakeConfirm"),
    ("externalip", "ExternalIp"),
    ("fileinfo", "FileInfo"),
    ("recipientready", "RecipientReady"),
    ("close-sender", "CloseSender"),
    ("close-recipient", "CloseRecipient"),
    ("finished", "Finished"),
    ("error", "Error"),
];

/// A field that is base64 if present: its bytes, empty if absent or null.
fn bytes_of(v: &Value, key: &str) -> Vec<u8> {
    match v.get(key) {
        None | Some(Value::Null) => Vec::new(),
        Some(s) => STANDARD
            .decode(s.as_str().expect("read a non-string as base64"))
            .expect("read bad base64"),
    }
}

/// An unsealed message, restated from its inflated JSON.
fn restated(json: &[u8]) -> Option<Read> {
    let v = value_of(json, "a message")?;
    let v = as_struct(v, &["t", "v", "m", "b", "b2"]);
    let t = v
        .get("t")
        .and_then(Value::as_str)
        .expect("read a message without t");
    let kind = KINDS
        .iter()
        .find(|(wire, _)| *wire == t)
        .map_or("Other", |(_, ours)| ours);
    let m = match v.get("m") {
        None | Some(Value::Null) => String::new(),
        Some(m) => m.as_str().expect("read a non-string m").to_owned(),
    };
    let version = match v.get("v") {
        None => 0,
        Some(n) => n.as_i64().expect("read a v that is no integer"),
    };
    Some((
        kind.to_owned(),
        version,
        m,
        bytes_of(&v, "b"),
        bytes_of(&v, "b2"),
    ))
}

fuzz_target!(|data: &[u8]| {
    let read = fuzzing::control(data);
    assert!(read.sealed.is_none(), "a frame opened without the key");
    let inflated = fuzzing::inflate(data, CONTROL_BYTES);
    if let Some(i) = &inflated {
        assert!(i.len() <= CONTROL_BYTES, "inflated to {} bytes", i.len());
        // Whole and under its cap: a smaller cap takes it or nothing.
        let half = i.len() / 2;
        if let Some(h) = fuzzing::inflate(data, half) {
            assert!(h.len() <= half, "a cap of {half} gave {}", h.len());
        }
    }
    if let Some(plain) = &read.plain {
        assert!(
            data.len() <= CONTROL_BYTES,
            "a {} byte frame read",
            data.len()
        );
        let json = inflated
            .as_deref()
            .expect("read a frame that does not inflate");
        if let Some(r) = restated(json) {
            assert_eq!(*plain, r, "the message is not its JSON");
        }
    }
    if let Some((kind, bytes, version)) = &read.probe {
        assert!(
            data.len() <= 2 * PAKE_BYTES,
            "a {} byte probe read",
            data.len()
        );
        if let Some(v) = value_of(data, "a probe") {
            let v = as_struct(v, &["Bytes", "Kind", "Version"]);
            assert_eq!(v.get("Kind").and_then(Value::as_str), Some(kind.as_str()));
            assert_eq!(*bytes, bytes_of(&v, "Bytes"), "the probe's bytes");
            let expected = v.get("Version").map_or(Some(0), Value::as_i64);
            assert_eq!(Some(*version), expected, "the probe's version");
        }
    }
    // The input as a message's JSON, sealed and read back.
    let packed = fuzzing::deflate(data);
    assert_eq!(
        fuzzing::inflate(&packed, CONTROL_BYTES).as_deref(),
        Some(data),
        "deflate does not round trip"
    );
    assert_eq!(
        fuzzing::sealed_control(data),
        fuzzing::control(&packed).plain,
        "sealing changed the message"
    );
});
