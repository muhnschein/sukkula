//! S2: text a peer chose, made safe to show.
//!
//! Aliases, device models and messages lose every control, bidirectional
//! and invisible character, runs of whitespace collapse, stacks of combining
//! marks are cut short, and the result is length-capped. QML still shows
//! the result with `textFormat: Text.PlainText`; this module is what makes
//! the *plain* text honest, so that "Alice" cannot be followed by a
//! right-to-left override that makes a file called `gpj.exe` read `exe.jpg`.
//!
//! The same character classes are used for file names by [`crate::name`].

use crate::limits::{MAX_COMBINING_RUN, MAX_MESSAGE_BYTES};

/// What happens to one character on its way to the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Shown as it is.
    Keep,
    /// Shown as a single ordinary space, and collapsed with its neighbours.
    Space,
    /// A line break. Kept in messages, a space everywhere else.
    Newline,
    /// A combining mark: kept, but only [`MAX_COMBINING_RUN`] in a row.
    Combining,
    /// Removed.
    Drop,
}

/// Classifies one character. This is the whole of S2's character policy.
#[must_use]
pub fn classify(c: char) -> Class {
    let u = u32::from(c);
    match u {
        // Line breaks first: they are C0 controls too.
        0x0A | 0x0D | 0x2028 | 0x2029 | 0x85 => Class::Newline,
        0x09 => Class::Space,
        // C0 controls, DEL and C1 controls.
        0x00..=0x1F | 0x7F..=0x9F => Class::Drop,
        // Every Unicode space separator (Zs).
        0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 => Class::Space,
        // Bidirectional controls: embeddings, overrides, isolates, marks.
        0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069 => Class::Drop,
        // Zero-width and other invisible format characters (Cf), and the
        // default-ignorable code points that render as nothing: soft hyphen,
        // combining grapheme joiner, Arabic and Syriac format marks, Hangul
        // fillers, Khmer inherent vowels, Mongolian selectors, zero-width
        // space/joiners, word joiner and invisible operators, deprecated
        // format characters, variation selectors, BOM, interlinear
        // annotation, Kaithi/Egyptian/shorthand/musical format controls,
        // and the tag and variation-selector-supplement planes.
        0xAD
        | 0x034F
        | 0x0600..=0x0605
        | 0x06DD
        | 0x070F
        | 0x0890..=0x0891
        | 0x08E2
        | 0x115F
        | 0x1160
        | 0x17B4
        | 0x17B5
        | 0x180B..=0x180F
        | 0x200B..=0x200D
        | 0x2060..=0x2065
        | 0x206A..=0x206F
        | 0x3164
        | 0xFE00..=0xFE0F
        | 0xFEFF
        | 0xFFA0
        | 0xFFF0..=0xFFFB
        | 0x110BD
        | 0x110CD
        | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A
        | 0xE0000..=0xE0FFF => Class::Drop,
        // Private use: glyphs that mean whatever a font says they mean.
        0xE000..=0xF8FF | 0xF0000..=0x10FFFF => Class::Drop,
        // Noncharacters.
        0xFDD0..=0xFDEF => Class::Drop,
        _ if u & 0xFFFE == 0xFFFE => Class::Drop,
        // The combining blocks Zalgo text is built from. Scripts whose
        // letters are written with combining marks (Devanagari, Thai, ...)
        // use their own blocks and are not limited.
        0x0300..=0x036F
        | 0x0483..=0x0489
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F => Class::Combining,
        _ => Class::Keep,
    }
}

/// True for a character that S2 never lets through, in any output of this
/// module or of [`crate::name`]. Tests and fuzz targets assert on it.
#[must_use]
pub fn is_forbidden(c: char) -> bool {
    matches!(classify(c), Class::Drop | Class::Newline) || (c != ' ' && classify(c) == Class::Space)
}

/// One line of display text -- an alias, a device model, a file name for a
/// label -- at most `max_chars` characters long. Empty only when the input
/// had nothing showable in it.
///
/// Truncation ends with `…`, which counts towards `max_chars`.
#[must_use]
pub fn display(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    if max_chars == 0 {
        return out;
    }
    let mut chars: usize = 0;
    let mut pending_space = false;
    let mut combining: usize = 0;
    for c in raw.chars() {
        let kept = match classify(c) {
            Class::Drop => continue,
            Class::Space | Class::Newline => {
                pending_space = true;
                continue;
            }
            Class::Combining => {
                // A mark with nothing to sit on, or one too many.
                if out.is_empty() || combining >= MAX_COMBINING_RUN {
                    continue;
                }
                combining = combining.saturating_add(1);
                c
            }
            Class::Keep => {
                combining = 0;
                c
            }
        };
        let needed = if pending_space && !out.is_empty() {
            2
        } else {
            1
        };
        if chars.saturating_add(needed) > max_chars {
            // More to show than fits: make room for the ellipsis.
            while chars >= max_chars {
                if out.pop().is_none() {
                    break;
                }
                chars = chars.saturating_sub(1);
            }
            while out.ends_with(' ') {
                out.pop();
                chars = chars.saturating_sub(1);
            }
            out.push('…');
            return out;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
            chars = chars.saturating_add(1);
        }
        pending_space = false;
        out.push(kept);
        chars = chars.saturating_add(1);
    }
    out
}

