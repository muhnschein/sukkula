//! Digests and fingerprints from peers (a LocalSend file's SHA-256, a
//! certificate fingerprint), through `hex::decode_32`.
//!
//! Asserted: exactly 64 hex digits of either case decode and nothing else
//! does -- no prefix, separator, whitespace or non-ASCII lookalike -- and
//! what decodes re-encodes to the lowercase input; any 32 bytes survive
//! `encode` then `decode_32`.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::hex;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let strictly_hex = s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    match hex::decode_32(&s) {
        Some(bytes) => {
            assert!(strictly_hex, "accepted {s:?}");
            assert_eq!(hex::encode(&bytes), s.to_ascii_lowercase());
        }
        None => assert!(!strictly_hex, "refused {s:?}"),
    }
    if let Some(chunk) = data.get(..32) {
        let mut raw = [0u8; 32];
        raw.copy_from_slice(chunk);
        let encoded = hex::encode(&raw);
        assert_eq!(encoded.len(), 64);
        assert_eq!(hex::decode_32(&encoded), Some(raw));
        assert_eq!(hex::decode_32(&encoded.to_ascii_uppercase()), Some(raw));
    }
});
