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

mod marks;

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
        // Every Unicode space separator (Zs), and BRAILLE PATTERN BLANK:
        // not a separator, but it draws nothing, so a run of it pads a name
        // until the label elides whatever follows (`photo.jpg⠀⠀⠀⠀.exe`).
        0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 | 0x2800 => Class::Space,
        // Bidirectional controls: embeddings, overrides, isolates, marks.
        0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069 => Class::Drop,
        // Every format character (Cf) of Unicode 16.0 and every
        // Default_Ignorable_Code_Point, which between them are the
        // characters that render as nothing: soft hyphen, combining grapheme
        // joiner, Arabic and Syriac format marks, Hangul fillers, Khmer
        // inherent vowels, Mongolian selectors, zero-width space/joiners,
        // word joiner and invisible operators, deprecated format characters,
        // variation selectors, BOM, interlinear annotation,
        // Kaithi/Egyptian/shorthand/musical format controls, and the tag and
        // variation-selector-supplement planes. Checked before the marks
        // below, because several of these are also Mn.
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
        // Every nonspacing and enclosing mark, in every script. Thai tone
        // marks and Hebrew cantillation stack as high as U+0300..U+036F do,
        // so the cap applies to all of them; no script needs more than
        // [`MAX_COMBINING_RUN`] on one base in ordinary writing.
        _ if is_mark(u) => Class::Combining,
        _ => Class::Keep,
    }
}