/// True for a character that [`message`] never lets through.
#[must_use]
pub fn is_forbidden_in_message(c: char) -> bool {
    match classify(c) {
        Class::Drop => true,
        Class::Newline => c != '\n',
        Class::Space => c != ' ',
        Class::Keep | Class::Combining => false,
    }
}

/// A received text message (F-C4): multi-line plain text, at most
/// [`MAX_MESSAGE_BYTES`] bytes, with line breaks normalised to `\n`.
///
/// Everything [`display`] removes is removed here too, except line breaks.
/// Runs of more than two empty lines are cut to two, and the result is
/// trimmed.
#[must_use]
pub fn message(raw: &str) -> String {
    let mut out = String::new();
    let mut combining: usize = 0;
    let mut newlines: usize = 0;
    let mut pending_space = false;
    let mut prev_cr = false;
    for c in raw.chars() {
        let class = classify(c);
        // "\r\n" is one break.
        if prev_cr && c == '\n' {
            prev_cr = false;
            continue;
        }
        prev_cr = c == '\r';
        let piece = match class {
            Class::Drop => continue,
            Class::Space => {
                pending_space = true;
                continue;
            }
            Class::Newline => {
                pending_space = false;
                if out.is_empty() || newlines >= 3 {
                    continue;
                }
                newlines = newlines.saturating_add(1);
                combining = 0;
                '\n'
            }
            Class::Combining => {
                if out.is_empty() || combining >= MAX_COMBINING_RUN || newlines > 0 {
                    continue;
                }
                combining = combining.saturating_add(1);
                c
            }
            Class::Keep => {
                combining = 0;
                c
            }
        };
        if piece != '\n' {
            if pending_space && !out.is_empty() && !out.ends_with('\n') {
                if out.len().saturating_add(1) > MAX_MESSAGE_BYTES {
                    break;
                }
                out.push(' ');
            }
            pending_space = false;
            newlines = 0;
        }
        if out.len().saturating_add(piece.len_utf8()) > MAX_MESSAGE_BYTES {
            break;
        }
        out.push(piece);
    }
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    out
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a
/// character boundary.
#[must_use]
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(display("Alice's Jolla", 64), "Alice's Jolla");
        assert_eq!(message("hello\nworld"), "hello\nworld");
    }

    #[test]
    fn bidi_overrides_are_removed() {
        assert_eq!(display("photo\u{202E}gpj.exe", 64), "photogpj.exe");
        assert_eq!(display("a\u{2066}b\u{2069}c\u{200F}", 64), "abc");
    }

    #[test]
    fn invisible_characters_are_removed() {
        assert_eq!(display("Al\u{200B}ice\u{FEFF}\u{2060}", 64), "Alice");
        assert_eq!(display("\u{3164}\u{115F}", 64), "");
        assert_eq!(display("x\u{E0041}\u{E0042}", 64), "x");
    }

    #[test]
    fn controls_are_removed_and_whitespace_collapses() {
        assert_eq!(
            display("  a\u{0}\u{7}\u{1B}[31m  b\t\nc  ", 64),
            "a[31m b c"
        );
        assert_eq!(display("a\u{00A0}\u{3000}b", 64), "a b");
    }

    #[test]
    fn display_is_capped_with_an_ellipsis() {
        let s = display(&"x".repeat(100), 10);
        assert_eq!(s.chars().count(), 10);
        assert!(s.ends_with('…'));
        assert_eq!(display("exactly10!", 10), "exactly10!");
        assert_eq!(display("", 10), "");
        assert_eq!(display("abc", 0), "");
        assert_eq!(display("abcdef", 1), "…");
    }

    #[test]
    fn combining_stacks_are_cut() {
        let zalgo = format!("a{}", "\u{0301}".repeat(50));
        assert_eq!(display(&zalgo, 64).chars().count(), 1 + MAX_COMBINING_RUN);
        // A mark with no base is dropped.
        assert_eq!(display("\u{0301}a", 64), "a");
    }

    #[test]
    fn messages_keep_lines_but_not_blank_floods() {
        assert_eq!(message("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(message("a\n\n\n\n\n\nb"), "a\n\n\nb");
        assert_eq!(message("\n\n  a  \n"), "a");
        assert_eq!(message("x\u{2028}y"), "x\ny");
    }

    #[test]
    fn messages_are_capped_on_a_char_boundary() {
        let big = "é".repeat(MAX_MESSAGE_BYTES);
        let m = message(&big);
        assert!(m.len() <= MAX_MESSAGE_BYTES);
        assert!(m.chars().all(|c| c == 'é'));
    }

    #[test]
    fn truncate_bytes_respects_boundaries() {
        assert_eq!(truncate_bytes("héllo", 2), "h");
        assert_eq!(truncate_bytes("héllo", 3), "hé");
        assert_eq!(truncate_bytes("abc", 10), "abc");
    }
}
