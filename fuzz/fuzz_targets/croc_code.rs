//! croc codes as the user types them, through the adapter's check and the
//! room, password and public relay croc 11 derives from them
//! (`sukkula_engine::croc::fuzzing::code`).
//!
//! Nothing reaches the relay with a code that fails here. Asserted of
//! every accepted code, from croc 11's rules (`codephrase.Parse`) restated
//! here: it is the input's words joined by hyphens, 6 to 128 bytes of
//! printable ASCII with no space. Its room is the hex SHA-256 of a
//! selector and "croc", and its password the rest, by its shape: three
//! words of the EFF short list (found here by trying every way to cut it
//! in three) or three words of `a` to `z` give the first word and the
//! other two; four such words the first two and the last two; anything
//! else the first four bytes and everything after the fifth. Its relay is
//! its SHA-256, big-endian, modulo 4: the last byte's two low bits. And
//! the check is a function of the code: accepting it again, with white
//! space around it, gives the same.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use std::collections::HashSet;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use sha2::{Digest, Sha256};
use sukkula_engine::croc::fuzzing::{self, CODE_BYTES};

fn eff() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        include_str!("../../crates/sukkula-engine/src/croc/eff/eff-short-wordlist-1.txt")
            .lines()
            .collect()
    })
}

/// The code as three EFF words, if it is: the first way to cut its parts
/// in three, the first cut earliest.
fn three_eff_words(parts: &[&str]) -> Option<[String; 3]> {
    let n = parts.len();
    for i in 1..n {
        for j in i + 1..n {
            let words = [
                parts[..i].join("-"),
                parts[i..j].join("-"),
                parts[j..].join("-"),
            ];
            if words.iter().all(|w| eff().contains(w.as_str())) {
                return Some(words);
            }
        }
    }
    None
}

fn lowercase(parts: &[&str], count: usize) -> bool {
    parts.len() == count
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()))
}

/// The selector and the password of `code`.
fn split(code: &str) -> (String, Vec<u8>) {
    let parts: Vec<&str> = code.split('-').collect();
    if let Some([a, b, c]) = three_eff_words(&parts) {
        return (a, format!("{b}-{c}").into_bytes());
    }
    if lowercase(&parts, 3) {
        return (
            parts[0].to_owned(),
            format!("{}-{}", parts[1], parts[2]).into_bytes(),
        );
    }
    if lowercase(&parts, 4) {
        return (
            format!("{}-{}", parts[0], parts[1]),
            format!("{}-{}", parts[2], parts[3]).into_bytes(),
        );
    }
    let bytes = code.as_bytes();
    (
        String::from_utf8(bytes.get(..4).expect("six bytes at least").to_vec())
            .expect("ASCII"),
        bytes.get(5..).expect("six bytes at least").to_vec(),
    )
}

fuzz_target!(|data: &[u8]| {
    let Ok(typed) = std::str::from_utf8(data) else {
        return;
    };
    let joined = typed.split_whitespace().collect::<Vec<_>>().join("-");
    let valid =
        (6..=CODE_BYTES).contains(&joined.len()) && joined.bytes().all(|b| b.is_ascii_graphic());
    let Some((room, password, relay)) = fuzzing::code(typed) else {
        assert!(!valid, "{joined:?} refused");
        return;
    };
    assert!(valid, "{joined:?} accepted");
    let (selector, expected_password) = split(&joined);
    let mut h = Sha256::new();
    h.update(selector.as_bytes());
    h.update(b"croc");
    let expected: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(room, expected, "the room is not croc's");
    assert_eq!(password, expected_password, "the password is not croc's");
    let digest = Sha256::digest(joined.as_bytes());
    assert_eq!(relay, usize::from(digest[31] & 3), "the relay is not croc's");
    assert_eq!(
        fuzzing::code(&format!("\t{joined} \n")),
        Some((room, password, relay)),
        "white space around a code changed it"
    );
});
