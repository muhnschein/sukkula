//! croc codes as the user types them, through the adapter's check and the
//! room and password croc derives from them
//! (`sukkula_engine::croc::fuzzing::code`).
//!
//! Nothing reaches the relay with a code that fails here. Asserted of
//! every accepted code, from croc's rules restated here: it is the input's
//! words joined by hyphens, 6 to 128 bytes of printable ASCII with no
//! space; its room is the hex SHA-256 of its first four bytes and "croc",
//! computed here independently; its password is everything after the
//! fifth byte. And the check is a function of the code: accepting it
//! again, with white space around it, gives the same room and password.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sha2::{Digest, Sha256};
use sukkula_engine::croc::fuzzing::{self, CODE_BYTES};

fuzz_target!(|data: &[u8]| {
    let Ok(typed) = std::str::from_utf8(data) else {
        return;
    };
    let joined = typed.split_whitespace().collect::<Vec<_>>().join("-");
    let valid =
        (6..=CODE_BYTES).contains(&joined.len()) && joined.bytes().all(|b| b.is_ascii_graphic());
    let Some((room, password)) = fuzzing::code(typed) else {
        assert!(!valid, "{joined:?} refused");
        return;
    };
    assert!(valid, "{joined:?} accepted");
    let bytes = joined.as_bytes();
    let mut h = Sha256::new();
    h.update(bytes.get(..4).expect("six bytes at least"));
    h.update(b"croc");
    let expected: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(room, expected, "the room is not croc's");
    assert_eq!(password, bytes.get(5..).expect("six bytes at least"));
    assert_eq!(
        fuzzing::code(&format!("\t{joined} \n")),
        Some((room, password)),
        "white space around a code changed it"
    );
});
