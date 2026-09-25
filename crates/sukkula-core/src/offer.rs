//! S4 and S6: an incoming offer, checked before anyone looks at it.
//!
//! Adapters turn whatever their protocol sent into a [`RawOffer`], keeping
//! every value as the peer sent it -- sizes as signed 128-bit integers so
//! that a negative `i64` or an oversized `u64` survives the conversion to be
//! rejected here rather than wrapping on the way. [`Offer::validate`] is the
//! only way to an [`Offer`], and an `Offer` is what the consent dialog and
//! the inbox work from.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hex;
use crate::limits::{
    MAX_ALIAS_CHARS, MAX_FILE_BYTES, MAX_FILES_PER_OFFER, MAX_MESSAGE_BYTES, MAX_MIME_BYTES,
    MAX_MODEL_CHARS, MAX_OFFER_BYTES, MAX_PIN_CHARS,
};
use crate::name::{self, SafeName};
use crate::text;

/// The four protocols.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// LocalSend v2.
    LocalSend,
    /// Quick Share (Nearby Share) over the LAN.
    QuickShare,
    /// Magic Wormhole v1.
    Wormhole,
    /// Bluetooth OBEX Object Push.
    Bluetooth,
}

impl Protocol {
    /// Every protocol, in the order the UI lists them.
    pub const ALL: [Protocol; 4] = [
        Protocol::LocalSend,
        Protocol::QuickShare,
        Protocol::Wormhole,
        Protocol::Bluetooth,
    ];
}

/// What shows in place of a sender with no showable name.
pub const UNKNOWN_SENDER: &str = "Unknown device";

/// One file as the peer described it. Nothing here has been checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawFile {
    /// The name the peer gave.
    pub name: String,
    /// The size the peer declared, widened without loss.
    pub size: i128,
    /// The MIME type the peer declared.
    pub mime: Option<String>,
    /// The SHA-256 the peer declared, as hex.
    pub sha256: Option<String>,
}

/// An offer as the peer described it. Nothing here has been checked.
#[derive(Clone, PartialEq, Eq)]
pub struct RawOffer {
    /// Which protocol it came over.
    pub protocol: Protocol,
    /// The sender's alias or device name.
    pub sender: String,
    /// The sender's device model, if the protocol carries one.
    pub model: Option<String>,
    /// The files offered.
    pub files: Vec<RawFile>,
    /// A text message offered, if any.
    pub text: Option<String>,
    /// A PIN to show so the user can compare it with the sender's screen
    /// (Quick Share's handshake PIN, F-QS3).
    pub pin: Option<String>,
}

impl RawOffer {
    /// An empty offer from `sender` over `protocol`.
    #[must_use]
    pub fn new(protocol: Protocol, sender: impl Into<String>) -> Self {
        RawOffer {
            protocol,
            sender: sender.into(),
            model: None,
            files: Vec::new(),
            text: None,
            pin: None,
        }
    }
}

/// One file of a checked offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferFile {
    /// The name, after S1.
    pub name: SafeName,
    /// The size, within [`MAX_FILE_BYTES`].
    pub size: u64,
    /// The MIME type, if it was well-formed.
    pub mime: Option<String>,
    /// The digest to verify against, if the sender gave one.
    pub sha256: Option<[u8; 32]>,
}

/// A checked offer: everything the consent dialog shows and the inbox
/// enforces.
#[derive(Clone, PartialEq, Eq)]
pub struct Offer {
    /// Which protocol it came over.
    pub protocol: Protocol,
    /// The sender, after S2. Never empty.
    pub sender: String,
    /// The sender's model, after S2.
    pub model: Option<String>,
    /// The files, at most [`MAX_FILES_PER_OFFER`].
    pub files: Vec<OfferFile>,
    /// The sum of the file sizes, within [`MAX_OFFER_BYTES`].
    pub total_bytes: u64,
    /// The message, after S2, if any.
    pub text: Option<String>,
    /// The PIN to compare, digits only.
    pub pin: Option<String>,
}

// S9: the message and the PIN are the user's and the handshake's, so a
// `{:?}` of an offer -- in a log line, a panic message -- gives the
// message's length and whether there is a PIN, not what they say.
fn redacted_text(text: Option<&String>) -> Option<String> {
    text.map(|t| format!("<{} bytes>", t.len()))
}

fn redacted_pin(pin: Option<&String>) -> Option<&'static str> {
    pin.map(|_| "<redacted>")
}

impl std::fmt::Debug for RawOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawOffer")
            .field("protocol", &self.protocol)
            .field("sender", &self.sender)
            .field("model", &self.model)
            .field("files", &self.files)
            .field("text", &redacted_text(self.text.as_ref()))
            .field("pin", &redacted_pin(self.pin.as_ref()))
            .finish()
    }
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer")
            .field("protocol", &self.protocol)
            .field("sender", &self.sender)
            .field("model", &self.model)
            .field("files", &self.files)
            .field("total_bytes", &self.total_bytes)
            .field("text", &redacted_text(self.text.as_ref()))
            .field("pin", &redacted_pin(self.pin.as_ref()))
            .finish()
    }
}

