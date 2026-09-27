//! croc codes, as croc 11 reads them (`src/codephrase/codephrase.go`).
//!
//! A code gives two things: the room the two sides meet in on the relay,
//! `hex(SHA-256(selector ‖ "croc"))`, and the PAKE password. Which part of
//! the code is which depends on its shape:
//!
//! - three words of the EFF short word list (one of which, `yo-yo`, has a
//!   hyphen of its own), or three words of lowercase letters: the first
//!   word selects the room, the other two, hyphen and all, are the
//!   password;
//! - four words of lowercase letters: the first two, the last two;
//! - anything else, croc 10's `8123-alpha-bravo-charlie` among them: the
//!   first four bytes, and everything after the fifth.
//!
//! Nothing is normalised: a code is compared byte for byte.
//!
//! Ours are three EFF words whose first word has four letters, so that
//! croc 10's rule -- the first four bytes, everything after the fifth --
//! finds the same room and password: a croc 10 receiver on the same relay
//! can take them too. The password is the last two words, about 21 bits,
//! as croc 11's own; the first word only picks the room.
//!
//! With croc's public relays, the code also says which of them the two
//! sides use: [`Code::relay_index`].
//!
//! A code also comes from a QR code ([`from_qr`], spec v0.6): croc 11's
//! link for receiving in a browser, or a code on its own, as croc 10 put
//! it in its QR codes and Sukkula does.

use std::collections::HashSet;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use url::Url;

use crate::api::{ErrorCode, ErrorInfo, Scanned};

/// Shortest code croc takes, in bytes.
pub(super) const MIN_CODE_BYTES: usize = 6;

/// Longest code taken here, in bytes. croc's are under 40.
pub(super) const MAX_CODE_BYTES: usize = 128;

/// The EFF's Short Wordlist #1, as croc 11 carries it: 1296 words, one a
/// line. By the Electronic Frontier Foundation, under the Creative Commons
/// Attribution 4.0 International licence (`eff/LICENSE.txt`); the dice
/// numbers of the original are left out
/// (<https://www.eff.org/files/2016/09/08/eff_short_wordlist_1.txt>).
static EFF_LIST: &str = include_str!("eff/eff-short-wordlist-1.txt");

/// The list, in order: 1296 words.
fn eff_words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| EFF_LIST.lines().collect())
}

/// Whether `word` is on the list.
fn is_eff_word(word: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| eff_words().iter().copied().collect())
        .contains(word)
}

/// The list's four-letter words, which our codes start with.
fn four_letter_words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| {
        eff_words()
            .iter()
            .copied()
            .filter(|w| w.len() == 4)
            .collect()
    })
}

/// A code that can be used: printable ASCII, no spaces, 6 to 128 bytes,
/// with the room and password it gives.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Code {
    typed: String,
    room: String,
    password: String,
}

impl std::fmt::Debug for Code {
    /// S9: the code is the transfer's secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Code(<{} bytes>)", self.typed.len())
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.typed)
    }
}

impl Code {
    /// A code already checked to be printable ASCII of a length croc takes.
    fn new(typed: String) -> Code {
        let (selector, password) = split(&typed);
        let mut h = Sha256::new();
        h.update(selector.as_bytes());
        h.update(b"croc");
        let room = sukkula_core::hex::encode(&h.finalize());
        Code {
            room,
            password,
            typed,
        }
    }

    /// The room on the relay.
    pub(super) fn room(&self) -> String {
        self.room.clone()
    }

    /// The PAKE password.
    pub(super) fn password(&self) -> &[u8] {
        self.password.as_bytes()
    }

    /// Whether this is croc 10's own kind of code, four digits and then
    /// words (`8123-alpha-bravo-charlie`), which croc 11 never makes.
    pub(super) fn is_croc10s(&self) -> bool {
        let b = self.typed.as_bytes();
        b.get(..4)
            .is_some_and(|pin| pin.iter().all(u8::is_ascii_digit))
            && b.get(4) == Some(&b'-')
    }

