//! What a QR code says, read for a code to receive with (spec v0.6):
//! `sukkula_engine::scan::read`, which every code the camera finds goes
//! through before the UI sees it.
//!
//! A QR code is anyone's to print, so this is a peer's input. Asserted of
//! every code it gives, beyond the shape every code keeps
//! (`sukkula_fuzz::assert_scanned`): a Magic Wormhole code comes from a
//! text that starts `wormhole-transfer:`, in any case, and is that URI's
//! path, percent-decoded and lowercased; a mailbox comes with a
//! `rendezvous` parameter. A croc code is the text itself when the text is
//! croc-shaped -- three or more words of `a` to `z`, or four digits and
//! three or more such words, restated here -- and otherwise comes from
//! croc's web link: an `https` URL to `getcroc.com`, root path, no port,
//! credentials or fragment, whose one query parameter is `code`, with the
//! code as that parameter says, white space joined by hyphens. Anything
//! else is `Other`, and every croc-shaped code of 6 to 128 bytes is read
//! as one. White space around the text changes nothing. And the same text,
//! typed (`scan::typed`), gives what the QR code would, or else what the
//! typed rules restated in `typed` below say.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_engine::api::Scanned;
use sukkula_engine::scan;
use sukkula_fuzz::assert_scanned;

const SCHEME: &str = "wormhole-transfer:";

/// croc's own shapes of code.
fn croc_shaped(t: &str) -> bool {
    let word = |w: &&str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase());
    let parts: Vec<&str> = t.split('-').collect();
    let pin = parts
        .first()
        .is_some_and(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_digit()));
    if pin {
        parts.len() >= 4 && parts.iter().skip(1).all(word)
    } else {
        parts.len() >= 3 && parts.iter().all(word)
    }
}

/// `%XX` decoded, anything else as it is.
fn percent_decoded(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let escaped = (b[i] == b'%' && i + 2 < b.len())
            .then(|| hex(b[i + 1]).zip(hex(b[i + 2])))
            .flatten();
        match escaped {
            Some((h, l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    out
}

/// croc's web link, as the URL parser reads it: the code it carries.
fn croc_link(t: &str) -> Option<String> {
    let url = url::Url::parse(t).ok()?;
    if url.host_str() != Some("getcroc.com") {
        return None;
    }
    let plain = url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.fragment().is_none();
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    match pairs.as_slice() {
        [(k, v)] if plain && k == "code" => {
            Some(v.split_whitespace().collect::<Vec<_>>().join("-"))
        }
        _ => Some(String::new()),
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let found = scan::read(text);
    assert_scanned(&found);
    let t = text.trim();
    let wormhole = t
        .get(..SCHEME.len())
        .is_some_and(|h| h.eq_ignore_ascii_case(SCHEME));
    match &found {
        Scanned::Wormhole { code, mailbox_url } => {
            assert!(wormhole, "a wormhole code from {t:?}");
            // The URL parser drops C0 controls and spaces at either end,
            // and tabs and newlines anywhere.
            let rest: String = t[SCHEME.len()..]
                .trim_end_matches(|c: char| c <= ' ')
                .chars()
                .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
                .collect();
            let path = rest.split(['?', '#']).next().unwrap_or_default();
            let decoded = String::from_utf8(percent_decoded(path))
                .expect("a code from bytes that are not UTF-8");
            assert_eq!(
                decoded.to_ascii_lowercase(),
                *code,
                "the code is not the URI's"
            );
            if mailbox_url.is_some() {
                assert!(
                    rest.contains("rendezvous="),
                    "a mailbox from nowhere in {t:?}"
                );
            }
        }
        Scanned::Croc { code } => {
            assert!(!wormhole, "a croc code from a wormhole URI");
            if croc_shaped(t) {
                assert_eq!(code, t, "croc's words changed");
            } else {
                let link = croc_link(t).unwrap_or_else(|| panic!("a croc code from {t:?}"));
                assert_eq!(*code, link, "the code is not the link's");
            }
        }
        Scanned::Other => {
            assert!(
                !(croc_shaped(t) && (6..=128).contains(&t.len())),
                "croc's words {t:?} not read"
            );
        }
    }
    assert_eq!(
        scan::read(&format!("\t{t} \n")),
        found,
        "white space around the text changed it"
    );
    typed(t, &found);
});

/// The same text typed instead (`scan::typed`): what a QR code would give,
/// else croc's words in lower case, else a numbered code of letters and
/// digits for Magic Wormhole, else a croc code as typed -- words joined by
/// hyphens, 6 to 128 printable characters, never a URL.
fn typed(t: &str, scanned: &Scanned) {
    let got = scan::typed(t);
    if *scanned != Scanned::Other {
        assert_eq!(got.as_ref(), Some(scanned), "typed, {t:?} reads otherwise");
        return;
    }
    let joined = t.split_whitespace().collect::<Vec<_>>().join("-");
    let lower = joined.to_ascii_lowercase();
    match got {
        None | Some(Scanned::Other) => {
            assert!(got.is_none(), "typed gives Other for {t:?}");
            assert!(
                !(croc_shaped(&lower) && (6..=128).contains(&lower.len())),
                "croc's words {t:?} refused"
            );
        }
        Some(Scanned::Wormhole { code, mailbox_url }) => {
            assert_eq!(code, lower, "a typed wormhole code changed");
            assert!(mailbox_url.is_none(), "a mailbox from typing");
            let (nameplate, words) = code.split_once('-').expect("a numbered code");
            assert!(!nameplate.is_empty() && nameplate.bytes().all(|b| b.is_ascii_digit()));
            assert!(
                !words.is_empty()
                    && words
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            );
        }
        Some(Scanned::Croc { code }) => {
            assert!(
                !t.to_ascii_lowercase().contains("://"),
                "a URL as a croc code"
            );
            if croc_shaped(&lower) {
                assert_eq!(code, lower, "croc's words not lowercased");
            } else {
                assert_eq!(code, joined, "a chosen croc code changed");
            }
            assert!((6..=128).contains(&code.len()));
            assert!(code.bytes().all(|b| b.is_ascii_graphic()));
        }
    }
}
