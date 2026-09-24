//! The FFI command channel: `sukkula_command(handle, json)` hands the string
//! to `sukkula_engine::api::parse_command`.
//!
//! The UI is ours, but the parser is held to the peer standard (spec §3,
//! §7 "FFI command JSON"). Asserted: over 64 KiB is refused as too large
//! before any parsing; a parsed command has the current API version and
//! survives its own serialiser unchanged, so the engine and the shell cannot
//! disagree about what a command said; an error carries an id only when
//! the input really had one; and settings arriving in `set_settings` validate
//! to something S2-clean.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::limits::{MAX_ALIAS_CHARS, MAX_MESSAGE_BYTES};
use sukkula_engine::api::{API_VERSION, Command, ParseError, parse_command};
use sukkula_fuzz::{assert_clean, oracle};

fuzz_target!(|data: &[u8]| {
    // A C string crosses the ABI; the FFI turns invalid UTF-8 into an error
    // before this point, so only valid UTF-8 reaches the parser.
    let Ok(json) = std::str::from_utf8(data) else {
        return;
    };
    match parse_command(json) {
        Ok(env) => {
            assert!(json.len() <= MAX_MESSAGE_BYTES);
            assert_eq!(env.v, API_VERSION);
            let again = serde_json::to_string(&env).expect("commands serialise");
            let reparsed = parse_command(&again).expect("a serialised command parses");
            assert_eq!(reparsed, env, "round trip changed the command");
            if let Command::SetSettings { settings } = env.cmd
                && let Ok(v) = settings.validate()
            {
                assert_clean(
                    "device_name",
                    &v.device_name,
                    oracle::display_violation(&v.device_name, MAX_ALIAS_CHARS),
                );
            }
        }
        Err((id, e)) => {
            if json.len() > MAX_MESSAGE_BYTES {
                assert_eq!(e, ParseError::TooLarge);
                assert_eq!(id, None);
            } else {
                assert_ne!(e, ParseError::TooLarge);
            }
            if let Some(id) = id {
                // The id was really there: an object with that numeric id,
                // or -- serde's derive reads structs positionally too -- an
                // array that starts with it.
                // `Value` is stricter than JSON itself: it refuses numbers
                // that overflow an f64 (`4e+6666`), which the grammar allows
                // and the id probe rightly skips over. The first run of this
                // target found exactly that; only a `Value` can be checked.
                let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
                    return;
                };
                let found = match &v {
                    serde_json::Value::Array(a) => a.first().and_then(serde_json::Value::as_u64),
                    other => other.get("id").and_then(serde_json::Value::as_u64),
                };
                assert_eq!(found, Some(id));
            }
        }
    }
});
