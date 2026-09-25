//! S2: aliases and device models, through `text::display`.
//!
//! The first byte picks the cap (0..=255 characters), the rest is the text.
//! Asserted: nothing the oracle forbids survives -- no control, bidi,
//! zero-width, default-ignorable, format, blank, private-use character, no
//! whitespace but the plain space -- the character cap holds with the
//! ellipsis counted, spaces are collapsed and trimmed, mark stacks are cut,
//! and display is idempotent.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::limits::MAX_ALIAS_CHARS;
use sukkula_core::text;
use sukkula_fuzz::{assert_clean, oracle};

fuzz_target!(|data: &[u8]| {
    let Some((&cap, rest)) = data.split_first() else {
        return;
    };
    let raw = String::from_utf8_lossy(rest);
    for max in [usize::from(cap), MAX_ALIAS_CHARS] {
        let out = text::display(&raw, max);
        assert_clean("display", &raw, oracle::display_violation(&out, max));
        assert!(!out.chars().any(text::is_forbidden), "{out:?}");
        assert_eq!(text::display(&out, max), out, "not idempotent for {raw:?}");
    }
});
