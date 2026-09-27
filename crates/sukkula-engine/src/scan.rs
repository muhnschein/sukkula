//! Codes from the camera (spec v0.6): the QR codes in a frame, and the
//! Magic Wormhole or croc code one of them holds, read as strictly as a
//! typed code.
//!
//! The shell grabs the viewfinder, turns it grey, and hands it to
//! `sukkula_scan_qr` (`sukkula-ffi`), which lands in [`scan`]. rqrr,
//! vendored with its grouping bounded (`third_party/rqrr.patches`), finds
//! and decodes the codes, and [`read`] takes a code out of what one says:
//!
//! - a `wormhole-transfer:` URI, which Warp, Destiny and Sukkula put in
//!   their QR codes: its code, checked as a typed one is, and the mailbox
//!   server it names, checked as one in Settings is
//!   (`wormhole::code::from_qr`);
//! - croc's link for receiving in a browser,
//!   `https://getcroc.com/?code=<code>`, which croc 11 prints, or a code
//!   on its own in croc's shape, as croc 10 and Sukkula put them in their
//!   QR codes (`croc::code::from_qr`).
//!
//! Anything else is [`Scanned::Other`], and what it says goes no further:
//! it is never shown, logged or opened (S2, S8). Nothing here touches the
//! network or a file; receiving is the UI's next command, and the
//! protocol's switch in Settings still decides it (F-C1).
//!
//! A frame is at most [`MAX_SIDE`] pixels a side. Codes printed light on
//! dark -- croc's in a light terminal -- are found too: a frame with no
//! code in it is read once more, inverted.
//!
//! A code the user types or pastes instead goes through [`typed`], which
//! tells the protocol from the code's shape as well: nobody has to say
//! whether it is a Magic Wormhole or a croc code.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::api::Scanned;

/// Longest side of a frame, in pixels, which the shell scales the
/// viewfinder to.
pub const MAX_SIDE: usize = 1024;

/// Largest distance between two rows' starts, in bytes.
pub const MAX_STRIDE: usize = 4 * MAX_SIDE;

/// Most codes read from one frame.
#[cfg(any(feature = "wormhole", feature = "croc"))]
const MAX_CODES: usize = 4;

/// Longest text taken from a code. A wormhole URI with a mailbox is a few
/// hundred bytes; a QR code holds up to 2953.
const MAX_TEXT_BYTES: usize = 1024;

/// A grey frame: `height` rows of `width` pixels, each row `stride` bytes
/// after the last, one byte of luma a pixel.
#[derive(Clone, Copy)]
// Without Magic Wormhole and croc there is nothing to look for in it.
#[cfg_attr(not(any(feature = "wormhole", feature = "croc")), allow(dead_code))]
pub struct Frame<'a> {
    luma: &'a [u8],
    width: usize,
    height: usize,
    stride: usize,
}

