//! LocalSend's `POST /prepare-upload` body -- the offer itself, from any
//! sender on the LAN -- through upstream's DTO, the adapter's conversion
//! and `Offer::validate`: `sukkula_engine::localsend::offer_from_prepare_upload`,
//! the steps the server takes (spec §7 "LocalSend DTOs").
//!
//! Asserted of every offer the user would be shown: every S-rule
//! (`sukkula_fuzz::assert_offer`); nothing over 64 KiB was parsed; the
//! sender said HTTPS (F-LS2) and offered something; and, read independently
//! through `serde_json::Value`, the offer is what the JSON said -- a lone
//! `text/plain` file with a preview is the message and nothing else
//! (F-C4), otherwise every file is there, in id order, with the size it
//! declared exactly and the digest it declared.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use sukkula_core::Protocol;
use sukkula_core::limits::MAX_MESSAGE_BYTES;
use sukkula_engine::localsend::offer_from_prepare_upload;
use sukkula_fuzz::assert_offer;

fn hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let Some(offer) = offer_from_prepare_upload(data) else {
        return;
    };
    assert!(
        data.len() <= MAX_MESSAGE_BYTES,
        "a body over 64 KiB was parsed"
    );
    assert_offer(&offer);
    assert_eq!(offer.protocol, Protocol::LocalSend);
    assert_eq!(offer.pin, None, "LocalSend offers carry no PIN");

    // What the JSON said, read another way. `Value` refuses numbers that
    // overflow an f64, which the DTO skips over in fields it ignores; only
    // what `Value` can hold is compared.
    let Ok(v) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    assert_eq!(
        v["info"]["protocol"], "https",
        "a plain-HTTP sender's offer (F-LS2)"
    );
    let files = v["files"].as_object().expect("an offer has a files object");
    assert!(!files.is_empty(), "an offer of no files");

    let lone_text = files.len() == 1
        && files.values().all(|f| {
            !f["preview"].is_null()
                && f["fileType"]
                    .as_str()
                    .is_some_and(|t| t.trim().to_ascii_lowercase().starts_with("text/plain"))
        });
    if lone_text {
        assert!(offer.files.is_empty(), "a message came with files");
        assert!(offer.text.is_some(), "a message lost its text");
        return;
    }
    assert_eq!(offer.text, None, "files came with a message");
    // serde_json's map is ordered by key, as the adapter sorts the ids.
    assert_eq!(offer.files.len(), files.len(), "files went missing");
    for (got, (id, f)) in offer.files.iter().zip(files) {
        assert_eq!(
            f["size"].as_u64(),
            Some(got.size),
            "file {id:?}: the size changed"
        );
        match f.get("sha256") {
            Some(Value::String(h)) => {
                assert_eq!(hex32(h), got.sha256, "file {id:?}: the digest changed")
            }
            _ => assert_eq!(got.sha256, None, "file {id:?}: a digest from nowhere"),
        }
    }
});
