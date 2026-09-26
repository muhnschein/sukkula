//! What croc peers say to each other (`src/message/message.go`,
//! `src/croc/croc.go`).
//!
//! A control message is JSON `{"t", "m", "b", "b2"}`, raw DEFLATE, and --
//! from the second PAKE message on -- sealed. `b` and `b2` are base64,
//! and for a file list or a file request `b` holds JSON of its own. Go
//! ignores fields and types it does not know, and so does this; everything
//! is read under a size cap, and what the protocol lets a peer choose --
//! names, sizes, counts, indices, ranges -- is checked here before anyone
//! acts on it. Names are then S1's (`sukkula_core::offer`).
//!
//! The LAN probe's messages are different: plain JSON
//! `{"Bytes", "Kind"}`, neither compressed nor sealed.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::crypt::{self, Cipher};
use super::random;
use crate::api::{ErrorCode, ErrorInfo};

/// Longest control frame read, and the most one inflates to. A list of
/// 500 files is a hundred kilobytes.
pub(super) const MAX_CONTROL_BYTES: usize = 1024 * 1024;

/// Bytes of file in one chunk: croc's `TCP_BUFFER_SIZE / 2`.
pub(super) const CHUNK_BYTES: usize = 32 * 1024;

/// Longest data frame read: a sealed chunk and its position, compressed
/// or not, with room to spare.
pub(super) const MAX_DATA_FRAME: usize = 64 * 1024;

/// A chunk's plaintext: its position, then at most [`CHUNK_BYTES`].
pub(super) const MAX_CHUNK_PLAIN: usize = CHUNK_BYTES + 8;

/// The kinds of control message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// A PAKE message, the only kind sent before the key.
    Pake,
    /// Each side's address as the relay saw it; informational.
    ExternalIp,
    /// The sender's file list.
    FileInfo,
    /// The receiver asks for one file.
    RecipientReady,
    /// The receiver has a whole file.
    CloseSender,
    /// The sender's answer to that.
    CloseRecipient,
    /// Everything is done.
    Finished,
    /// One side gives up; `m` says why.
    Error,
    /// A kind this build does not know: ignored, as Go ignores it.
    Other,
}

impl Kind {
    fn wire(self) -> &'static str {
        match self {
            Kind::Pake => "pake",
            Kind::ExternalIp => "externalip",
            Kind::FileInfo => "fileinfo",
            Kind::RecipientReady => "recipientready",
            Kind::CloseSender => "close-sender",
            Kind::CloseRecipient => "close-recipient",
            Kind::Finished => "finished",
            Kind::Error => "error",
            Kind::Other => "",
        }
    }

    fn from_wire(t: &str) -> Kind {
        [
            Kind::Pake,
            Kind::ExternalIp,
            Kind::FileInfo,
            Kind::RecipientReady,
            Kind::CloseSender,
            Kind::CloseRecipient,
            Kind::Finished,
            Kind::Error,
        ]
        .into_iter()
        .find(|k| k.wire() == t)
        .unwrap_or(Kind::Other)
    }
}

/// One control message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Message {
    pub(super) kind: Kind,
    pub(super) m: String,
    pub(super) b: Vec<u8>,
    pub(super) b2: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct MessageWire {
    t: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    m: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    b: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    b2: Option<String>,
}

impl Message {
    /// A message of `kind` with nothing in it.
    pub(super) fn new(kind: Kind) -> Message {
        Message {
            kind,
            m: String::new(),
            b: Vec::new(),
            b2: Vec::new(),
        }
    }

