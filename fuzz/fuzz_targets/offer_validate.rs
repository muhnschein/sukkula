//! S1, S2, S4 and S6 together: a whole offer as a peer could describe it,
//! through `Offer::validate`, the only way to an `Offer`.
//!
//! The raw offer is built from the input with `arbitrary`, sizes as the
//! `i128` every adapter widens to, with the boundaries mixed in so that
//! 8 GiB, 16 GiB, 500 files and 64 KiB are reached rather than hoped for.
//! Asserted of everything that validates: no negative or oversized size, the
//! total is the exact sum and within the cap, at most 500 files, and every
//! name, sender, model, message, MIME type and PIN is clean. Asserted of
//! everything refused: the refusal names a real problem.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use sukkula_core::limits::{
    MAX_ALIAS_CHARS, MAX_FILE_BYTES, MAX_FILES_PER_OFFER, MAX_MESSAGE_BYTES, MAX_MIME_BYTES,
    MAX_MODEL_CHARS, MAX_OFFER_BYTES, MAX_PIN_CHARS,
};
use sukkula_core::offer::{Offer, OfferError, Protocol, RawFile, RawOffer};
use sukkula_fuzz::{assert_clean, oracle};

/// A size field: mostly boundaries, sometimes anything.
#[derive(Arbitrary, Debug)]
enum Size {
    Small(u16),
    NearFileCap(i8),
    NearHalfOfferCap(i8),
    Negative(i64),
    Wide(u64),
    Any(i128),
}

impl Size {
    fn value(&self) -> i128 {
        let file = i128::from(MAX_FILE_BYTES);
        match *self {
            Size::Small(n) => i128::from(n),
            Size::NearFileCap(d) => file + i128::from(d),
            Size::NearHalfOfferCap(d) => i128::from(MAX_OFFER_BYTES / 3) + i128::from(d),
            Size::Negative(n) => i128::from(n.min(-1)),
            Size::Wide(n) => i128::from(n),
            Size::Any(n) => n,
        }
    }
}

#[derive(Arbitrary, Debug)]
struct File {
    name: String,
    size: Size,
    mime: Option<String>,
    sha256: Option<String>,
}

#[derive(Arbitrary, Debug)]
struct Wire {
    protocol: u8,
    sender: String,
    model: Option<String>,
    files: Vec<File>,
    text: Option<String>,
    pin: Option<String>,
    /// Which paddings apply, each one time in sixteen: an offer of 500
    /// files or a 64 KiB message costs a hundred ordinary executions, so
    /// they are reached often enough without being most of the run.
    pads: u8,
    /// How many files to pad the list with, towards the 500 cap.
    extra_files: u16,
    /// How far past 64 KiB the padded message goes (0..8 bytes).
    text_over: u8,
}

fuzz_target!(|data: &[u8]| {
    let Ok(wire) = Wire::arbitrary(&mut Unstructured::new(data)) else {
        return;
    };
    let protocol = Protocol::ALL[usize::from(wire.protocol) % Protocol::ALL.len()];
    let mut raw = RawOffer::new(protocol, wire.sender);
    raw.model = wire.model;
    raw.pin = wire.pin;
    let pad_text = wire.pads & 0x0F == 0x01;
    let pad_files = wire.pads & 0xF0 == 0x10;
    raw.text = match wire.text {
        Some(t) if pad_text => {
            let fill = MAX_MESSAGE_BYTES.saturating_sub(t.len()).saturating_sub(4);
            Some(format!(
                "{}{t}{}",
                "m".repeat(fill),
                "x".repeat(usize::from(wire.text_over % 8))
            ))
        }
        t => t,
    };
    raw.files = wire
        .files
        .into_iter()
        .map(|f| RawFile {
            name: f.name,
            size: f.size.value(),
            mime: f.mime,
            sha256: f.sha256,
        })
        .collect();
    let extra = usize::from(wire.extra_files % 520);
    if pad_files && extra > 0 && raw.files.len() < MAX_FILES_PER_OFFER + 10 {
        let n = extra.min(MAX_FILES_PER_OFFER + 10 - raw.files.len());
        raw.files.extend((0..n).map(|i| RawFile {
            name: format!("{i}.bin"),
            size: 1,
            ..RawFile::default()
        }));
    }

    let sizes: Vec<i128> = raw.files.iter().map(|f| f.size).collect();
    let text_len = raw.text.as_ref().map(String::len);
    let file_count = raw.files.len();
    match Offer::validate(raw) {
        Ok(o) => {
            assert!(o.files.len() <= MAX_FILES_PER_OFFER);
            assert!(!o.files.is_empty() || o.text.is_some());
            let mut total: u64 = 0;
            for (f, raw_size) in o.files.iter().zip(&sizes) {
                assert!(*raw_size >= 0, "negative size accepted");
                assert!(f.size <= MAX_FILE_BYTES, "oversized file accepted");
                assert_eq!(i128::from(f.size), *raw_size);
                total = total.checked_add(f.size).expect("total overflowed");
                assert_clean(
                    "file name",
                    f.name.as_str(),
                    oracle::name_violation(f.name.as_str()),
                );
                if let Some(m) = &f.mime {
                    assert!(m.len() <= MAX_MIME_BYTES && m.contains('/'));
                    assert!(
                        m.bytes()
                            .all(|b| b.is_ascii_graphic() && !b.is_ascii_uppercase())
                    );
                }
            }
            assert_eq!(o.total_bytes, total);
            assert!(o.total_bytes <= MAX_OFFER_BYTES, "offer cap broken");
            assert!(!o.sender.is_empty());
            assert_clean(
                "sender",
                &o.sender,
                oracle::display_violation(&o.sender, MAX_ALIAS_CHARS),
            );
            if let Some(m) = &o.model {
                assert!(!m.is_empty());
                assert_clean("model", m, oracle::display_violation(m, MAX_MODEL_CHARS));
            }
            if let Some(t) = &o.text {
                assert!(!t.is_empty());
                assert_clean("text", t, oracle::message_violation(t));
            }
            if let Some(p) = &o.pin {
                assert!(!p.is_empty() && p.len() <= MAX_PIN_CHARS);
                assert!(p.bytes().all(|b| b.is_ascii_digit()));
            }
        }
        Err(e) => match e {
            OfferError::NegativeSize(i) => assert!(sizes[i] < 0),
            OfferError::FileTooLarge(i) => assert!(sizes[i] > i128::from(MAX_FILE_BYTES)),
            OfferError::TooManyFiles(n) => assert!(n == file_count && n > MAX_FILES_PER_OFFER),
            OfferError::OfferTooLarge => {
                let mut running = 0i128;
                assert!(sizes.iter().any(|&s| {
                    running = running.saturating_add(s);
                    running > i128::from(MAX_OFFER_BYTES)
                }));
            }
            OfferError::TextTooLarge => assert!(text_len.unwrap_or(0) > MAX_MESSAGE_BYTES),
            OfferError::Empty | OfferError::BadDigest(_) | OfferError::BadPin => {}
        },
    }
});
