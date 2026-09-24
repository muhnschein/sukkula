//! Hex for digests and fingerprints, strict on the way in.

use std::fmt::Write as _;

/// Lowercase hex of `bytes`.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        // Writing to a String cannot fail.
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Parses exactly 32 bytes of hex, either case, nothing else: no prefix, no
/// separators, no whitespace.
#[must_use]
pub fn decode_32(s: &str) -> Option<[u8; 32]> {
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        let [hi, lo] = pair else { return None };
        *slot = nibble(*hi)?.checked_mul(16)?.checked_add(nibble(*lo)?)?;
    }
    Some(out)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => c.checked_sub(b'0'),
        b'a'..=b'f' => c.checked_sub(b'a')?.checked_add(10),
        b'A'..=b'F' => c.checked_sub(b'A')?.checked_add(10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let d = [0xABu8; 32];
        assert_eq!(decode_32(&encode(&d)), Some(d));
        assert_eq!(decode_32(&encode(&d).to_uppercase()), Some(d));
    }

    #[test]
    fn rejects_anything_else() {
        assert_eq!(decode_32(""), None);
        assert_eq!(decode_32(&"0".repeat(63)), None);
        assert_eq!(decode_32(&"0".repeat(65)), None);
        assert_eq!(decode_32(&format!("0x{}", "0".repeat(62))), None);
        assert_eq!(decode_32(&"g".repeat(64)), None);
        assert_eq!(decode_32(&"é".repeat(32)), None);
    }
}