impl<'a> Frame<'a> {
    /// `None` unless both sides are 1 to [`MAX_SIDE`], the stride from the
    /// width to [`MAX_STRIDE`], and `luma` long enough for every row.
    #[must_use]
    pub fn new(luma: &'a [u8], width: usize, height: usize, stride: usize) -> Option<Frame<'a>> {
        let sides = (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height);
        if !sides || !(width..=MAX_STRIDE).contains(&stride) {
            return None;
        }
        let needed = Frame::bytes(width, height, stride)?;
        (luma.len() >= needed).then_some(Frame {
            luma,
            width,
            height,
            stride,
        })
    }

    /// The bytes a frame of this shape spans: every row but the last in
    /// full, and the last up to its last pixel.
    #[must_use]
    pub fn bytes(width: usize, height: usize, stride: usize) -> Option<usize> {
        stride
            .checked_mul(height.checked_sub(1)?)?
            .checked_add(width)
    }

    /// The pixel at `x`, `y`; white outside the frame.
    #[cfg(any(feature = "wormhole", feature = "croc"))]
    fn luma(&self, x: usize, y: usize) -> u8 {
        y.checked_mul(self.stride)
            .and_then(|row| row.checked_add(x))
            .and_then(|i| self.luma.get(i))
            .copied()
            .unwrap_or(u8::MAX)
    }
}

/// What the QR codes in `frame` hold: the first that is a code to receive
/// with, else [`Scanned::Other`] when a code was read at all, else `None`,
/// which is also what a code too blurred to read gives.
#[must_use]
pub fn scan(frame: &Frame<'_>) -> Option<Scanned> {
    // rqrr's own asserts are its; a frame that trips one is a frame with
    // nothing in it, not a failed app.
    catch_unwind(AssertUnwindSafe(|| texts(frame)))
        .ok()
        .flatten()
        .map(|texts| {
            texts
                .iter()
                .map(|t| read(t))
                .find(|s| *s != Scanned::Other)
                .unwrap_or(Scanned::Other)
        })
}

/// The texts of the codes in `frame`, dark on light or else light on dark;
/// `None` when it holds none that decodes.
#[cfg(any(feature = "wormhole", feature = "croc"))]
fn texts(frame: &Frame<'_>) -> Option<Vec<String>> {
    let decoded = |invert: bool| {
        let mut img =
            rqrr::PreparedImage::prepare_from_greyscale(frame.width, frame.height, |x, y| {
                let l = frame.luma(x, y);
                if invert { u8::MAX.wrapping_sub(l) } else { l }
            });
        let grids = img.detect_grids();
        let found = !grids.is_empty();
        let texts: Vec<String> = grids
            .iter()
            .take(MAX_CODES)
            .filter_map(|g| g.decode().ok())
            .map(|(_, text)| text)
            .collect();
        (found, texts)
    };
    let (found, texts) = decoded(false);
    let texts = if found { texts } else { decoded(true).1 };
    (!texts.is_empty()).then_some(texts)
}

/// Without Magic Wormhole or croc, no code is looked for.
#[cfg(not(any(feature = "wormhole", feature = "croc")))]
fn texts(_frame: &Frame<'_>) -> Option<Vec<String>> {
    None
}

/// Whether `text` is shaped as croc makes codes: three or more words of
/// `a` to `z` (croc 11's, one of whose EFF words is `yo-yo`), or croc 10's
/// four digits and then three or more such words.
#[must_use]
pub(crate) fn croc_words(text: &str) -> bool {
    let word = |w: &&str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase());
    let parts: Vec<&str> = text.split('-').collect();
    match parts.split_first() {
        Some((pin, words)) if pin.len() == 4 && pin.bytes().all(|b| b.is_ascii_digit()) => {
            words.len() >= 3 && words.iter().all(word)
        }
        _ => parts.len() >= 3 && parts.iter().all(word),
    }
}

/// Which protocol a code the user typed or pasted is for, and the code as
/// that protocol's receive takes it (spec v0.6); `None` for text that is
/// neither's. The two kinds look nothing alike, so the user need not say:
///
/// - what a QR code would give ([`read`]): a pasted `wormhole-transfer:`
///   URI, with its mailbox, croc's web link, or croc's words;
/// - croc's words, whatever their case, in lower case: croc makes no
///   capitals, and a phone keyboard makes the first one;
/// - a number, a hyphen and words of letters and digits: a Magic Wormhole
///   code, lowercased, as its receive reads it, which holds it to the rest
///   of the typed grammar;
/// - anything else croc takes, 6 to 128 printable characters, as typed:
///   a code the sender chose. Not a URL, nor a `wormhole-transfer:` URI
///   that did not read: those are no croc code anybody chose.
///
/// Spaces between the words are hyphens, as croc's command line joins
/// them and as people read codes aloud. A wormhole code with a four-digit
/// number and three or more words is taken for croc 10's: mailbox servers
/// hand out small numbers.
#[must_use]
pub fn typed(text: &str) -> Option<Scanned> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        return None;
    }
    let found = read(text);
    if found != Scanned::Other {
        return Some(found);
    }
    let lower_text = text.to_ascii_lowercase();
    if lower_text.contains("://") || lower_text.starts_with("wormhole-transfer:") {
        return None;
    }
    let joined = text.split_whitespace().collect::<Vec<_>>().join("-");
    let lower = joined.to_ascii_lowercase();
    let croc_length = (6..=128).contains(&joined.len());
    if croc_length && croc_words(&lower) {
        return Some(Scanned::Croc { code: lower });
    }
    let numbered = joined.split_once('-').is_some_and(|(nameplate, words)| {
        !nameplate.is_empty() && nameplate.bytes().all(|b| b.is_ascii_digit()) && !words.is_empty()
    });
    if numbered {
        // Only letters, digits and hyphens: what is left of the grammar,
        // and the library's entropy check, is the adapter's.
        let plain = lower
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        return plain.then_some(Scanned::Wormhole {
            code: lower,
            mailbox_url: None,
        });
    }
    (croc_length && joined.bytes().all(|b| b.is_ascii_graphic()))
        .then_some(Scanned::Croc { code: joined })
}

