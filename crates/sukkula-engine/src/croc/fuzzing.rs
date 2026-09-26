//! Entry points for the fuzz targets in `fuzz/` (spec §7 "Parsers"): the
//! croc parsers that eat bytes from the relay, the peer or the user,
//! reachable without a network. Each runs the real code and hands back
//! what it made of the input in plain types; none is used by the engine.

use sukkula_core::offer::RawOffer;

use super::code;
use super::crypt::{self, Cipher};
use super::message::{self, Message, SimpleMessage};
use super::pake::{Curve, Pake};
use super::relay;

/// Longest code accepted.
pub const CODE_BYTES: usize = code::MAX_CODE_BYTES;
/// Longest control message read, before and after inflating.
pub const CONTROL_BYTES: usize = message::MAX_CONTROL_BYTES;
/// Most data ports taken from a relay's banner.
pub const BANNER_PORTS: usize = relay::MAX_BANNER_PORTS;
/// Longest PAKE message read.
pub const PAKE_BYTES: usize = super::pake::MAX_PAKE_BYTES;
/// A chunk's size: what a request's ranges count in.
pub const CHUNK_BYTES: usize = message::CHUNK_BYTES;

/// A typed code: its room and password, if it is one.
#[must_use]
pub fn code(typed: &str) -> Option<(String, Vec<u8>)> {
    let c = code::parse(typed).ok()?;
    Some((c.room(), c.password().to_vec()))
}

/// A peer's or a relay's PAKE message, answered on each curve spoken: the
/// curve it says it is on, and whether each answer succeeded.
#[must_use]
pub fn pake(bytes: &[u8]) -> (Option<&'static str>, bool, bool) {
    let curve = Curve::of_message(bytes).map(Curve::name);
    let siec = Pake::answer(Curve::Siec, b"pw", [7; 32], bytes).is_ok();
    let p256 = Pake::answer(Curve::P256, b"pw", [7; 32], bytes).is_ok();
    // And as the answer to our own first message.
    for c in [Curve::Siec, Curve::P256] {
        if let Some(mut ours) = Pake::start(c, b"pw", [9; 32]) {
            let _ = ours.finish(bytes);
        }
    }
    (curve, siec, p256)
}

/// One control message as read: its kind (`Debug` of the engine's own),
/// `m`, `b` and `b2`.
pub type Read = (String, String, Vec<u8>, Vec<u8>);

/// What [`control`] makes of a frame, each way it can be read.
#[derive(Debug, Default)]
pub struct Control {
    /// Unsealed, as before the key.
    pub plain: Option<Read>,
    /// Sealed under a fixed key the fuzzer does not have.
    pub sealed: Option<Read>,
    /// As the LAN probe's message: its kind and bytes.
    pub probe: Option<(String, Vec<u8>)>,
}

fn read(m: Message) -> Read {
    (format!("{:?}", m.kind), m.m, m.b, m.b2)
}

/// A control frame, read unsealed, sealed under a fixed key, and as the
/// LAN probe's message.
#[must_use]
pub fn control(frame: &[u8]) -> Control {
    Control {
        plain: Message::decode(frame, None).map(read),
        sealed: Message::decode(frame, Some(&Cipher::new(&[3; 32]))).map(read),
        probe: SimpleMessage::decode(frame),
    }
}

/// The same control message sealed, so the fuzzer reaches past the seal:
/// `plain` deflated and sealed under the fixed key, then read back.
#[must_use]
pub fn sealed_control(plain: &[u8]) -> Option<Read> {
    let key = Cipher::new(&[3; 32]);
    let sealed = key.seal(&crypt::deflate(plain), [5; 12])?;
    Message::decode(&sealed, Some(&key)).map(read)
}

/// What [`file_list`] makes of a list.
#[derive(Debug)]
pub struct FileList {
    /// The one file is a text.
    pub text: bool,
    /// Chunks will be deflated.
    pub compressed: bool,
    /// Each file taken: its index in the sender's list, its name as sent,
    /// its size.
    pub files: Vec<(usize, String, u64)>,
    /// What the user is asked about, before `Offer::validate`.
    pub offer: RawOffer,
}

/// A sender's file list.
///
/// # Errors
///
/// Why the list is refused.
pub fn file_list(json: &[u8]) -> Result<FileList, String> {
    message::read_offer(json)
        .map(|o| FileList {
            text: o.text,
            compressed: o.compressed,
            files: o
                .files
                .iter()
                .map(|f| (f.index, f.name.clone(), f.size))
                .collect(),
            offer: super::receive::raw_offer(&o),
        })
        .map_err(|e| format!("{e:?}"))
}

/// A receiver's request against a list of `sizes`: the index and chunks.
#[must_use]
pub fn file_request(json: &[u8], sizes: &[u64]) -> Option<(usize, Vec<u64>)> {
    message::read_request(json, sizes).map(|r| (r.index, r.chunks))
}

/// A relay's banner, decrypted: the ports.
///
/// # Errors
///
/// Why it is refused.
pub fn banner(plain: &[u8]) -> Result<Vec<u16>, String> {
    relay::parse_banner(plain).map_err(|e| format!("{e:?}"))
}

/// `data` as our control messages deflate it.
#[must_use]
pub fn deflate(data: &[u8]) -> Vec<u8> {
    crypt::deflate(data)
}

/// A raw DEFLATE stream inflated under a cap.
#[must_use]
pub fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    crypt::inflate(data, limit)
}
