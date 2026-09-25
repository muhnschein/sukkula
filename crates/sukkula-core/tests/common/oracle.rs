//! A second statement of S1 and S2, for tests and fuzz targets to hold the
//! real one to.
//!
//! Deliberately written differently from `sukkula_core::text::classify`:
//! from the Unicode properties a reader acts on (controls, whitespace,
//! bidi controls, `Default_Ignorable_Code_Point`, format characters, private
//! use, noncharacters) spelled out from the UCD, not from the table the code
//! uses. A range dropped from that table then fails here instead of passing
//! by agreeing with itself. Shared by `tests/properties.rs`,
//! `tests/hostile.rs` and every target in `fuzz/`, through `#[path]`.

use std::path::{Component, Path};

use sukkula_core::limits::{MAX_COMBINING_RUN, MAX_MESSAGE_BYTES, MAX_NAME_BYTES};
use sukkula_core::text::{Class, classify};

/// Unicode 16.0 `Default_Ignorable_Code_Point`, from
/// `DerivedCoreProperties.txt`, range by range.
pub fn default_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// Unicode 16.0 general category Cf.
pub fn format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// Characters that draw nothing although they are neither of the above:
/// the Hangul fillers are in the list above already; BRAILLE PATTERN BLANK
/// is the one that is not.
pub fn blank(c: char) -> bool {
    c == '\u{2800}'
}

/// Private use and noncharacters: whatever a font makes of them.
pub fn private_or_nonchar(c: char) -> bool {
    let u = u32::from(c);
    matches!(u, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF | 0xFDD0..=0xFDEF) || (u & 0xFFFE) == 0xFFFE
}

/// Not allowed anywhere in S2 output: invisible, steering, a control, or
/// whitespace other than the plain space.
pub fn hides_or_steers(c: char) -> bool {
    c.is_control()
        || (c.is_whitespace() && c != ' ')
        || default_ignorable(c)
        || format_char(c)
        || blank(c)
        || private_or_nonchar(c)
}

/// Not allowed in a message: as above, but `\n` is how lines are kept.
pub fn hides_or_steers_in_message(c: char) -> bool {
    c != '\n' && hides_or_steers(c)
}

fn is_mark(c: char) -> bool {
    classify(c) == Class::Combining
}

/// The longest run of marks in `s`.
pub fn longest_mark_run(s: &str) -> usize {
    let mut longest = 0usize;
    let mut run = 0usize;
    for c in s.chars() {
        if is_mark(c) {
            run = run.saturating_add(1);
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// S2 for one line of display text capped at `max` characters. Returns
/// what is wrong, if anything.
pub fn display_violation(out: &str, max: usize) -> Option<String> {
    if let Some(c) = out.chars().find(|c| hides_or_steers(*c)) {
        return Some(format!("{c:?} survived"));
    }
    if out.chars().count() > max {
        return Some(format!("{} chars, over {max}", out.chars().count()));
    }
    if out.starts_with(' ') || out.ends_with(' ') || out.contains("  ") {
        return Some("stray spaces".into());
    }
    if out.chars().next().is_some_and(is_mark) {
        return Some("starts with a mark".into());
    }
    if longest_mark_run(out) > MAX_COMBINING_RUN {
        return Some("mark stack over the cap".into());
    }
    None
}

/// S2 for a received message.
pub fn message_violation(out: &str) -> Option<String> {
    if let Some(c) = out.chars().find(|c| hides_or_steers_in_message(*c)) {
        return Some(format!("{c:?} survived"));
    }
    if out.len() > MAX_MESSAGE_BYTES {
        return Some(format!("{} bytes", out.len()));
    }
    if out.starts_with([' ', '\n']) || out.ends_with([' ', '\n']) {
        return Some("not trimmed".into());
    }
    if out.contains("  ") || out.contains(" \n") || out.contains("\n ") || out.contains("\n\n\n\n")
    {
        return Some("whitespace not collapsed".into());
    }
    let mut prev = None;
    for c in out.chars() {
        if is_mark(c) && matches!(prev, None | Some('\n')) {
            return Some("a mark without a base".into());
        }
        prev = Some(c);
    }
    if longest_mark_run(out) > MAX_COMBINING_RUN {
        return Some("mark stack over the cap".into());
    }
    None
}

/// S1 for a sanitised name.
pub fn name_violation(out: &str) -> Option<String> {
    if out.is_empty() {
        return Some("empty".into());
    }
    if out.len() > MAX_NAME_BYTES {
        return Some(format!("{} bytes", out.len()));
    }
    if let Some(c) = out
        .chars()
        .find(|c| matches!(c, '/' | '\\' | '\0') || hides_or_steers(*c))
    {
        return Some(format!("{c:?} survived"));
    }
    if out.starts_with('.') || out.starts_with(' ') {
        return Some("leading dot or space".into());
    }
    if out.ends_with('.') || out.ends_with(' ') {
        return Some("trailing dot or space".into());
    }
    if out.contains("  ") {
        return Some("double space".into());
    }
    if out.chars().next().is_some_and(is_mark) {
        return Some("starts with a mark".into());
    }
    // A mark never sits on a dot, where it would hide a trailing one.
    let mut prev = None;
    for c in out.chars() {
        if is_mark(c) && prev == Some('.') {
            return Some("a mark on a dot".into());
        }
        prev = Some(c);
    }
    if longest_mark_run(out) > MAX_COMBINING_RUN {
        return Some("mark stack over the cap".into());
    }
    let mut components = Path::new(out).components();
    if !matches!(components.next(), Some(Component::Normal(n)) if *n == *out)
        || components.next().is_some()
    {
        return Some("not one normal path component".into());
    }
    None
}