/// The code a QR code's text holds, or [`Scanned::Other`]. White space
/// around it is dropped; nothing else is changed but what reading a typed
/// code changes.
#[must_use]
pub fn read(text: &str) -> Scanned {
    let text = text.trim();
    if text.len() > MAX_TEXT_BYTES {
        return Scanned::Other;
    }
    #[cfg(feature = "wormhole")]
    if let Some(found) = crate::wormhole::code::from_qr(text) {
        return found;
    }
    #[cfg(feature = "croc")]
    if let Some(found) = crate::croc::code::from_qr(text) {
        return found;
    }
    Scanned::Other
}

#[cfg(test)]
#[cfg(all(feature = "wormhole", feature = "croc"))]
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)] // Test scenes.
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// A white frame of `w` by `h`.
    fn blank(w: usize, h: usize) -> Vec<u8> {
        vec![u8::MAX; w * h]
    }

    /// `text` as a QR code, `scale` pixels a module, its top left corner at
    /// `x`, `y` of a frame `w` wide.
    fn draw(frame: &mut [u8], w: usize, text: &str, scale: usize, x: usize, y: usize) {
        let q = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let size = q.width();
        for (i, c) in q.to_colors().iter().enumerate() {
            if *c != qrcode::Color::Dark {
                continue;
            }
            let (mx, my) = (i % size, i / size);
            for dy in 0..scale {
                for dx in 0..scale {
                    frame[(y + my * scale + dy) * w + x + mx * scale + dx] = 0;
                }
            }
        }
    }

    /// What a 480 by 360 frame with `text` in it as a QR code holds.
    fn scanned(text: &str) -> Option<Scanned> {
        let (w, h) = (480, 360);
        let mut f = blank(w, h);
        draw(&mut f, w, text, 5, 60, 20);
        scan(&Frame::new(&f, w, h, w).unwrap())
    }

    fn wormhole(code: &str, mailbox_url: Option<&str>) -> Option<Scanned> {
        Some(Scanned::Wormhole {
            code: code.into(),
            mailbox_url: mailbox_url.map(Into::into),
        })
    }

    fn croc(code: &str) -> Option<Scanned> {
        Some(Scanned::Croc { code: code.into() })
    }

    #[test]
    fn wormhole_uris_give_their_code_and_a_mailbox_that_is_not_the_default() {
        assert_eq!(
            scanned("wormhole-transfer:7-guitarist-revenge"),
            wormhole("7-guitarist-revenge", None)
        );
        assert_eq!(
            scanned(
                "wormhole-transfer:7-guitarist-revenge?rendezvous=wss%3A%2F%2Fmailbox.example%2Fv1"
            ),
            wormhole("7-guitarist-revenge", Some("wss://mailbox.example/v1"))
        );
        // The library's own default, named anyway, is no custom mailbox.
        assert_eq!(
            read(
                "wormhole-transfer:7-guitarist-revenge?rendezvous=ws%3A%2F%2Frelay.magic-wormhole.io%3A4000%2Fv1"
            ),
            Scanned::Wormhole {
                code: "7-guitarist-revenge".into(),
                mailbox_url: None
            }
        );
        // As a phone keyboard or a scanner may have it.
        assert_eq!(
            read(" WORMHOLE-TRANSFER:12-Guitarist-Revenge\n"),
            Scanned::Wormhole {
                code: "12-guitarist-revenge".into(),
                mailbox_url: None
            }
        );
    }

    #[test]
    fn croc_links_and_croc_shaped_words_give_their_code() {
        assert_eq!(
            scanned("https://getcroc.com/?code=gala-tulip-acorn"),
            croc("gala-tulip-acorn")
        );
        assert_eq!(scanned("gala-tulip-acorn"), croc("gala-tulip-acorn"));
        assert_eq!(
            scanned("8123-alpha-bravo-charlie"),
            croc("8123-alpha-bravo-charlie")
        );
        assert_eq!(
            read("yo-yo-gala-tulip"),
            Scanned::Croc {
                code: "yo-yo-gala-tulip".into()
            }
        );
        // croc's escaping of a code with spaces, which croc joins.
        assert_eq!(
            read("https://getcroc.com/?code=gala+tulip+acorn"),
            Scanned::Croc {
                code: "gala-tulip-acorn".into()
            }
        );
        assert_eq!(
            read("https://getcroc.com/?code=My%2FSecret%3Fcode"),
            Scanned::Croc {
                code: "My/Secret?code".into()
            }
        );
    }

    #[test]
    fn anything_else_is_other_and_says_nothing() {
        for text in [
            "https://example.org/",
            "WIFI:T:WPA;S:home;P:secret;;",
            "hello world",
            "Gala-Tulip-Acorn",
            "gala-tulip",
            "812-alpha-bravo-charlie",
            "8123-alpha-bravo",
            "wormhole-transfer:",
            "wormhole-transfer:guitarist-revenge",
            "wormhole-transfer:7-guitarist-revenge?role=leader",
            "wormhole-transfer:7-guitarist-revenge?version=1",
            "wormhole-transfer://host/7-guitarist-revenge",
            "wormhole-transfer:7-gu%C3%AFtarist-revenge",
            "wormhole-transfer:7-guitarist-revenge?rendezvous=http%3A%2F%2Fmailbox.example%2Fv1",
            "wormhole-transfer:7-guitarist-revenge?rendezvous=wss%3A%2F%2Fu%3Ap%40mailbox.example%2Fv1",
            "wormhole-transfer:7-guitarist-revenge?rendezvous=wss%3A%2F%2Fmailbox.example%2Fv1%3Fx%3D1",
            "http://getcroc.com/?code=gala-tulip-acorn",
            "https://getcroc.com:8443/?code=gala-tulip-acorn",
            "https://getcroc.com/receive?code=gala-tulip-acorn",
            "https://getcroc.com/?code=gala-tulip-acorn&relay=evil.example",
            "https://getcroc.com/?code=abc",
            "https://getcroc.com/#ssh?code=gala-tulip-acorn",
            "https://www.getcroc.com/?code=gala-tulip-acorn",
            "https://getcroc.com.evil.example/?code=gala-tulip-acorn",
        ] {
            assert_eq!(read(text), Scanned::Other, "{text:?}");
        }
        let long = format!(
            "wormhole-transfer:7-guitarist-revenge?x={}",
            "a".repeat(MAX_TEXT_BYTES)
        );
        assert_eq!(read(&long), Scanned::Other);
    }

    #[test]
    fn a_code_is_found_among_other_codes_and_light_on_dark() {
        let (w, h) = (640, 360);
        let mut f = blank(w, h);
        draw(&mut f, w, "https://example.org/menu", 4, 10, 10);
        draw(&mut f, w, "gala-tulip-acorn", 4, 330, 10);
        assert_eq!(
            scan(&Frame::new(&f, w, h, w).unwrap()),
            croc("gala-tulip-acorn")
        );

        let mut f = blank(480, 360);
        draw(
            &mut f,
            480,
            "wormhole-transfer:7-guitarist-revenge",
            5,
            60,
            20,
        );
        for p in &mut f {
            *p = u8::MAX - *p;
        }
        assert_eq!(
            scan(&Frame::new(&f, 480, 360, 480).unwrap()),
            wormhole("7-guitarist-revenge", None)
        );
        assert_eq!(scanned("https://example.org/"), Some(Scanned::Other));
        assert_eq!(scan(&Frame::new(&blank(64, 64), 64, 64, 64).unwrap()), None);
    }

    #[test]
    fn our_own_qr_codes_scan_as_the_code_they_show() {
        let shown = crate::wormhole::code::parse("7-guitarist-revenge").unwrap();
        let custom = "wss://mailbox.example/v1".parse().unwrap();
        for (mailbox, expected) in [
            (None, None),
            (Some(&custom), Some("wss://mailbox.example/v1")),
        ] {
            let q = crate::wormhole::code::qr(&shown, mailbox).unwrap();
            assert_eq!(
                scan_rows(&q.rows),
                wormhole("7-guitarist-revenge", expected)
            );
        }
        let q = crate::by_code::qr_code(b"gala-tulip-acorn").unwrap();
        assert_eq!(scan_rows(&q.rows), croc("gala-tulip-acorn"));
    }

    /// The rows of an event's QR code, drawn as the code page draws them:
    /// four pixels a module and four modules of quiet zone.
    fn scan_rows(rows: &[String]) -> Option<Scanned> {
        let size = rows.len();
        let side = (size + 8) * 4;
        let mut f = blank(side, side);
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.bytes().enumerate() {
                if c == b'1' {
                    for d in 0..16 {
                        f[((y + 4) * 4 + d / 4) * side + (x + 4) * 4 + d % 4] = 0;
                    }
                }
            }
        }
        scan(&Frame::new(&f, side, side, side).unwrap())
    }

    #[test]
    fn a_typed_code_says_which_protocol_it_is_for() {
        let w = |c: &str| {
            Some(Scanned::Wormhole {
                code: c.into(),
                mailbox_url: None,
            })
        };
        let c = |c: &str| Some(Scanned::Croc { code: c.into() });
        // Magic Wormhole: a number, then words, however spoken or typed.
        assert_eq!(typed("7-guitarist-revenge"), w("7-guitarist-revenge"));
        assert_eq!(typed("  7 Guitarist  revenge\n"), w("7-guitarist-revenge"));
        assert_eq!(typed("1234-guitarist-revenge"), w("1234-guitarist-revenge"));
        // croc 11 and croc 10, with a keyboard's capital undone.
        assert_eq!(typed("gala-tulip-acorn"), c("gala-tulip-acorn"));
        assert_eq!(typed("Gala tulip acorn"), c("gala-tulip-acorn"));
        assert_eq!(typed("yo-yo-gala-tulip"), c("yo-yo-gala-tulip"));
        assert_eq!(
            typed("8123 Alpha bravo charlie"),
            c("8123-alpha-bravo-charlie")
        );
        // A code a croc sender chose, kept as typed.
        assert_eq!(typed("MySecret!42"), c("MySecret!42"));
        assert_eq!(typed("my secret 42"), c("my-secret-42"));
        // Pasted as a QR code would carry them.
        assert_eq!(
            typed("wormhole-transfer:7-guitarist-revenge?rendezvous=wss%3A%2F%2Fm.example%2Fv1"),
            Some(Scanned::Wormhole {
                code: "7-guitarist-revenge".into(),
                mailbox_url: Some("wss://m.example/v1".into()),
            })
        );
        assert_eq!(
            typed("https://getcroc.com/?code=gala-tulip-acorn"),
            c("gala-tulip-acorn")
        );
        // Nobody's code.
        for text in [
            "",
            "   ",
            "abc",
            "https://example.org/menu",
            "HTTPS://getcroc.com/?code=x",
            "wormhole-transfer:guitarist",
            "7-guitarist-revenge\u{202e}x",
            &"a".repeat(129),
            &"a".repeat(MAX_TEXT_BYTES + 1),
        ] {
            assert_eq!(typed(text), None, "{text:?}");
        }
    }

    #[test]
    fn frames_are_held_to_their_shape() {
        let f = blank(100, 100);
        assert!(Frame::new(&f, 10, 10, 10).is_some());
        assert!(Frame::new(&f, 10, 9, 11).is_some());
        assert!(Frame::new(&f, 0, 10, 10).is_none());
        assert!(Frame::new(&f, 10, 0, 10).is_none());
        assert!(
            Frame::new(&f, 11, 10, 10).is_none(),
            "a stride under the width"
        );
        assert!(Frame::new(&f, 10, 10, MAX_STRIDE + 1).is_none());
        assert!(Frame::new(&f, 100, 100, 100).is_some());
        assert!(
            Frame::new(&f, 100, 100, 101).is_none(),
            "rows past the buffer"
        );
        assert!(Frame::new(&f, 1, 10_000, 1).is_none());
        let wide = blank(MAX_SIDE + 1, 1);
        assert!(Frame::new(&wide, MAX_SIDE + 1, 1, MAX_SIDE + 1).is_none());
        assert_eq!(Frame::bytes(10, 3, 12), Some(34));
        assert_eq!(Frame::bytes(10, 0, 12), None);
        assert_eq!(Frame::bytes(usize::MAX, 3, usize::MAX), None);
    }

    /// A frame tiled with finder patterns once kept rqrr's grouping busy
    /// for minutes (third_party/rqrr.patches/0002); now it is skipped at
    /// the cost of finding them.
    #[test]
    fn a_frame_full_of_finder_patterns_is_skipped_quickly() {
        let (w, h) = (640, 480);
        let mut f = blank(w, h);
        let mut count = 0;
        for ty in 0..h / 8 - 1 {
            for tx in 0..w / 8 - 1 {
                let (x0, y0) = (4 + tx * 8, 4 + ty * 8);
                for dy in 0..7 {
                    for dx in 0..7 {
                        let ring = dx == 0 || dx == 6 || dy == 0 || dy == 6;
                        let eye = (2..=4).contains(&dx) && (2..=4).contains(&dy);
                        if ring || eye {
                            f[(y0 + dy) * w + x0 + dx] = 0;
                        }
                    }
                }
                count += 1;
            }
        }
        assert!(count > 10 * rqrr::MAX_CAPSTONES);
        let started = Instant::now();
        assert_eq!(scan(&Frame::new(&f, w, h, w).unwrap()), None);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }
}
