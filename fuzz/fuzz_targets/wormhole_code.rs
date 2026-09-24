//! Magic Wormhole codes as the user types them (F-MW2), through the
//! adapter's strict check and the library's own parser and entropy
//! estimate: `sukkula_engine::wormhole::fuzzing::code` (spec §7).
//!
//! Nothing reaches the network with a code that fails here, so this is
//! the whole of what a code can do before it is used. Asserted of every
//! accepted code, from the grammar restated here: it is the input trimmed
//! and lowercased, at most 128 bytes; a nameplate of one to nine digits
//! without a leading zero; one to eight words of ASCII lowercase letters
//! and digits, 1 to 32 each, at least 4 bytes in all; no other character
//! at all. And the check is a function of the code: accepting it again, in
//! capitals or with white space around it, gives the same code. Every
//! refusal is `BadCode`.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_engine::api::ErrorCode;
use sukkula_engine::wormhole::fuzzing::{self, CODE_BYTES};

fn grammatical(code: &str) -> bool {
    let Some((nameplate, password)) = code.split_once('-') else {
        return false;
    };
    let words: Vec<&str> = password.split('-').collect();
    (1..=9).contains(&nameplate.len())
        && nameplate.bytes().all(|b| b.is_ascii_digit())
        && !nameplate.starts_with('0')
        && password.len() >= 4
        && (1..=8).contains(&words.len())
        && words.iter().all(|w| {
            (1..=32).contains(&w.len())
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    match fuzzing::code(raw) {
        Ok(code) => {
            assert_eq!(
                code,
                raw.trim().to_ascii_lowercase(),
                "the code is not what was typed"
            );
            assert!(code.len() <= CODE_BYTES, "{} bytes", code.len());
            assert!(grammatical(&code), "{code:?} accepted");
            assert_eq!(fuzzing::code(&code).ok().as_deref(), Some(code.as_str()));
            assert_eq!(
                fuzzing::code(&format!(" {}\n", code.to_ascii_uppercase()))
                    .ok()
                    .as_deref(),
                Some(code.as_str())
            );
        }
        Err(e) => assert_eq!(e.code, ErrorCode::BadCode, "refused as {:?}", e.code),
    }
});