    /// Which of `pool` public relays the code belongs to: SHA-256 of the
    /// code, read as a big-endian number, modulo `pool`
    /// (`codephrase.RelayIndex`). Zero for an empty pool.
    pub(super) fn relay_index(&self, pool: usize) -> usize {
        let Ok(n) = u64::try_from(pool) else {
            return 0;
        };
        let digest = Sha256::digest(self.typed.as_bytes());
        let index = digest.iter().fold(0u64, |acc, b| {
            acc.checked_mul(256)
                .and_then(|a| a.checked_add(u64::from(*b)))
                .and_then(|a| a.checked_rem(n))
                .unwrap_or(0)
        });
        usize::try_from(index).unwrap_or(0)
    }
}

/// The room selector and the password of `code`, by its shape.
fn split(code: &str) -> (&str, String) {
    if let Some(words) = eff_sequence(code, 3).or_else(|| lowercase_words(code, 3)) {
        let (first, rest) = words.split_at(1);
        return (first.first().copied().unwrap_or_default(), rest.join("-"));
    }
    if let Some(words) = lowercase_words(code, 4) {
        // The first two words and the hyphen between them, as typed.
        let first_two = words
            .iter()
            .take(2)
            .map(|w| w.len())
            .sum::<usize>()
            .saturating_add(1);
        let selector = code.get(..first_two).unwrap_or_default();
        return (selector, words.get(2..).unwrap_or_default().join("-"));
    }
    (
        code.get(..4).unwrap_or_default(),
        code.get(5..).unwrap_or_default().to_owned(),
    )
}

/// `code` as exactly `count` words of the EFF list, hyphen-joined; the
/// list's `yo-yo` makes the reading a search.
fn eff_sequence(code: &str, count: usize) -> Option<Vec<&str>> {
    let parts: Vec<&str> = code.split('-').collect();
    if parts.len() < count || parts.len() > count.saturating_mul(2) {
        return None;
    }
    let mut words = Vec::with_capacity(count);
    search(code, &parts, 0, count, &mut words).then_some(words)
}

/// Whether `parts[start..]` reads as the remaining words, pushed onto
/// `words` (taken off again when they do not lead anywhere).
fn search<'a>(
    code: &'a str,
    parts: &[&str],
    start: usize,
    count: usize,
    words: &mut Vec<&'a str>,
) -> bool {
    if words.len() == count {
        return start == parts.len();
    }
    for end in start.saturating_add(1)..=parts.len() {
        let Some(word) = span(code, parts, start, end) else {
            continue;
        };
        if !is_eff_word(word) {
            continue;
        }
        words.push(word);
        if search(code, parts, end, count, words) {
            return true;
        }
        words.pop();
    }
    false
}

/// Parts `start..end` of `code`, with the hyphens between them, as a slice
/// of `code` itself.
fn span<'a>(code: &'a str, parts: &[&str], start: usize, end: usize) -> Option<&'a str> {
    let offset = parts
        .get(..start)?
        .iter()
        .map(|p| p.len().saturating_add(1))
        .sum::<usize>();
    let len = parts
        .get(start..end)?
        .iter()
        .map(|p| p.len())
        .sum::<usize>()
        .saturating_add(end.saturating_sub(start).saturating_sub(1));
    code.get(offset..offset.checked_add(len)?)
}

/// `code` as exactly `count` hyphen-separated words of `a` to `z`.
fn lowercase_words(code: &str, count: usize) -> Option<Vec<&str>> {
    let words: Vec<&str> = code.split('-').collect();
    (words.len() == count
        && words
            .iter()
            .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase())))
    .then_some(words)
}

/// A new code: three EFF words, the first of four letters, on relay
/// `index` of a pool of `pool` when `on` is `Some((index, pool))`.
///
/// # Errors
///
/// [`ErrorCode::Internal`]: no randomness.
pub(super) fn generate(on: Option<(usize, usize)>) -> Result<Code, ErrorInfo> {
    generate_with(on, super::random::<2>)
}

