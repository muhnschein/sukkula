//! croc codes: `1234-word-word-word`.
//!
//! croc v10.0.3 and later derive two things from a code: the room the two
//! sides meet in on the relay, `hex(SHA-256(first four bytes ‖ "croc"))`,
//! and the PAKE password, everything after the fifth byte. Nothing is
//! normalised: a code is compared byte for byte.
//!
//! Ours are croc's own shape: four digits, then three words from
//! mnemonicode's list spelling four random bytes as croc spells them. The
//! words carry 32 bits; the digits only pick the room.

use sha2::{Digest, Sha256};

use super::words::{BASE, WORDS};
use crate::api::{ErrorCode, ErrorInfo};

/// Shortest code croc takes, in bytes.
pub(super) const MIN_CODE_BYTES: usize = 6;

/// Longest code taken here, in bytes. croc's are under 40.
pub(super) const MAX_CODE_BYTES: usize = 128;

/// A code that can be used: printable ASCII, no spaces, 6 to 128 bytes.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Code(String);

impl std::fmt::Debug for Code {
    /// S9: the code is the transfer's secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Code(<{} bytes>)", self.0.len())
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Code {
    /// The room on the relay.
    pub(super) fn room(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.0.as_bytes().get(..4).unwrap_or_default());
        h.update(b"croc");
        sukkula_core::hex::encode(&h.finalize())
    }

    /// The PAKE password: everything after the fifth byte.
    pub(super) fn password(&self) -> &[u8] {
        self.0.as_bytes().get(5..).unwrap_or_default()
    }
}

/// A new code from ten random bytes: four digits, three words.
pub(super) fn generate(random: [u8; 10]) -> Code {
    let [d0, d1, d2, d3, d4, d5, w0, w1, w2, w3] = random;
    // Four digits from 48 bits: the bias of 2^48 mod 10^4 is nothing.
    let digits = u64::from_le_bytes([d0, d1, d2, d3, d4, d5, 0, 0])
        .checked_rem(10_000)
        .unwrap_or(0);
    let x = u32::from_le_bytes([w0, w1, w2, w3]);
    let word = |i: u32| {
        let n = x
            .checked_div(BASE.checked_pow(i).unwrap_or(1))
            .and_then(|v| v.checked_rem(BASE))
            .unwrap_or(0);
        WORDS
            .get(usize::try_from(n).unwrap_or(0))
            .copied()
            .unwrap_or("croc")
    };
    Code(format!("{digits:04}-{}-{}-{}", word(0), word(1), word(2)))
}

/// A code as the user typed it: spaces between the parts become hyphens,
/// as croc's command line joins its arguments, and nothing else changes.
///
/// # Errors
///
/// [`ErrorCode::BadCode`]: too short or long, or not printable ASCII.
pub(super) fn parse(typed: &str) -> Result<Code, ErrorInfo> {
    let joined = typed.split_whitespace().collect::<Vec<_>>().join("-");
    let ok = (MIN_CODE_BYTES..=MAX_CODE_BYTES).contains(&joined.len())
        && joined.bytes().all(|b| b.is_ascii_graphic());
    if !ok {
        return Err(ErrorInfo::new(ErrorCode::BadCode, "not a croc code"));
    }
    Ok(Code(joined))
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;

    #[test]
    fn rooms_and_passwords_are_croc_s() {
        // croc-protocol.md §1.2, seen on a live relay.
        let code = parse("8123-alpha-bravo-charlie").unwrap();
        assert_eq!(
            code.room(),
            "1cdafe5c70a87f001f2f437fa6773c0ee726d10b7c3f86184dc68fa709c2f355"
        );
        assert_eq!(code.password(), b"alpha-bravo-charlie");
        assert_eq!(parse(" 8123 alpha  bravo charlie ").unwrap(), code);
        assert_eq!(format!("{code:?}"), "Code(<24 bytes>)");
    }

    #[test]
    fn codes_are_checked() {
        for bad in [
            "",
            "12345",
            "1234-ä-b",
            "1234-\u{7}",
            &"a".repeat(MAX_CODE_BYTES + 1),
        ] {
            assert_eq!(parse(bad).unwrap_err().code, ErrorCode::BadCode, "{bad:?}");
        }
        assert!(parse("abcdef").is_ok(), "croc takes any six bytes");
    }

    #[test]
    fn our_codes_are_croc_shaped() {
        // mnemonicode's own vectors (croc-protocol.md A.4).
        let c = generate([0, 0, 0, 0, 0, 0, 1, 2, 3, 4]);
        assert_eq!(c.to_string(), "0000-papa-twist-alpine");
        let c = generate([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(c.to_string(), "0655-natural-analyze-verbal");
        assert_eq!(WORDS[0], "academy");
        assert_eq!(WORDS[1625], "amen");
        let c = generate([9; 10]);
        assert_eq!(parse(&c.to_string()).unwrap(), c);
    }
}