    /// As it goes on the wire: JSON, deflated, sealed if there is a key.
    ///
    /// # Errors
    ///
    /// No randomness for the nonce.
    pub(super) fn encode(&self, cipher: Option<&Cipher>) -> Result<Vec<u8>, ErrorInfo> {
        let b64 = |v: &[u8]| (!v.is_empty()).then(|| STANDARD.encode(v));
        let wire = MessageWire {
            t: self.kind.wire().to_owned(),
            m: self.m.clone(),
            b: b64(&self.b),
            b2: b64(&self.b2),
        };
        let json = serde_json::to_vec(&wire)
            .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "a croc message"))?;
        let packed = crypt::deflate(&json);
        match cipher {
            None => Ok(packed),
            Some(c) => c
                .seal(&packed, random()?)
                .ok_or_else(|| ErrorInfo::new(ErrorCode::Internal, "sealing failed")),
        }
    }

    /// A message off the wire, or `None` if it does not open, inflate
    /// within [`MAX_CONTROL_BYTES`] or parse.
    pub(super) fn decode(frame: &[u8], cipher: Option<&Cipher>) -> Option<Message> {
        if frame.len() > MAX_CONTROL_BYTES {
            return None;
        }
        let opened;
        let packed = match cipher {
            Some(c) => {
                opened = c.open(frame)?;
                opened.as_slice()
            }
            None => frame,
        };
        let json = crypt::inflate(packed, MAX_CONTROL_BYTES)?;
        let wire: MessageWire = serde_json::from_slice(&json).ok()?;
        let unb64 = |v: Option<String>| match v {
            None => Some(Vec::new()),
            Some(s) => STANDARD.decode(s).ok(),
        };
        Some(Message {
            kind: Kind::from_wire(&wire.t),
            m: wire.m,
            b: unb64(wire.b)?,
            b2: unb64(wire.b2)?,
        })
    }
}

/// The LAN probe's message: `{"Bytes": <base64>, "Kind": "pake1"}`.
#[derive(Serialize, Deserialize)]
pub(super) struct SimpleMessage {
    #[serde(rename = "Bytes")]
    bytes: String,
    #[serde(rename = "Kind")]
    pub(super) kind: String,
}

impl SimpleMessage {
    /// A probe message of `kind` carrying `bytes`.
    pub(super) fn encode(kind: &str, bytes: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&SimpleMessage {
            bytes: STANDARD.encode(bytes),
            kind: kind.to_owned(),
        })
        .unwrap_or_default()
    }

    /// `(kind, bytes)` of a probe message, if the frame is one.
    pub(super) fn decode(frame: &[u8]) -> Option<(String, Vec<u8>)> {
        if frame.len() > super::pake::MAX_PAKE_BYTES.saturating_mul(2) {
            return None;
        }
        let m: SimpleMessage = serde_json::from_slice(frame).ok()?;
        Some((m.kind, STANDARD.decode(m.bytes).ok()?))
    }
}

// ---- The file list --------------------------------------------------------

/// A file as the sender describes it (`FileInfo`, with its short tags).
#[derive(Serialize, Deserialize, Default)]
struct FileWire<'a> {
    #[serde(rename = "n", default)]
    name: String,
    #[serde(rename = "fr", default)]
    folder: String,
    #[serde(rename = "h", default, skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(rename = "s", default)]
    size: i64,
    #[serde(rename = "m", borrow, default, skip_serializing_if = "Option::is_none")]
    mtime: Option<&'a RawValue>,
    #[serde(rename = "sy", default, skip_serializing_if = "String::is_empty")]
    symlink: String,
    #[serde(rename = "md", default, skip_serializing_if = "Option::is_none")]
    mode: Option<u32>,
}

/// The sender's file list (`SenderInfo`, with Go's field names).
#[derive(Serialize, Deserialize, Default)]
struct SenderWire<'a> {
    #[serde(rename = "FilesToTransfer", borrow, default)]
    files: Option<Vec<FileWire<'a>>>,
    #[serde(rename = "TotalNumberFolders", default)]
    folders: i64,
    #[serde(rename = "MachineID", default)]
    machine: String,
    #[serde(rename = "Ask", default)]
    ask: bool,
    #[serde(rename = "SendingText", default)]
    text: bool,
    #[serde(rename = "NoCompress", default)]
    no_compress: bool,
    #[serde(rename = "HashAlgorithm", default)]
    hash: String,
    #[serde(rename = "ReconnectVersion", default)]
    reconnect: i64,
    #[serde(rename = "NextReconnectRoom", default)]
    next_room: String,
}

