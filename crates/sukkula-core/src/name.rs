//! S1: a peer-supplied file name, made into one safe path component.
//!
//! The result of [`sanitize`] is a [`SafeName`], and a `SafeName` is the only
//! thing [`crate::inbox`] will create a file from. So the type is the proof:
//! a name that has not been through here cannot reach the file system.
//!
//! The rules, in order:
//!
//! 1. Only the last segment survives: everything up to the final `/` or `\`
//!    is dropped, so `../../.bashrc` and `C:\x\y.txt` become `.bashrc` and
//!    `y.txt`.
//! 2. NUL, control, bidirectional and invisible characters are removed, and
//!    whitespace becomes single spaces ([`crate::text::classify`]). A
//!    combining mark is kept only on a base that is not a dot, and at most
//!    [`MAX_COMBINING_RUN`] in a row.
//! 3. Leading dots and spaces are removed: no hidden files, no `.` or `..`.
//! 4. Trailing dots and spaces are removed.
//! 5. The name is shortened to [`MAX_NAME_BYTES`] bytes on a character
//!    boundary, keeping an extension of up to [`MAX_EXTENSION_BYTES`] bytes.
//! 6. A name with nothing left becomes [`FALLBACK_NAME`].

use std::fmt;

use crate::limits::{MAX_COMBINING_RUN, MAX_EXTENSION_BYTES, MAX_NAME_BYTES};
use crate::text::{Class, classify, truncate_bytes};

/// What a name with nothing usable in it becomes.
pub const FALLBACK_NAME: &str = "received-file";

// `numbered` falls back to FALLBACK_NAME as the stem, next to the longest
// suffix (` (4294967295)`, 13 bytes) and the longest kept extension; all of
// it must fit the cap, or a numbered name could outgrow S1.
const _: () = assert!(
    FALLBACK_NAME.len() + 13 + 1 + MAX_EXTENSION_BYTES <= MAX_NAME_BYTES,
    "S1: the numbered fallback must fit MAX_NAME_BYTES"
);

/// One path component that satisfies S1. Construct it with [`sanitize`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SafeName(String);

impl SafeName {
    /// The name as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The part before the extension, and the extension without its dot.
    /// `("archive.tar", Some("gz"))` for `archive.tar.gz`; `("README", None)`.
    #[must_use]
    pub fn split_extension(&self) -> (&str, Option<&str>) {
        split_extension(&self.0)
    }

    /// The `n`th alternative spelling of this name, for when the plain one is
    /// taken: `photo (1).jpg`, `photo (2).jpg`, ... Still a `SafeName`: the
    /// suffix is ASCII and the stem is shortened to keep the byte cap.
    #[must_use]
    pub fn numbered(&self, n: u32) -> SafeName {
        if n == 0 {
            return self.clone();
        }
        let suffix = format!(" ({n})");
        let (stem, ext) = self.split_extension();
        let ext_len = ext.map_or(0, |e| e.len().saturating_add(1));
        let room = MAX_NAME_BYTES
            .saturating_sub(suffix.len())
            .saturating_sub(ext_len);
        let stem = truncate_bytes(stem, room).trim_end_matches([' ', '.']);
        let stem = if stem.is_empty() { FALLBACK_NAME } else { stem };
        let name = match ext {
            Some(e) => format!("{stem}{suffix}.{e}"),
            None => format!("{stem}{suffix}"),
        };
        SafeName(name)
    }
}

impl fmt::Display for SafeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for SafeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // S9: names are not logged at info level; Debug is for tests.
        write!(f, "SafeName({:?})", self.0)
    }
}

impl AsRef<std::path::Path> for SafeName {
    fn as_ref(&self) -> &std::path::Path {
        std::path::Path::new(&self.0)
    }
}

