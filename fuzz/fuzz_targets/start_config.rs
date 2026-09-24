//! `sukkula_start(config_json, ...)`: the start configuration, through
//! `sukkula_engine::api::parse_start_config`.
//!
//! Asserted: over 64 KiB is refused before parsing; a parsed configuration
//! has the current API version, refuses unknown fields (so a misspelt
//! `allow_loopback` cannot silently mean something), and survives its own
//! serialiser unchanged.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::limits::MAX_MESSAGE_BYTES;
use sukkula_engine::api::{API_VERSION, ParseError, parse_start_config};

fuzz_target!(|data: &[u8]| {
    let Ok(json) = std::str::from_utf8(data) else {
        return;
    };
    match parse_start_config(json) {
        Ok(cfg) => {
            assert!(json.len() <= MAX_MESSAGE_BYTES);
            assert_eq!(cfg.v, API_VERSION);
            let again = serde_json::to_string(&cfg).expect("configs serialise");
            assert_eq!(
                parse_start_config(&again).ok(),
                Some(cfg),
                "round trip changed it"
            );
            // Only the known keys: every key of the object is one of them.
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(json) {
                for k in map.keys() {
                    assert!(
                        matches!(
                            k.as_str(),
                            "v" | "data_dir" | "download_dir" | "device_model" | "allow_loopback"
                        ),
                        "unknown key {k:?} accepted"
                    );
                }
            }
        }
        Err(e) => {
            assert_eq!(e == ParseError::TooLarge, json.len() > MAX_MESSAGE_BYTES);
        }
    }
});
