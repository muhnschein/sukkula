//! The settings, as the file on disk (maybe hand-edited, maybe from an
//! older version) and as `set_settings` from the UI: `serde_json` into
//! `Settings`, then `Settings::validate`.
//!
//! Asserted of what validates: the device name is S2-clean and capped, the
//! PIN is 1 to 16 ASCII letters or digits, a custom URL is printable ASCII
//! within 256 bytes and has its protocol's scheme; validation is idempotent,
//! and what is saved reads back as what was validated. The input is capped
//! at the store's 16 KiB read limit, as the real file is.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::config::{MAX_SETTINGS_BYTES, MAX_URL_BYTES, Settings};
use sukkula_core::limits::{MAX_ALIAS_CHARS, MAX_PIN_CHARS};
use sukkula_fuzz::{assert_clean, oracle};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_SETTINGS_BYTES {
        return;
    }
    let Ok(parsed) = serde_json::from_slice::<Settings>(data) else {
        return;
    };
    let Ok(v) = parsed.validate() else {
        return;
    };
    assert_clean(
        "device_name",
        &v.device_name,
        oracle::display_violation(&v.device_name, MAX_ALIAS_CHARS),
    );
    if let Some(p) = &v.localsend.pin {
        assert!(!p.is_empty() && p.chars().count() <= MAX_PIN_CHARS, "{p:?}");
        assert!(p.bytes().all(|b| b.is_ascii_alphanumeric()), "{p:?}");
    }
    for (url, schemes) in [
        (&v.wormhole.mailbox_url, &["ws://", "wss://"][..]),
        (&v.wormhole.relay_url, &["tcp://"][..]),
    ] {
        if let Some(u) = url {
            assert!(
                u.len() <= MAX_URL_BYTES && u.bytes().all(|b| b.is_ascii_graphic()),
                "{u:?}"
            );
            let lower = u.to_ascii_lowercase();
            assert!(
                schemes
                    .iter()
                    .any(|s| lower.starts_with(s) && lower.len() > s.len()),
                "{u:?}"
            );
        }
    }
    assert_eq!(v.clone().validate().ok(), Some(v.clone()), "not idempotent");
    let saved = serde_json::to_vec(&v).expect("settings serialise");
    let back: Settings = serde_json::from_slice(&saved).expect("saved settings parse");
    assert_eq!(back, v, "round trip changed the settings");
});