/// Why an offer was refused before it reached the user.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OfferError {
    /// No files and no text.
    #[error("the offer is empty")]
    Empty,
    /// More than [`MAX_FILES_PER_OFFER`] files.
    #[error("the offer has {0} files, more than the {max} allowed", max = MAX_FILES_PER_OFFER)]
    TooManyFiles(usize),
    /// A size below zero.
    #[error("file {0} declares a negative size")]
    NegativeSize(usize),
    /// A file over [`MAX_FILE_BYTES`].
    #[error("file {0} is larger than the limit")]
    FileTooLarge(usize),
    /// The files together over [`MAX_OFFER_BYTES`].
    #[error("the offer is larger than the limit")]
    OfferTooLarge,
    /// A message over [`MAX_MESSAGE_BYTES`].
    #[error("the message is larger than the limit")]
    TextTooLarge,
    /// A digest that is not 64 hex digits.
    #[error("file {0} declares a malformed digest")]
    BadDigest(usize),
    /// A PIN that is not 1 to [`MAX_PIN_CHARS`] ASCII digits.
    #[error("the offer carries a malformed PIN")]
    BadPin,
}

impl Offer {
    /// Checks a raw offer against S1, S2, S4 and S6.
    ///
    /// # Errors
    ///
    /// The first rule the offer breaks. Adapters decline such an offer at
    /// the protocol level; it never reaches the consent queue.
    pub fn validate(raw: RawOffer) -> Result<Offer, OfferError> {
        if raw.files.is_empty() && raw.text.as_deref().is_none_or(str::is_empty) {
            return Err(OfferError::Empty);
        }
        if raw.files.len() > MAX_FILES_PER_OFFER {
            return Err(OfferError::TooManyFiles(raw.files.len()));
        }
        if raw
            .text
            .as_ref()
            .is_some_and(|t| t.len() > MAX_MESSAGE_BYTES)
        {
            return Err(OfferError::TextTooLarge);
        }

        let mut total: u64 = 0;
        let mut files = Vec::with_capacity(raw.files.len());
        for (i, f) in raw.files.into_iter().enumerate() {
            if f.size < 0 {
                return Err(OfferError::NegativeSize(i));
            }
            let size = u64::try_from(f.size).map_err(|_| OfferError::FileTooLarge(i))?;
            if size > MAX_FILE_BYTES {
                return Err(OfferError::FileTooLarge(i));
            }
            total = total.checked_add(size).ok_or(OfferError::OfferTooLarge)?;
            if total > MAX_OFFER_BYTES {
                return Err(OfferError::OfferTooLarge);
            }
            let sha256 = match f.sha256.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(h) => Some(hex::decode_32(h).ok_or(OfferError::BadDigest(i))?),
            };
            files.push(OfferFile {
                name: name::sanitize(&f.name),
                size,
                mime: f.mime.as_deref().and_then(clean_mime),
                sha256,
            });
        }

        let pin = match raw.pin {
            None => None,
            Some(p) => {
                let ok = !p.is_empty()
                    && p.chars().count() <= MAX_PIN_CHARS
                    && p.chars().all(|c| c.is_ascii_digit());
                if !ok {
                    return Err(OfferError::BadPin);
                }
                Some(p)
            }
        };

        let sender = text::display(&raw.sender, MAX_ALIAS_CHARS);
        let sender = if sender.is_empty() {
            UNKNOWN_SENDER.to_owned()
        } else {
            sender
        };
        let model = raw
            .model
            .map(|m| text::display(&m, MAX_MODEL_CHARS))
            .filter(|m| !m.is_empty());
        let text = raw
            .text
            .map(|t| text::message(&t))
            .filter(|t| !t.is_empty());
        if files.is_empty() && text.is_none() {
            return Err(OfferError::Empty);
        }

        Ok(Offer {
            protocol: raw.protocol,
            sender,
            model,
            files,
            total_bytes: total,
            text,
            pin,
        })
    }
}