/// Whether `u` is a nonspacing (Mn) or enclosing (Me) mark.
fn is_mark(u: u32) -> bool {
    marks::MARKS
        .binary_search_by(|&(lo, hi)| {
            if hi < u {
                std::cmp::Ordering::Less
            } else if lo > u {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
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
        assert_eq!(truncate_bytes("€", 2), "");
        assert_eq!(truncate_bytes("", 0), "");
    }

    #[test]
    fn marks_table_is_sorted() {
        let mut prev: Option<u32> = None;
        for &(lo, hi) in marks::MARKS {
            assert!(lo <= hi, "{lo:X}..{hi:X}");
            assert!(hi <= 0x10_FFFF);
            if let Some(p) = prev {
                // Sorted, disjoint, and merged: adjacent ranges would be one.
                assert!(lo > p.saturating_add(1), "{lo:X} after {p:X}");
            }
            prev = Some(hi);
        }
    }

    #[test]
    fn marks_are_found_at_range_edges() {
        for (c, mark) in [
            ('\u{2FF}', false),
            ('\u{300}', true),
            ('\u{36F}', true),
            ('\u{370}', false),
            ('\u{E48}', true),  // Thai tone mark
            ('\u{591}', true),  // Hebrew accent
            ('\u{64B}', true),  // Arabic fathatan
            ('\u{20DD}', true), // enclosing circle
            ('\u{E01EF}', true),
            ('a', false),
            ('\u{0}', false),
            ('\u{10FFFF}', false),
        ] {
            assert_eq!(is_mark(u32::from(c)), mark, "{c:?}");
        }
    }

    #[test]
    fn script_mark_floods_are_cut_too() {
        // Thai: one consonant under a tower of tone marks, the classic
        // "overflowing text" trick. U+0E48 was not limited before.
        let thai = format!("\u{E2A}{}", "\u{E49}".repeat(60));
        assert_eq!(display(&thai, 64).chars().count(), 1 + MAX_COMBINING_RUN);
        let hebrew = format!("\u{5D0}{}", "\u{596}".repeat(60));
        assert_eq!(message(&hebrew).chars().count(), 1 + MAX_COMBINING_RUN);
        let arabic = format!("\u{628}{}", "\u{651}".repeat(60));
        assert_eq!(display(&arabic, 64).chars().count(), 1 + MAX_COMBINING_RUN);
        // Ordinary text with marks is untouched.
        assert_eq!(display("ภาษาไทย", 64), "ภาษาไทย");
        assert_eq!(display("שָׁלוֹם", 64), "שָׁלוֹם");
        assert_eq!(display("cafe\u{301}", 64), "cafe\u{301}");
    }

    #[test]
    fn marks_that_are_also_invisible_stay_dropped() {
        // CGJ, Mongolian and ordinary variation selectors, Khmer inherent
        // vowels are Mn as well as default-ignorable; they must stay Drop.
        for c in ['\u{34F}', '\u{180B}', '\u{FE0F}', '\u{E0100}', '\u{17B4}'] {
            assert_eq!(classify(c), Class::Drop, "{c:?}");
        }
    }

    #[test]
    fn blank_padding_collapses() {
        assert_eq!(classify('\u{2800}'), Class::Space);
        assert!(is_forbidden('\u{2800}'));
        assert_eq!(
            display("photo.jpg\u{2800}\u{2800}\u{2800}\u{2800}.exe", 64),
            "photo.jpg .exe"
        );
        assert_eq!(display("\u{2800}\u{3164}\u{FFA0}\u{115F}", 64), "");
    }

    #[test]
    fn classes_cover_the_categories() {
        assert_eq!(classify('a'), Class::Keep);
        assert_eq!(classify('\t'), Class::Space);
        assert_eq!(classify('\n'), Class::Newline);
        assert_eq!(classify('\u{85}'), Class::Newline);
        assert_eq!(classify('\u{2029}'), Class::Newline);
        assert_eq!(classify('\u{7F}'), Class::Drop);
        assert_eq!(classify('\u{E000}'), Class::Drop);
        assert_eq!(classify('\u{FDD0}'), Class::Drop);
        assert_eq!(classify('\u{1FFFE}'), Class::Drop);
        assert_eq!(classify('\u{10FFFF}'), Class::Drop);
        assert_eq!(classify('\u{301}'), Class::Combining);
        assert!(!is_forbidden(' '));
        assert!(!is_forbidden('\u{301}'));
        assert!(is_forbidden('\n'));
        assert!(is_forbidden('\u{A0}'));
        assert!(!is_forbidden_in_message('\n'));
        assert!(is_forbidden_in_message('\r'));
        assert!(is_forbidden_in_message('\u{A0}'));
        assert!(is_forbidden_in_message('\u{202E}'));
        assert!(!is_forbidden_in_message('\u{301}'));
    }

    #[test]
    fn display_edges() {
        // A space before the cut is not left dangling before the ellipsis.
        assert_eq!(display("abc defgh", 5), "abc…");
        // Room for exactly the space and one more.
        assert_eq!(display("ab cd", 4), "ab…");
        // Nothing showable left over is not "more": no ellipsis.
        assert_eq!(display("abc \u{200B}\t", 3), "abc");
        // A lone combining mark on a space survives, one on nothing does not.
        assert_eq!(display("a \u{301}", 64), "a \u{301}");
        assert_eq!(display(" \u{301}", 64), "");
        assert_eq!(display("abc", usize::MAX), "abc");
        // The cut falls on a mark: the mark goes, the base stays.
        assert_eq!(display("ab\u{301}c", 3), "ab…");
    }

    #[test]
    fn message_edges() {
        assert_eq!(message(""), "");
        assert_eq!(message("\r\r\n"), "");
        assert_eq!(message("a\r\r\nb"), "a\n\nb");
        assert_eq!(message("a \n b"), "a\nb");
        assert_eq!(message("a\n\u{301}b"), "a\nb");
        assert_eq!(message("\u{301}a"), "a");
        assert_eq!(message("a\u{A0}\u{3000}b"), "a b");
        // A space that would push the message over the cap is not added.
        let edge = format!("{} b", "a".repeat(MAX_MESSAGE_BYTES - 1));
        let m = message(&edge);
        assert_eq!(m.len(), MAX_MESSAGE_BYTES - 1);
        let edge = format!("{}\u{301}", "a".repeat(MAX_MESSAGE_BYTES - 1));
        assert_eq!(message(&edge).len(), MAX_MESSAGE_BYTES - 1);
    }
}
