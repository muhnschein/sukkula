//! S2 and F-C4: received text messages, through `text::message`.
//!
//! Asserted: nothing the oracle forbids survives except `\n`, the result is
//! at most 64 KiB, trimmed, with spaces collapsed, no space at a line edge,
//! at most two empty lines in a row, no mark without a base, mark stacks
//! cut; and `message` is idempotent.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::text;
use sukkula_fuzz::{assert_clean, oracle};

fuzz_target!(|data: &[u8]| {
    let raw = String::from_utf8_lossy(data);
    let out = text::message(&raw);
    assert_clean("message", &raw, oracle::message_violation(&out));
    assert!(!out.chars().any(text::is_forbidden_in_message), "{out:?}");
    assert_eq!(text::message(&out), out, "not idempotent for {raw:?}");
});