/// A MIME type is `type/subtype`, both RFC 6838 restricted names. Anything
/// else is dropped; the offer is not refused over it.
fn clean_mime(m: &str) -> Option<String> {
    // Parameters (`; charset=utf-8`) are dropped, not refused, and before
    // the length check, so a long parameter does not cost a good type.
    let m = m.split(';').next().unwrap_or("").trim();
    if m.is_empty() || m.len() > MAX_MIME_BYTES {
        return None;
    }
    let (ty, sub) = m.split_once('/')?;
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    (token(ty) && token(sub))
        .then(|| format!("{}/{}", ty.to_ascii_lowercase(), sub.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: i128) -> RawFile {
        RawFile {
            name: name.into(),
            size,
            mime: None,
            sha256: None,
        }
    }

    fn offer(files: Vec<RawFile>) -> RawOffer {
        RawOffer {
            files,
            ..RawOffer::new(Protocol::LocalSend, "Alice")
        }
    }

    #[test]
    fn a_plain_offer_validates() {
        let o = Offer::validate(offer(vec![file("a.jpg", 10), file("../b.txt", 20)])).unwrap();
        assert_eq!(o.total_bytes, 30);
        assert_eq!(o.files[1].name.as_str(), "b.txt");
        assert_eq!(o.sender, "Alice");
    }

    #[test]
    fn negative_and_huge_sizes_are_refused() {
        assert_eq!(
            Offer::validate(offer(vec![file("a", -1)])),
            Err(OfferError::NegativeSize(0))
        );
        assert_eq!(
            Offer::validate(offer(vec![file("a", i128::from(i64::MIN))])),
            Err(OfferError::NegativeSize(0))
        );
        assert_eq!(
            Offer::validate(offer(vec![file("a", i128::from(u64::MAX))])),
            Err(OfferError::FileTooLarge(0))
        );
        let over = i128::from(MAX_FILE_BYTES) + 1;
        assert_eq!(
            Offer::validate(offer(vec![file("a", over)])),
            Err(OfferError::FileTooLarge(0))
        );
        assert_eq!(
            Offer::validate(offer(vec![file("a", i128::MAX)])),
            Err(OfferError::FileTooLarge(0))
        );
    }

    #[test]
    fn the_total_is_capped() {
        let max = i128::from(MAX_FILE_BYTES);
        let o = Offer::validate(offer(vec![file("a", max), file("b", max)])).unwrap();
        assert_eq!(o.total_bytes, MAX_OFFER_BYTES);
        assert_eq!(
            Offer::validate(offer(vec![file("a", max), file("b", max), file("c", 1)])),
            Err(OfferError::OfferTooLarge)
        );
    }

    #[test]
    fn file_count_is_capped() {
        let files = (0..=MAX_FILES_PER_OFFER)
            .map(|i| file(&i.to_string(), 1))
            .collect();
        assert_eq!(
            Offer::validate(offer(files)),
            Err(OfferError::TooManyFiles(MAX_FILES_PER_OFFER + 1))
        );
    }

    #[test]
    fn empty_offers_are_refused() {
        assert_eq!(Offer::validate(offer(vec![])), Err(OfferError::Empty));
        let mut o = offer(vec![]);
        o.text = Some("\u{200B}\u{202E}".into());
        assert_eq!(Offer::validate(o), Err(OfferError::Empty));
    }

    #[test]
    fn digests_must_be_well_formed() {
        let mut f = file("a", 1);
        f.sha256 = Some("zz".into());
        assert_eq!(
            Offer::validate(offer(vec![f.clone()])),
            Err(OfferError::BadDigest(0))
        );
        f.sha256 = Some("ab".repeat(32));
        assert_eq!(
            Offer::validate(offer(vec![f])).unwrap().files[0].sha256,
            Some([0xAB; 32])
        );
    }

    #[test]
    fn pins_are_digits() {
        let mut o = offer(vec![file("a", 1)]);
        o.pin = Some("1234".into());
        assert_eq!(
            Offer::validate(o.clone()).unwrap().pin.as_deref(),
            Some("1234")
        );
        o.pin = Some("12a4".into());
        assert_eq!(Offer::validate(o.clone()), Err(OfferError::BadPin));
        o.pin = Some(String::new());
        assert_eq!(Offer::validate(o), Err(OfferError::BadPin));
    }

    #[test]
    fn senders_are_sanitised_and_never_empty() {
        let mut o = offer(vec![file("a", 1)]);
        o.sender = "\u{202E}\u{200B}".into();
        assert_eq!(Offer::validate(o.clone()).unwrap().sender, UNKNOWN_SENDER);
        o.sender = "x".repeat(1000);
        assert_eq!(
            Offer::validate(o).unwrap().sender.chars().count(),
            MAX_ALIAS_CHARS
        );
    }

    #[test]
    fn mime_types_are_checked() {
        assert_eq!(clean_mime("image/JPEG"), Some("image/jpeg".into()));
        assert_eq!(
            clean_mime("text/plain; charset=utf-8"),
            Some("text/plain".into())
        );
        assert_eq!(clean_mime("image"), None);
        assert_eq!(clean_mime("a/b\nc"), None);
        assert_eq!(clean_mime(&format!("a/{}", "b".repeat(200))), None);
    }

    #[test]
    fn debug_hides_the_message_and_the_pin() {
        let mut raw = offer(vec![file("a.txt", 1)]);
        raw.text = Some("meet me at the quay".into());
        raw.pin = Some("4821".into());
        let checked = Offer::validate(raw.clone()).unwrap();
        for shown in [format!("{raw:?}"), format!("{checked:?}")] {
            assert!(
                !shown.contains("quay") && !shown.contains("4821"),
                "{shown}"
            );
            assert!(shown.contains("<19 bytes>") && shown.contains("<redacted>"));
            assert!(shown.contains("a.txt"), "{shown}");
        }
    }
}