/// One file of an offer, as the receiver acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Offered {
    /// Its index in the sender's list: what a request names.
    pub(super) index: usize,
    /// The name the sender gave; S1 comes after.
    pub(super) name: String,
    /// Its size.
    pub(super) size: u64,
    /// Its XXH64, for a file that has bytes.
    pub(super) hash: Option<[u8; 8]>,
}

/// A checked file list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Offer {
    /// The files, symbolic links left out (they are never requested).
    pub(super) files: Vec<Offered>,
    /// The one file is a text.
    pub(super) text: bool,
    /// Chunks are deflated.
    pub(super) compressed: bool,
}

/// Why a file list is not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OfferError {
    /// Not a file list.
    Malformed,
    /// Hashed with something other than XXH64.
    Hash,
    /// Nothing to receive in it.
    Empty,
    /// A text that is not one small file.
    Text,
}

/// Longest name, folder or link read at all; S1 then has its say.
const MAX_PATH_BYTES: usize = 4096;

/// The sender's list, checked.
pub(super) fn read_offer(json: &[u8]) -> Result<Offer, OfferError> {
    let wire: SenderWire<'_> = serde_json::from_slice(json).map_err(|_| OfferError::Malformed)?;
    if !(wire.hash.is_empty() || wire.hash == "xxhash") {
        return Err(OfferError::Hash);
    }
    let mut files = Vec::new();
    for (index, f) in wire.files.unwrap_or_default().into_iter().enumerate() {
        if f.name.len() > MAX_PATH_BYTES
            || f.folder.len() > MAX_PATH_BYTES
            || f.symlink.len() > MAX_PATH_BYTES
        {
            return Err(OfferError::Malformed);
        }
        if !f.symlink.is_empty() {
            continue;
        }
        let size = u64::try_from(f.size).map_err(|_| OfferError::Malformed)?;
        let hash = match (size, f.hash) {
            (0, _) => None,
            (_, Some(h)) => Some(
                STANDARD
                    .decode(h)
                    .ok()
                    .and_then(|v| <[u8; 8]>::try_from(v).ok())
                    .ok_or(OfferError::Malformed)?,
            ),
            (_, None) => return Err(OfferError::Malformed),
        };
        files.push(Offered {
            index,
            name: f.name,
            size,
            hash,
        });
    }
    if files.is_empty() {
        return Err(OfferError::Empty);
    }
    if wire.text
        && (files.len() != 1
            || files.first().is_none_or(|f| {
                f.size > u64::try_from(sukkula_core::limits::MAX_MESSAGE_BYTES).unwrap_or(0)
            }))
    {
        return Err(OfferError::Text);
    }
    Ok(Offer {
        files,
        text: wire.text,
        compressed: !wire.no_compress,
    })
}

/// A file we send, for the list.
pub(super) struct Sending<'a> {
    pub(super) name: &'a str,
    pub(super) size: u64,
    pub(super) hash: [u8; 8],
}

/// Our file list: the files in `./`, hashed with XXH64, chunks not
/// deflated, no reconnection, no machine id, no mtime (Go leaves a zero
/// time alone).
pub(super) fn write_offer(files: &[Sending<'_>], text: bool) -> Vec<u8> {
    let zero = RawValue::from_string("\"0001-01-01T00:00:00Z\"".to_owned()).ok();
    let wire = SenderWire {
        files: Some(
            files
                .iter()
                .map(|f| FileWire {
                    name: f.name.to_owned(),
                    folder: "./".to_owned(),
                    hash: Some(STANDARD.encode(f.hash)),
                    size: i64::try_from(f.size).unwrap_or(i64::MAX),
                    mtime: zero.as_deref(),
                    symlink: String::new(),
                    mode: Some(0o644),
                })
                .collect(),
        ),
        text,
        no_compress: true,
        hash: "xxhash".to_owned(),
        ..SenderWire::default()
    };
    serde_json::to_vec(&wire).unwrap_or_default()
}

// ---- File requests --------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct RequestWire {
    #[serde(rename = "CurrentFileChunkRanges", default)]
    ranges: Option<Vec<i64>>,
    #[serde(rename = "FilesToTransferCurrentNum", default)]
    index: i64,
    #[serde(rename = "MachineID", default)]
    machine: String,
    #[serde(rename = "ReconnectVersion", default)]
    reconnect: i64,
}

