//! croc's PAKE messages -- from the relay, on every connection, and from
//! the peer, before there is a key -- answered on each curve spoken, and
//! taken as the answer to our own first message
//! (`sukkula_engine::croc::fuzzing::pake`).
//!
//! These are the only bytes read before anything is sealed, from anyone
//! who can reach the relay. Asserted, against the message read
//! independently as a JSON object of raw values (a coordinate is 78
//! digits, more than `serde_json::Value` holds exactly): an answer is made
//! only to a message of at most 8 KiB whose `Role` is 0 and whose `Xᵤ` and
//! `Xᵥ` are unsigned decimal integers of at most 78 digits (the point it
//! sends, which must then be on the curve); the curve a message names is
//! one of the two spoken, and only if it carries a `Uᵤ`.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use serde_json::value::RawValue;
use sukkula_engine::croc::fuzzing::{self, PAKE_BYTES};

type Fields = HashMap<String, Box<RawValue>>;

/// The message's fields by name: an object, or serde's array form of the
/// engine's struct, in its order.
fn fields(data: &[u8]) -> Option<Fields> {
    if let Ok(map) = serde_json::from_slice::<Fields>(data) {
        return Some(map);
    }
    let items: Vec<Box<RawValue>> = serde_json::from_slice(data).ok()?;
    let names = ["Role", "Uᵤ", "Xᵤ", "Xᵥ", "Yᵤ", "Yᵥ"];
    Some(names.iter().map(|n| (*n).to_owned()).zip(items).collect())
}

fn coordinate(v: Option<&RawValue>) -> bool {
    v.is_some_and(|n| {
        let s = n.get();
        !s.is_empty() && s.len() <= 78 && s.bytes().all(|b| b.is_ascii_digit())
    })
}

fuzz_target!(|data: &[u8]| {
    let (curve, siec, p256) = fuzzing::pake(data);
    if let Some(c) = curve {
        assert!(c == "siec" || c == "p256", "curve {c:?}");
        let v = fields(data).expect("a named curve from no JSON");
        assert!(v.contains_key("Uᵤ"), "a curve named without Uᵤ");
    }
    if siec || p256 {
        assert!(data.len() <= PAKE_BYTES, "{} bytes answered", data.len());
        // A map keeps the last of a key given twice, and so can differ
        // from the engine on a message it refuses anyway; only answered
        // messages are restated.
        let v = fields(data).expect("answered no JSON");
        let role = v
            .get("Role")
            .and_then(|r| serde_json::from_str::<u8>(r.get()).ok());
        assert_eq!(role, Some(0), "answered role");
        assert!(
            coordinate(v.get("Xᵤ").map(AsRef::as_ref)),
            "answered without Xᵤ"
        );
        assert!(
            coordinate(v.get("Xᵥ").map(AsRef::as_ref)),
            "answered without Xᵥ"
        );
    }
});
