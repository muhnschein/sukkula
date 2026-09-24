//! S1: every peer-supplied file name, through `name::sanitize`.
//!
//! The property, not just survival: the output is one normal path
//! component, non-empty, at most 200 bytes, with nothing the oracle forbids,
//! no leading dot, no trailing dot or space, no mark without a base or on a
//! dot, and sanitising it again changes nothing. The numbered spellings the
//! inbox falls back to keep all of that.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::name::{is_safe, sanitize};
use sukkula_fuzz::{assert_clean, oracle};

fuzz_target!(|data: &[u8]| {
    // Adapters hand over names from JSON or protobuf strings, so they are
    // valid UTF-8 by then; lossy decoding keeps every byte pattern in play.
    let raw = String::from_utf8_lossy(data);
    let out = sanitize(&raw);
    assert_clean("sanitize", &raw, oracle::name_violation(out.as_str()));
    assert_eq!(sanitize(out.as_str()), out, "not idempotent for {raw:?}");
    assert!(is_safe(out.as_str()));

    let n = data.iter().fold(0u32, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u32::from(*b))
    });
    let numbered = out.numbered(n % 2000);
    assert_clean("numbered", &raw, oracle::name_violation(numbered.as_str()));
    assert!(
        is_safe(numbered.as_str()),
        "{numbered:?} is not a fixed point"
    );
});