/// A request for the whole of file `index`: what our receiver always asks.
pub(super) fn write_request(index: usize) -> Vec<u8> {
    serde_json::to_vec(&RequestWire {
        ranges: Some(Vec::new()),
        index: i64::try_from(index).unwrap_or(i64::MAX),
        machine: String::new(),
        reconnect: 0,
    })
    .unwrap_or_default()
}

/// A checked request: which file, and which of its chunks, each once, in
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub(super) index: usize,
    pub(super) chunks: Vec<u64>,
}

/// A receiver's request against our list of `sizes`: an index in range
/// of a file with bytes, and either no ranges (the whole file) or
/// `[32768, start, count, …]` of aligned chunks inside the file, none
/// twice. Go's own sender panics on a range without its count; this
/// refuses it.
pub(super) fn read_request(json: &[u8], sizes: &[u64]) -> Option<Request> {
    let wire: RequestWire = serde_json::from_slice(json).ok()?;
    let index = usize::try_from(wire.index).ok()?;
    let size = *sizes.get(index)?;
    if size == 0 {
        return None;
    }
    let chunk = u64::try_from(CHUNK_BYTES).ok()?;
    let count = size.div_ceil(chunk);
    let ranges = wire.ranges.unwrap_or_default();
    let Some((&first, pairs)) = ranges.split_first() else {
        return Some(Request {
            index,
            chunks: (0..count).collect(),
        });
    };
    if u64::try_from(first).ok()? != chunk || pairs.is_empty() || !pairs.len().is_multiple_of(2) {
        return None;
    }
    let mut chunks = Vec::new();
    for pair in pairs.chunks_exact(2) {
        let [start, n] = pair else { return None };
        let start = u64::try_from(*start).ok()?;
        let n = u64::try_from(*n).ok()?;
        if !start.is_multiple_of(chunk) || n == 0 {
            return None;
        }
        let from = start.checked_div(chunk)?;
        let to = from.checked_add(n)?;
        if to > count || chunks.last().is_some_and(|l| *l >= from) {
            return None;
        }
        chunks.extend(from..to);
    }
    Some(Request { index, chunks })
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;
    use crate::croc::{hexs, unhex};

    #[test]
    fn go_s_messages_read() {
        // croc-protocol.md A.6: Go's deflated JSON.
        let go = unhex(
            "002700d8ff7b2274223a2270616b65222c2262223a226533303d222c226232223a22634449314e673d3d227d010000ffff",
        );
        let m = Message::decode(&go, None).unwrap();
        assert_eq!(m.kind, Kind::Pake);
        assert_eq!(m.b, b"{}");
        assert_eq!(m.b2, b"p256");
        let m = Message::decode(
            &unhex("001000efff7b2274223a2266696e6973686564227d010000ffff"),
            None,
        )
        .unwrap();
        assert_eq!(m, Message::new(Kind::Finished));
        // Unknown kinds and fields are let by, as Go lets them by.
        let odd = crypt::deflate(br#"{"t":"newkind","n":3,"x":[1]}"#);
        assert_eq!(Message::decode(&odd, None).unwrap().kind, Kind::Other);
    }

    #[test]
    fn messages_round_trip_sealed_or_not() {
        let c = Cipher::new(&[4; 32]);
        let mut m = Message::new(Kind::Error);
        m.m = "refusing files".into();
        m.b = vec![0, 1, 2];
        for cipher in [None, Some(&c)] {
            let wire = m.encode(cipher).unwrap();
            assert_eq!(Message::decode(&wire, cipher).unwrap(), m);
        }
        let sealed = m.encode(Some(&c)).unwrap();
        assert!(Message::decode(&sealed, None).is_none());
        assert!(Message::decode(&sealed, Some(&Cipher::new(&[5; 32]))).is_none());
        let json =
            String::from_utf8(crypt::inflate(&m.encode(None).unwrap(), 1024).unwrap()).unwrap();
        assert_eq!(json, r#"{"t":"error","m":"refusing files","b":"AAEC"}"#);
        // Bad base64, not JSON, or too long.
        for bad in [&br#"{"t":"pake","b":"!!"}"#[..], b"[]", b"{\"m\":1}"] {
            assert!(
                Message::decode(&crypt::deflate(bad), None).is_none(),
                "{bad:?}"
            );
        }
        let big = crypt::deflate(&vec![b' '; MAX_CONTROL_BYTES + 1]);
        assert!(Message::decode(&big, None).is_none());
    }

    #[test]
    fn probe_messages() {
        let wire = SimpleMessage::encode("pake2", b"xy");
        assert_eq!(wire, br#"{"Bytes":"eHk=","Kind":"pake2"}"#);
        assert_eq!(
            SimpleMessage::decode(br#"{"Bytes":"eHk=","Kind":"pake1"}"#),
            Some(("pake1".into(), b"xy".to_vec()))
        );
        assert_eq!(SimpleMessage::decode(b"handshake"), None);
    }

    #[test]
    fn go_s_file_list_reads() {
        // croc-protocol.md A.7, as Go's json.Marshal writes it.
        let go = br#"{"FilesToTransfer":[{"n":"a.txt","fr":"./","fs":"/home/u","h":"AQIDBAUGBwg=","s":12,"m":"2026-01-02T03:04:05.123456789+02:00","md":420},
            {"n":"link","fr":"dir/","fs":"/home/u/dir","h":"MThiN2NiMDk5YTllYTNmNTBiYTg5OWI1YmE4MWUwZDM3N2E1ZjNiMTZmOGY2ZWViOGIzZTU4Y2Q0NjkyYjk5Mw==","s":5,"m":"2026-01-02T03:04:05.123456789+02:00","sy":"../a.txt","md":134218239},
            {"n":"empty","fr":"dir/","s":0,"m":"0001-01-01T00:00:00Z"}],
            "EmptyFoldersToTransfer":[{"fr":"dir/empty/","m":"0001-01-01T00:00:00Z"}],
            "TotalNumberFolders":2,"MachineID":"abc","Ask":false,"SendingText":false,"NoCompress":false,"HashAlgorithm":"xxhash","ReconnectVersion":1,"NextReconnectRoom":"00ff"}"#;
        let offer = read_offer(go).unwrap();
        assert!(offer.compressed && !offer.text);
        assert_eq!(
            offer.files,
            vec![
                Offered {
                    index: 0,
                    name: "a.txt".into(),
                    size: 12,
                    hash: Some([1, 2, 3, 4, 5, 6, 7, 8])
                },
                Offered {
                    index: 2,
                    name: "empty".into(),
                    size: 0,
                    hash: None
                },
            ],
            "the link is left out, and indices kept"
        );
    }

    #[test]
    fn file_lists_that_do_not_hold_are_refused() {
        let base = r#"{"FilesToTransfer":[{"n":"a","fr":"./","h":"AQIDBAUGBwg=","s":12}],"HashAlgorithm":"xxhash"}"#;
        assert!(read_offer(base.as_bytes()).is_ok());
        for (from, to, why) in [
            ("xxhash", "md5", OfferError::Hash),
            ("xxhash", "imohash", OfferError::Hash),
            ("\"s\":12", "\"s\":-1", OfferError::Malformed),
            ("\"s\":12", "\"s\":1e3", OfferError::Malformed),
            ("AQIDBAUGBwg=", "AQID", OfferError::Malformed),
            (",\"h\":\"AQIDBAUGBwg=\"", "", OfferError::Malformed),
            ("[{", "[{\"sy\":\"x\",", OfferError::Empty),
            (
                "[{\"n\":\"a\",\"fr\":\"./\",\"h\":\"AQIDBAUGBwg=\",\"s\":12}]",
                "null",
                OfferError::Empty,
            ),
        ] {
            let bad = base.replacen(from, to, 1);
            assert_ne!(bad, base, "{from}");
            assert_eq!(read_offer(bad.as_bytes()).unwrap_err(), why, "{to}");
        }
        let text = base.replace(
            "\"HashAlgorithm\"",
            "\"SendingText\":true,\"HashAlgorithm\"",
        );
        assert!(read_offer(text.as_bytes()).unwrap().text);
        let big_text = base.replace("\"s\":12", "\"s\":70000").replace(
            "\"HashAlgorithm\"",
            "\"SendingText\":true,\"HashAlgorithm\"",
        );
        assert_eq!(
            read_offer(big_text.as_bytes()).unwrap_err(),
            OfferError::Text
        );
        let dup = r#"{"FilesToTransfer":[],"FilesToTransfer":[]}"#;
        assert_eq!(
            read_offer(dup.as_bytes()).unwrap_err(),
            OfferError::Malformed
        );
        let long = base.replace("\"n\":\"a\"", &format!("\"n\":\"{}\"", "a".repeat(5000)));
        assert_eq!(
            read_offer(long.as_bytes()).unwrap_err(),
            OfferError::Malformed
        );
    }

    #[test]
    fn our_file_list_is_go_s_shape() {
        let json = write_offer(
            &[Sending {
                name: "a.txt",
                size: 12,
                hash: [1, 2, 3, 4, 5, 6, 7, 8],
            }],
            false,
        );
        let text = String::from_utf8(json.clone()).unwrap();
        assert!(text.starts_with(r#"{"FilesToTransfer":[{"n":"a.txt","fr":"./","h":"AQIDBAUGBwg=","s":12,"m":"0001-01-01T00:00:00Z","md":420}],"#), "{text}");
        assert!(text.contains(r#""NoCompress":true,"HashAlgorithm":"xxhash","ReconnectVersion":0,"NextReconnectRoom":"""#), "{text}");
        let back = read_offer(&json).unwrap();
        assert!(!back.compressed);
        assert_eq!(back.files[0].hash, Some([1, 2, 3, 4, 5, 6, 7, 8]));
    }

    #[test]
    fn requests() {
        let sizes = [100_000u64, 0, 32_768];
        assert_eq!(
            String::from_utf8(write_request(2)).unwrap(),
            r#"{"CurrentFileChunkRanges":[],"FilesToTransferCurrentNum":2,"MachineID":"","ReconnectVersion":0}"#
        );
        let whole = |j: &str| read_request(j.as_bytes(), &sizes);
        assert_eq!(
            whole(r#"{"CurrentFileChunkRanges":null,"FilesToTransferCurrentNum":0}"#),
            Some(Request {
                index: 0,
                chunks: vec![0, 1, 2, 3]
            })
        );
        assert_eq!(
            whole(r#"{"CurrentFileChunkRanges":[],"FilesToTransferCurrentNum":2}"#),
            Some(Request {
                index: 2,
                chunks: vec![0]
            })
        );
        // croc-protocol.md §6.9's example shape.
        assert_eq!(
            whole(
                r#"{"CurrentFileChunkRanges":[32768,0,2,98304,1],"FilesToTransferCurrentNum":0}"#
            ),
            Some(Request {
                index: 0,
                chunks: vec![0, 1, 3]
            })
        );
        for bad in [
            r#"{"FilesToTransferCurrentNum":1}"#,
            r#"{"FilesToTransferCurrentNum":3}"#,
            r#"{"FilesToTransferCurrentNum":-1}"#,
            r#"{"CurrentFileChunkRanges":[32768,0],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[32768],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[4096,0,1],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[32768,1,1],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[32768,0,5],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[32768,0,0],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[32768,32768,1,0,1],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[32768,0,2,32768,1],"FilesToTransferCurrentNum":0}"#,
            r#"{"CurrentFileChunkRanges":[32768,0,9223372036854775807],"FilesToTransferCurrentNum":0}"#,
        ] {
            assert_eq!(whole(bad), None, "{bad}");
        }
        let _ = hexs(&[]);
    }
}