/// [`generate`] with the random numbers from `draw`, two bytes a time.
fn generate_with(
    on: Option<(usize, usize)>,
    mut draw: impl FnMut() -> Result<[u8; 2], ErrorInfo>,
) -> Result<Code, ErrorInfo> {
    let mut pick = |from: &[&'static str]| -> Result<&'static str, ErrorInfo> {
        // Uniform: a draw past the last whole multiple of the list's length
        // is drawn again.
        let n = u32::try_from(from.len()).unwrap_or(1).max(1);
        let limit = 65_536u32.checked_div(n).unwrap_or(1).saturating_mul(n);
        loop {
            let v = u32::from(u16::from_le_bytes(draw()?));
            if v < limit {
                let i = usize::try_from(v.checked_rem(n).unwrap_or(0)).unwrap_or(0);
                return from
                    .get(i)
                    .copied()
                    .ok_or_else(|| ErrorInfo::new(ErrorCode::Internal, "the word list"));
            }
        }
    };
    loop {
        let typed = format!(
            "{}-{}-{}",
            pick(four_letter_words())?,
            pick(eff_words())?,
            pick(eff_words())?
        );
        let code = Code::new(typed);
        match on {
            Some((index, pool)) if code.relay_index(pool) != index => {}
            _ => return Ok(code),
        }
    }
}

/// A code as the user typed it: spaces between the parts become hyphens,
/// as croc's command line joins its arguments, and nothing else changes.
///
/// # Errors
///
/// [`ErrorCode::BadCode`]: too short or long, or not printable ASCII.
pub(crate) fn parse(typed: &str) -> Result<Code, ErrorInfo> {
    let joined = typed.split_whitespace().collect::<Vec<_>>().join("-");
    let ok = (MIN_CODE_BYTES..=MAX_CODE_BYTES).contains(&joined.len())
        && joined.bytes().all(|b| b.is_ascii_graphic());
    if !ok {
        return Err(ErrorInfo::new(ErrorCode::BadCode, "not a croc code"));
    }
    Ok(Code::new(joined))
}

/// The host of croc's link for receiving in a browser,
/// `https://getcroc.com/?code=<code>`, which croc 11 (and croc 10 since
/// 10.7) prints and puts in its QR codes.
const WEB_HOST: &str = "getcroc.com";

/// The croc code in a QR code's text (spec v0.6), or `None` when the text
/// is not croc's: [`Scanned::Croc`] for a code this client can receive
/// with, [`Scanned::Other`] for croc's link around one it cannot.
///
/// Two shapes are croc's. The link, over HTTPS to croc's own host, with
/// the code as its one query parameter and nothing else. And a code on its
/// own, but only one shaped as croc makes them: three or more words of `a`
/// to `z`, or croc 10's four digits and then words. croc takes any six
/// printable characters as a code, and the text of a QR code is seldom
/// one: a web address or a Wi-Fi network is not taken for a code.
pub(crate) fn from_qr(text: &str) -> Option<Scanned> {
    let code = match Url::parse(text) {
        Ok(url) if url.host_str() == Some(WEB_HOST) => web_code(&url),
        Ok(_) => return None,
        Err(_) if crate::scan::croc_words(text) => Some(text.to_owned()),
        Err(_) => return None,
    };
    Some(
        code.and_then(|c| parse(&c).ok())
            .map_or(Scanned::Other, |c| Scanned::Croc {
                code: c.to_string(),
            }),
    )
}

/// The code in croc's web link, if the link is exactly one.
fn web_code(url: &Url) -> Option<String> {
    let plain = url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.fragment().is_none();
    if !plain {
        return None;
    }
    let mut pairs = url.query_pairs();
    let (key, code) = pairs.next()?;
    (key == "code" && pairs.next().is_none()).then(|| code.into_owned())
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)] // Test scenes.
mod tests {
    use super::*;

    #[test]
    fn the_list_is_croc_s() {
        assert_eq!(eff_words().len(), 1296);
        assert_eq!(eff_words()[0], "acid");
        assert_eq!(eff_words()[1295], "zoom");
        assert_eq!(four_letter_words().len(), 432);
        assert!(is_eff_word("yo-yo"));
        let unique: HashSet<_> = eff_words().iter().collect();
        assert_eq!(unique.len(), 1296);
    }