/// Applies S1 to a peer-supplied name.
///
/// Idempotent -- `sanitize(sanitize(x).as_str()) == sanitize(x)` -- so a
/// name checked at two boundaries reads the same as one checked at one; the
/// property tests and the `name_sanitize` fuzz target hold it to that.
#[must_use]
pub fn sanitize(raw: &str) -> SafeName {
    // 1. Last segment only. `rsplit` always yields at least one item.
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");

    // 2. Characters. Work is bounded on a hostile, huge name: nothing past
    // `SCAN_BYTES` of output can survive step 5 anyway, bar the extension,
    // which is taken from the raw tail below. Characters that are dropped do
    // not count, but they cost one `classify` each, so the loop stays linear
    // in the input, which the adapters already bound (S6).
    const SCAN_BYTES: usize = MAX_NAME_BYTES.saturating_mul(4);
    let mut cleaned = String::with_capacity(last.len().min(SCAN_BYTES));
    let mut pending_space = false;
    let mut combining: usize = 0;
    let mut cut = false;
    for c in last.chars() {
        if cleaned.len() > SCAN_BYTES {
            cut = true;
            break;
        }
        match classify(c) {
            Class::Drop => {}
            Class::Space | Class::Newline => pending_space = true,
            Class::Combining => {
                // A mark needs a base, and a dot is not one: a mark on a
                // trailing dot would hide that dot from step 4, and a mark
                // on a leading dot would be left without a base by step 3,
                // which is how `.\u{301}x` used to sanitise to a name that
                // sanitised differently again.
                let on_dot = cleaned.ends_with('.');
                if !cleaned.is_empty() && !on_dot && combining < MAX_COMBINING_RUN {
                    combining = combining.saturating_add(1);
                    cleaned.push(c);
                }
            }
            // 3. Leading dots never enter the name: no hidden files, no
            // `.` or `..`, and nothing left for a mark to hang from.
            Class::Keep if c == '.' && cleaned.is_empty() => {}
            Class::Keep => {
                if pending_space && !cleaned.is_empty() {
                    cleaned.push(' ');
                }
                pending_space = false;
                combining = 0;
                cleaned.push(c);
            }
        }
    }

    // A very long name loses its middle, not its extension.
    if cut && let (_, Some(ext)) = split_extension(last) {
        let ext_clean = sanitize_extension(ext);
        if !ext_clean.is_empty() {
            cleaned.push('.');
            cleaned.push_str(&ext_clean);
        }
    }

    // 3 and 4. Dots and spaces at either end. The start is already clean;
    // trimming it again costs nothing and keeps the rule in one place.
    let trimmed = cleaned
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);

    // 5. Length, keeping the extension.
    let shortened = shorten(trimmed);
    let shortened = shortened.trim_end_matches(['.', ' ']);

    // 6. Nothing left.
    if shortened.is_empty() {
        return SafeName(FALLBACK_NAME.to_owned());
    }
    SafeName(shortened.to_owned())
}

/// Whether `s` already satisfies S1. `sanitize(s) == s` for exactly these.
#[must_use]
pub fn is_safe(s: &str) -> bool {
    sanitize(s).as_str() == s
}

fn sanitize_extension(ext: &str) -> String {
    ext.chars()
        .filter(|c| classify(*c) == Class::Keep)
        .take(MAX_EXTENSION_BYTES)
        .collect::<String>()
        .chars()
        .scan(0usize, |len, c| {
            *len = len.saturating_add(c.len_utf8());
            (*len <= MAX_EXTENSION_BYTES).then_some(c)
        })
        .collect()
}

fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(0) | None => (name, None),
        Some(i) => {
            let stem = name.get(..i).unwrap_or(name);
            let ext = name.get(i.saturating_add(1)..).unwrap_or("");
            if ext.is_empty() || ext.len() > MAX_EXTENSION_BYTES || ext.contains(' ') {
                (name, None)
            } else {
                (stem, Some(ext))
            }
        }
    }
}

