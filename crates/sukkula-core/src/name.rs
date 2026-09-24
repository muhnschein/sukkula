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
//!    whitespace becomes single spaces ([`crate::text::classify`]).
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
#[must_use]
pub fn sanitize(raw: &str) -> SafeName {
    // 1. Last segment only. `rsplit` always yields at least one item.
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");

    // 2. Characters.
    let mut cleaned = String::with_capacity(last.len().min(MAX_NAME_BYTES.saturating_mul(4)));
    let mut pending_space = false;
    let mut combining: usize = 0;
    for c in last.chars() {
        // Bound the work on a hostile, huge name: nothing past this many
        // bytes can survive step 5 anyway, bar the extension, which is
        // taken from the raw tail below.
        if cleaned.len() > MAX_NAME_BYTES.saturating_mul(4) {
            break;
        }
        match classify(c) {
            Class::Drop => {}
            Class::Space | Class::Newline => pending_space = true,
            Class::Combining => {
                if !cleaned.is_empty() && combining < MAX_COMBINING_RUN {
                    combining = combining.saturating_add(1);
                    cleaned.push(c);
                }
            }
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
    if cleaned.len() > MAX_NAME_BYTES.saturating_mul(4)
        && let (_, Some(ext)) = split_extension(last)
    {
        let ext_clean = sanitize_extension(ext);
        if !ext_clean.is_empty() {
            cleaned.push('.');
            cleaned.push_str(&ext_clean);
        }
    }

    // 3 and 4. Dots and spaces at either end.
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
        ] {
            let once = sanitize(raw);
            assert_eq!(sanitize(once.as_str()), once);
        }
    }
}
