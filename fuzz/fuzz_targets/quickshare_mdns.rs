//! Quick Share discovery: a resolved mDNS service -- its instance name and
//! its `n` TXT record, both anyone on the link can announce -- through
//! rqs_lib's parsers and the adapter's naming, to the peer the send page
//! lists.
//!
//! The input is the full name, a NUL, then the record. Asserted: a listed
//! peer's id is `qs:` and the four endpoint-id bytes, as letters and digits
//! or as eight hex digits, and nothing else; its name is S2-clean, capped
//! and never empty, and is exactly what S2 makes of the name the record
//! carries, as decoded here independently (base64url without padding, a
//! flags byte, 16 identity bytes, a length byte, then UTF-8).
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_core::Protocol;
use sukkula_core::limits::MAX_ALIAS_CHARS;
use sukkula_core::offer::UNKNOWN_SENDER;
use sukkula_core::text;
use sukkula_engine::quickshare::fuzzing;
use sukkula_fuzz::assert_peer;

/// RFC 4648 base64url, no padding, strictly: what `URL_SAFE_NO_PAD` takes.
fn base64url(s: &str) -> Option<Vec<u8>> {
    let mut bits: u32 = 0;
    let mut n = 0u32;
    let mut out = Vec::new();
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        // At most 13 bits are ever pending.
        bits = ((bits << 6) | u32::from(v)) & 0x3FFF;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push(u8::try_from((bits >> n) & 0xFF).ok()?);
        }
    }
    // A lone leftover sextet, or leftover bits that are not zero, is not
    // canonical base64.
    if n >= 6 || bits & ((1 << n) - 1) != 0 {
        return None;
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let (fullname, n) = s.split_once('\0').unwrap_or((s, ""));
    let Some(peer) = fuzzing::peer_from_mdns(fullname, n) else {
        return;
    };
    assert_peer(&peer);
    assert_eq!(peer.protocol, Protocol::QuickShare);
    assert_eq!(peer.model, None);

    // The id: the endpoint id from the instance name.
    let instance = fullname.split('.').next().unwrap_or("");
    let bytes = base64url(instance).expect("a listed instance name is base64url");
    let [0x23, a, b, c, d, 0xFC, 0x9F, 0x5E, _, _] = bytes[..] else {
        panic!("{instance:?} listed but not a Quick Share instance name")
    };
    let id = [a, b, c, d];
    let want = if id.iter().all(u8::is_ascii_alphanumeric) {
        format!(
            "qs:{}",
            id.iter().map(|b| char::from(*b)).collect::<String>()
        )
    } else {
        format!(
            "qs:{}",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    };
    assert_eq!(peer.id, want);

    // The name: what the record carries, after S2.
    let record = base64url(n).expect("a listed record is base64url");
    let len = usize::from(record[17]);
    let name = std::str::from_utf8(&record[18..18 + len]).expect("a listed name is UTF-8");
    let shown = text::display(name, MAX_ALIAS_CHARS);
    let shown = if shown.is_empty() {
        UNKNOWN_SENDER.to_owned()
    } else {
        shown
    };
    assert_eq!(peer.name, shown);
});