fn shorten(name: &str) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name.to_owned();
    }
    match split_extension(name) {
        (stem, Some(ext)) => {
            let room = MAX_NAME_BYTES.saturating_sub(ext.len().saturating_add(1));
            let stem = truncate_bytes(stem, room).trim_end_matches([' ', '.']);
            if stem.is_empty() {
                truncate_bytes(name, MAX_NAME_BYTES).to_owned()
            } else {
                format!("{stem}.{ext}")
            }
        }
        (_, None) => truncate_bytes(name, MAX_NAME_BYTES).to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(raw: &str) -> String {
        sanitize(raw).as_str().to_owned()
    }

    #[test]
    fn ordinary_names_survive() {
        assert_eq!(s("holiday.jpg"), "holiday.jpg");
        assert_eq!(s("Kesä 2026 (1).tar.gz"), "Kesä 2026 (1).tar.gz");
        assert_eq!(s("README"), "README");
    }

    #[test]
    fn traversal_is_reduced_to_the_last_segment() {
        assert_eq!(s("../../.bashrc"), "bashrc");
        assert_eq!(s("/etc/passwd"), "passwd");
        assert_eq!(s("C:\\Windows\\evil.dll"), "evil.dll");
        assert_eq!(s("a/b\\c/d.txt"), "d.txt");
        assert_eq!(s("dir/"), FALLBACK_NAME);
        assert_eq!(s(".."), FALLBACK_NAME);
        assert_eq!(s("."), FALLBACK_NAME);
        assert_eq!(s(""), FALLBACK_NAME);
        assert_eq!(s("..\\..\\"), FALLBACK_NAME);
    }

    #[test]
    fn hidden_and_trailing_dots_are_removed() {
        assert_eq!(s(".hidden"), "hidden");
        assert_eq!(s("...x"), "x");
        assert_eq!(s("name. . ."), "name");
        assert_eq!(s("  spaced  "), "spaced");
    }

    #[test]
    fn dangerous_characters_are_removed() {
        assert_eq!(s("a\0b"), "ab");
        assert_eq!(s("in\u{202E}gpj.exe"), "ingpj.exe");
        assert_eq!(s("zero\u{200B}width"), "zerowidth");
        assert_eq!(s("line\nbreak"), "line break");
        assert_eq!(s("\u{1B}[2Jclear"), "[2Jclear");
    }

    #[test]
    fn long_names_keep_their_extension() {
        let long = format!("{}.jpeg", "a".repeat(1000));
        let out = s(&long);
        assert!(out.len() <= MAX_NAME_BYTES);
        assert!(out.ends_with(".jpeg"));
        let multibyte = format!("{}.txt", "ä".repeat(300));
        let out = s(&multibyte);
        assert!(out.len() <= MAX_NAME_BYTES);
        assert!(out.ends_with(".txt"));
        let huge = format!("{}.pdf", "x".repeat(100_000));
        assert!(s(&huge).ends_with(".pdf"));
    }

    #[test]
    fn numbered_names_stay_safe() {
        let n = sanitize("photo.jpg");
        assert_eq!(n.numbered(0).as_str(), "photo.jpg");
        assert_eq!(n.numbered(3).as_str(), "photo (3).jpg");
        let long = sanitize(&format!("{}.jpg", "b".repeat(300)));
        let numbered = long.numbered(999);
        assert!(numbered.as_str().len() <= MAX_NAME_BYTES);
        assert!(numbered.as_str().ends_with(" (999).jpg"));
        assert!(is_safe(numbered.as_str()));
    }

    #[test]
    fn sanitize_is_idempotent_on_examples() {
        for raw in [
            "../x",
            ".a.",
            "a\u{202E}b",
            "x".repeat(500).as_str(),
            "  .  ",
            // Found by the property tests: a mark after a leading dot used
            // to survive the trim and lose its base, so the second pass
            // dropped it and gave a different name.
            ".\u{301}x",
            "..\u{301}\u{302}x",
            " .\u{20DD}y",
        ] {
            let once = sanitize(raw);
            assert_eq!(sanitize(once.as_str()), once, "{raw:?}");
        }
    }

    #[test]
    fn marks_cannot_hide_dots() {
        assert_eq!(s(".\u{301}x"), "x");
        // A trailing dot with a mark on it is still a trailing dot.
        assert_eq!(s("a.\u{301}"), "a");
        assert_eq!(s("a.\u{301}b"), "a.b");
        assert_eq!(s("\u{301}\u{301}"), FALLBACK_NAME);
        assert_eq!(s("e\u{301}.txt"), "e\u{301}.txt");
    }

    #[test]
    fn a_name_just_over_the_scan_limit_keeps_one_extension() {
        // 801 bytes: the scan did not stop early, and the extension used to
        // be appended a second time because the length alone was checked.
        let raw = format!("{}.pdf", "x".repeat(797));
        let out = s(&raw);
        assert!(out.ends_with(".pdf"));
        assert!(!out.ends_with(".pdf.pdf"));
        assert!(out.len() <= MAX_NAME_BYTES);
        // Cut mid-scan, with the extension in the unscanned tail.
        let raw = format!("{}.ab\u{202E}cd", "x".repeat(799));
        let out = s(&raw);
        assert!(out.ends_with(".abcd"), "{out}");
        assert!(out.len() <= MAX_NAME_BYTES);
    }

    #[test]
    fn extensions_are_only_what_looks_like_one() {
        let n = sanitize("archive.tar.gz");
        assert_eq!(n.split_extension(), ("archive.tar", Some("gz")));
        assert_eq!(sanitize("README").split_extension(), ("README", None));
        assert_eq!(sanitize("a.b c").split_extension(), ("a.b c", None));
        let long_ext = format!("a.{}", "e".repeat(MAX_EXTENSION_BYTES + 1));
        assert_eq!(sanitize(&long_ext).split_extension().1, None);
        // A long name with a long "extension" is cut as a whole.
        let raw = format!("{}.{}", "x".repeat(300), "e".repeat(40));
        assert!(s(&raw).len() <= MAX_NAME_BYTES);
    }

    #[test]
    fn huge_hostile_names_cost_linear_time() {
        // A megabyte of invisible characters: nothing survives, and the
        // loop does one classify per character.
        let raw = "\u{200B}".repeat(1 << 20);
        assert_eq!(s(&raw), FALLBACK_NAME);
        let raw = format!("{}{}", "\u{301}".repeat(1 << 18), "ok.txt");
        assert_eq!(s(&raw), "ok.txt");
    }

    #[test]
    fn safe_names_format_as_themselves() {
        let n = sanitize("a b.txt");
        assert_eq!(n.to_string(), "a b.txt");
        assert_eq!(format!("{n:?}"), "SafeName(\"a b.txt\")");
        let p: &std::path::Path = n.as_ref();
        assert_eq!(p, std::path::Path::new("a b.txt"));
        assert!(is_safe("a b.txt"));
        assert!(!is_safe(".x"));
        assert!(!is_safe("a/b"));
    }
}