    #[test]
    fn rooms_passwords_and_relays_are_croc_11_s() {
        // croc-v11-delta.md §1.1, from croc 11.5.4's own codephrase.
        let vectors = [
            (
                "8123-alpha-bravo-charlie",
                "1cdafe5c70a87f001f2f437fa6773c0ee726d10b7c3f86184dc68fa709c2f355",
                "alpha-bravo-charlie",
                0,
            ),
            (
                "acid-acorn-acre",
                "f72491c26f320da8a93ea323d8d23b4561e0634f967ba21cb268a1cd0df48a12",
                "acorn-acre",
                2,
            ),
            (
                "yo-yo-acid-acorn",
                "f8fbf63e42e2aaebb9962a27896c7c9fcd5ad3c3d3457bbc23a60e5cf4384266",
                "acid-acorn",
                3,
            ),
            (
                "abc-def-ghi",
                "aeb08bd382d1018e9369dcda6e10630fa1766be6c196ae02e2b52e21a40bef82",
                "def-ghi",
                0,
            ),
            (
                "alpha-bravo-charlie-delta",
                "82ed354962d360d22cd73b8448ee8f9793b069714a504ac1deabcee219dee7a1",
                "charlie-delta",
                1,
            ),
            (
                "Alpha-bravo-charlie",
                "94548cbeeba0c89fffade65a415449490867e8840b31f99fc57acf9f0df56ed1",
                "-bravo-charlie",
                3,
            ),
            (
                "hello-world",
                "dfe709f1615e56e943774c13c219257e9e725f62f3fd44629a57c210bc357f6c",
                "-world",
                1,
            ),
        ];
        for (typed, room, password, relay) in vectors {
            let code = parse(typed).unwrap();
            assert_eq!(code.room(), room, "{typed}");
            assert_eq!(code.password(), password.as_bytes(), "{typed}");
            assert_eq!(code.relay_index(4), relay, "{typed}");
        }
        // Five EFF words are the legacy split, whose room is the first
        // word's when it has four letters.
        let five = parse("acid-acorn-acre-afar-agent").unwrap();
        assert_eq!(five.room(), parse("acid-acorn-acre").unwrap().room());
        assert_eq!(five.password(), b"acorn-acre-afar-agent");
        assert_eq!(
            parse(" acid acorn  acre ").unwrap(),
            parse("acid-acorn-acre").unwrap()
        );
        assert!(parse("8123-alpha-bravo-charlie").unwrap().is_croc10s());
        assert!(!parse("acid-acorn-acre").unwrap().is_croc10s());
        assert!(!parse("81234-alpha").unwrap().is_croc10s());
        assert_eq!(
            format!("{:?}", parse("acid-acorn-acre").unwrap()),
            "Code(<15 bytes>)"
        );
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
    fn our_codes_suit_croc_11_and_croc_10_alike() {
        let mut seed = 7u32;
        let mut draw = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            Ok((u16::try_from(seed >> 16).unwrap()).to_le_bytes())
        };
        for relay in 0..4 {
            let code = generate_with(Some((relay, 4)), &mut draw).unwrap();
            let typed = code.to_string();
            let words: Vec<_> = typed.split('-').collect();
            assert!(words.len() >= 3 && words[0].len() == 4, "{typed}");
            assert_eq!(code.relay_index(4), relay, "{typed}");
            // croc 11 reads it as three EFF words ...
            assert_eq!(eff_sequence(&typed, 3).unwrap().len(), 3, "{typed}");
            // ... and croc 10's byte split finds the same room and password.
            let mut h = Sha256::new();
            h.update(&typed.as_bytes()[..4]);
            h.update(b"croc");
            assert_eq!(code.room(), sukkula_core::hex::encode(&h.finalize()));
            assert_eq!(code.password(), &typed.as_bytes()[5..]);
            assert_eq!(parse(&typed).unwrap(), code);
        }
        let any = generate(None).unwrap();
        assert!(eff_sequence(&any.to_string(), 3).is_some());
    }
}
