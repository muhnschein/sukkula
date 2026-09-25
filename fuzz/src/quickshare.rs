//! A Quick Share sender for the Quick Share targets to play, and the
//! receiving side as the adapter drives it.
//!
//! rqs_lib's [`InboundRequest`] is the real parser: every frame goes
//! through its [`FrameReader`](rqs_lib::hdl::frame::FrameReader) from a
//! socket, then its state machine, UKEY2, the D2D channel, byte payload
//! reassembly and the introduction. What a fuzzer cannot produce by itself
//! is a frame that passes the channel's HMAC, so the sender here seals what
//! the input asks for, under keys it shares with the receiver; everything
//! inside the seal is the input's.
//!
//! [`Receiver`] then does with each event what `quickshare/receive.rs`
//! does -- the handshake loop, the consent wait, the receive loop -- and
//! asserts what that code relies on the library for:
//!
//! - **S5, consent first**: no file chunk and no text before the user
//!   accepted;
//! - **S4/S6 in the library**: an introduction has at most 1000 files,
//!   no negative size, no repeated payload id, and a text of at most
//!   64 KiB; a file's chunks are in order, never past its declared size,
//!   and end exactly there; at most two byte payloads are buffered;
//! - **S1/S2/S4/S6 in the adapter**: the raw offer is the introduction as
//!   sent, and what `Offer::validate` accepts of it keeps every S-rule
//!   ([`crate::assert_offer`]); a received text is S2-clean.
//!
//! No runtime: the socket never waits, so every future completes the first
//! time it is polled or the library is waiting on something that is not
//! there -- which is a finding too.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::OnceLock;
use std::task::{Context, Poll, Waker};

use arbitrary::Arbitrary;
use hmac::{Hmac, Mac};
use prost::Message;
use rqs_lib::hdl::info::Introduction;
use rqs_lib::hdl::payload::{
    MAX_INTRODUCTION_FILES, MAX_PENDING_BYTE_PAYLOADS, MAX_TEXT_PAYLOAD_LENGTH,
};
use rqs_lib::hdl::{InnerState, TransferState};
use rqs_lib::location_nearby_connections::payload_transfer_frame::{
    PacketType, PayloadChunk, PayloadHeader, payload_header::PayloadType,
};
use rqs_lib::location_nearby_connections::{
    self as lnc, DisconnectionFrame, KeepAliveFrame, OfflineFrame, PayloadTransferFrame,
};
use rqs_lib::securegcm::{DeviceToDeviceMessage, GcmMetadata};
use rqs_lib::securemessage::{EncScheme, Header, HeaderAndBody, SecureMessage, SigScheme};
use rqs_lib::sharing_nearby as sn;
use rqs_lib::{InboundEvent, InboundRequest};
use sha2::Sha256;
use sukkula_core::Protocol;
use sukkula_core::offer::Offer;
use sukkula_engine::quickshare::fuzzing;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{assert_clean, assert_offer, oracle};

// ------------------------------------------------------------- the socket

/// Most bytes rqs_lib may write in answer to one step: a keep-alive, a
/// paired-key result, a response and a goodbye are a few hundred bytes
/// each. More would be a reply a sender can amplify.
const MAX_REPLY_BYTES_PER_STEP: usize = 16 * 1024;

#[derive(Default)]
struct Pipe {
    incoming: Vec<u8>,
    read: usize,
    written: usize,
}

/// Both ends of the receiver's socket: what the sender wrote so far, read
/// in order, then end of stream; and a sink that counts what the receiver
/// writes. Neither ever waits.
#[derive(Clone, Default)]
pub struct Wire(Rc<RefCell<Pipe>>);

impl Wire {
    /// The sender writes `bytes`.
    pub fn send(&self, bytes: &[u8]) {
        self.0.borrow_mut().incoming.extend_from_slice(bytes);
    }

    /// The sender writes one frame: length prefix and body.
    pub fn send_frame(&self, body: &[u8]) {
        let len = u32::try_from(body.len()).expect("frames here are small");
        self.send(&len.to_be_bytes());
        self.send(body);
    }

    /// Bytes the receiver has written so far.
    pub fn written(&self) -> usize {
        self.0.borrow().written
    }

    /// Whether everything sent has been read.
    pub fn drained(&self) -> bool {
        let p = self.0.borrow();
        p.read == p.incoming.len()
    }
}

impl AsyncRead for Wire {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut p = self.0.borrow_mut();
        let start = p.read;
        let n = buf.remaining().min(p.incoming.len() - start);
        buf.put_slice(&p.incoming[start..start + n]);
        p.read += n;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Wire {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.borrow_mut().written += buf.len();
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Runs `fut` to completion. Nothing it can wait on exists, so it must
/// finish on the first poll.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("rqs_lib waited on a socket that never blocks"),
    }
}

// ------------------------------------------------------------- sealing

type HmacSha256 = Hmac<Sha256>;

/// The keys of an established D2D channel, for [`Receiver::established`]:
/// ours to encrypt and sign with, the receiver's to answer with.
pub const SENDER_KEY: [u8; 32] = [0x11; 32];
/// The sender's HMAC key.
pub const SENDER_HMAC_KEY: [u8; 32] = [0x22; 32];
const RECEIVER_KEY: [u8; 32] = [0x33; 32];
const RECEIVER_HMAC_KEY: [u8; 32] = [0x44; 32];

/// How a sealed frame is spoiled, for the steps that try the channel's
/// own checks.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tamper {
    /// A sequence number off by this much.
    Sequence(i8),
    /// One signature bit flipped.
    Signature(u8),
    /// One ciphertext bit flipped, the signature redone to match.
    Ciphertext(u16),
    /// Other schemes named in the header.
    Schemes(Enum, Enum),
    /// An IV of another length.
    Iv(u8),
}

/// `offline`, sealed as a sender with `key`/`hmac_key` seals message `seq`.
pub fn seal(
    offline: &[u8],
    seq: i32,
    key: &[u8],
    hmac_key: &[u8],
    tamper: Option<Tamper>,
) -> Vec<u8> {
    let seq = match tamper {
        Some(Tamper::Sequence(d)) => seq.wrapping_add(i32::from(d)),
        _ => seq,
    };
    let d2d = DeviceToDeviceMessage {
        sequence_number: Some(seq),
        message: Some(offline.to_vec()),
    }
    .encode_to_vec();
    let iv = [7u8; 16];
    let mut body =
        rqs_lib::utils::aes_cbc_encrypt(key, &iv, &d2d).expect("32-byte key, 16-byte IV");
    let mut header = Header {
        encryption_scheme: EncScheme::Aes256Cbc.into(),
        signature_scheme: SigScheme::HmacSha256.into(),
        iv: Some(iv.to_vec()),
        public_metadata: Some(
            GcmMetadata {
                r#type: rqs_lib::securegcm::Type::DeviceToDeviceMessage.into(),
                version: Some(1),
            }
            .encode_to_vec(),
        ),
        ..Default::default()
    };
    match tamper {
        Some(Tamper::Ciphertext(at)) if !body.is_empty() => {
            let i = usize::from(at) % body.len();
            body[i] ^= 1 << (at % 8);
        }
        Some(Tamper::Schemes(e, s)) => {
            header.encryption_scheme = e.value();
            header.signature_scheme = s.value();
        }
        Some(Tamper::Iv(n)) => header.iv = Some(vec![7; usize::from(n % 40)]),
        _ => {}
    }
    let header_and_body = HeaderAndBody { header, body }.encode_to_vec();
    let mut mac = HmacSha256::new_from_slice(hmac_key).expect("any key length");
    mac.update(&header_and_body);
    let mut signature = mac.finalize().into_bytes().to_vec();
    if let Some(Tamper::Signature(at)) = tamper {
        let i = usize::from(at) % signature.len();
        signature[i] ^= 1 << (at % 8);
    }
    SecureMessage {
        header_and_body,
        signature,
    }
    .encode_to_vec()
}

fn offline(v1: lnc::V1Frame) -> OfflineFrame {
    OfflineFrame {
        version: Some(lnc::offline_frame::Version::V1.into()),
        v1: Some(v1),
    }
}

fn payload(
    kind: PayloadType,
    id: i64,
    total: i64,
    offset: i64,
    body: &[u8],
    last: bool,
) -> Vec<u8> {
    offline(lnc::V1Frame {
        r#type: Some(lnc::v1_frame::FrameType::PayloadTransfer.into()),
        payload_transfer: Some(PayloadTransferFrame {
            packet_type: Some(PacketType::Data.into()),
            payload_header: Some(PayloadHeader {
                id: Some(id),
                r#type: Some(kind.into()),
                total_size: Some(total),
                is_sensitive: Some(false),
                ..Default::default()
            }),
            payload_chunk: Some(PayloadChunk {
                offset: Some(offset),
                flags: Some(i32::from(last)),
                body: Some(body.to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .encode_to_vec()
}

// ------------------------------------------------------------- the input

/// A protobuf enum field: mostly a small number, where every value the
/// protocol defines lies (and a few past them), sometimes any `i32`. A
/// plain `i32` would hit a defined value one time in a billion, and the
/// branches behind each value -- a text's kind, a frame's type -- would
/// never be reached.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enum {
    Small(u8),
    Any(i32),
}

impl Enum {
    /// The wire value.
    pub fn value(self) -> i32 {
        match self {
            Enum::Small(n) => i32::from(n % 8),
            Enum::Any(n) => n,
        }
    }
}

/// A file in an introduction. Every field is the sender's to choose,
/// missing ones included.
#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub name: Option<String>,
    pub payload_id: Option<i64>,
    pub size: Option<i64>,
    pub mime: Option<String>,
    pub kind: Option<Enum>,
}

/// A text in an introduction.
#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub struct TextMeta {
    pub title: Option<String>,
    pub payload_id: Option<i64>,
    pub size: Option<i64>,
    pub kind: Option<Enum>,
}

/// An introduction, before encoding.
#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub struct Intro {
    pub files: Vec<FileMeta>,
    pub texts: Vec<TextMeta>,
    /// Wi-Fi credentials offered (F-QS5: never taken).
    pub wifi: bool,
    pub required_package: Option<String>,
    /// Pad the file list towards the 1000-file cap, one time in sixteen.
    pub pad: u8,
    pub extra: u16,
}

/// A sharing frame, before encoding.
#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub enum Sharing {
    Introduction(Intro),
    PairedKeyEncryption {
        signed: Vec<u8>,
        hash: Vec<u8>,
    },
    PairedKeyResult(Enum),
    Response(Enum),
    Cancel,
    CertificateInfo,
    /// A frame type with nothing in it.
    Bare(Enum),
    /// Encoded bytes as they are.
    Bytes(Vec<u8>),
}

/// Which payload id a step names: one the introduction offered, or any.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    Offered(u8),
    Text,
    Any(i64),
}

/// How a byte payload is cut into chunks.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunks {
    Whole,
    /// Into this many pieces (1..=8), offsets right.
    Split(u8),
    /// Declaring this total, whatever the body is.
    Lying(i64),
    /// With the last chunk's flag missing.
    Unfinished,
}

/// One thing the sender does, or the user.
#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A sharing frame in a byte payload, as senders send control frames.
    Sharing {
        id: i64,
        frame: Sharing,
        chunks: Chunks,
    },
    /// A chunk of a file.
    File {
        id: Id,
        offset: Option<i64>,
        total: Option<i64>,
        body: Vec<u8>,
        last: bool,
    },
    /// The text's bytes, in a byte payload.
    Text {
        id: Id,
        body: Vec<u8>,
        chunks: Chunks,
    },
    KeepAlive(bool),
    Disconnection,
    /// A decrypted offline frame, encoded bytes as they are.
    Offline(Vec<u8>),
    /// A sealed frame spoiled one way.
    Tampered {
        offline: Vec<u8>,
        how: Tamper,
    },
    /// A frame that is not sealed at all.
    Unsealed(Vec<u8>),
    /// Bytes on the socket as they are: length prefixes are the input's.
    Raw(Vec<u8>),
    /// The user says yes.
    Accept,
    /// The user says no.
    Decline,
}

fn encode_sharing(frame: &Sharing) -> Vec<u8> {
    use sn::v1_frame::FrameType as T;
    let mut v1 = sn::V1Frame::default();
    match frame {
        Sharing::Introduction(intro) => {
            v1.r#type = Some(T::Introduction.into());
            let mut files: Vec<sn::FileMetadata> = intro
                .files
                .iter()
                .map(|f| sn::FileMetadata {
                    name: f.name.clone(),
                    r#type: f.kind.map(Enum::value),
                    payload_id: f.payload_id,
                    size: f.size,
                    mime_type: f.mime.clone(),
                    ..Default::default()
                })
                .collect();
            if intro.pad & 0x0F == 0x01 {
                let n = usize::from(intro.extra) % (MAX_INTRODUCTION_FILES + 10);
                files.extend((0..n).map(|i| sn::FileMetadata {
                    name: Some(format!("{i}.bin")),
                    payload_id: Some(1_000_000 + i64::try_from(i).unwrap_or(0)),
                    size: Some(1),
                    ..Default::default()
                }));
            }
            v1.introduction = Some(sn::IntroductionFrame {
                file_metadata: files,
                text_metadata: intro
                    .texts
                    .iter()
                    .map(|t| sn::TextMetadata {
                        text_title: t.title.clone(),
                        r#type: t.kind.map(Enum::value),
                        payload_id: t.payload_id,
                        size: t.size,
                        ..Default::default()
                    })
                    .collect(),
                required_package: intro.required_package.clone(),
                wifi_credentials_metadata: if intro.wifi {
                    vec![sn::WifiCredentialsMetadata {
                        ssid: Some("net".into()),
                        payload_id: Some(99),
                        ..Default::default()
                    }]
                } else {
                    Vec::new()
                },
            });
        }
        Sharing::PairedKeyEncryption { signed, hash } => {
            v1.r#type = Some(T::PairedKeyEncryption.into());
            v1.paired_key_encryption = Some(sn::PairedKeyEncryptionFrame {
                signed_data: Some(signed.clone()),
                secret_id_hash: Some(hash.clone()),
                ..Default::default()
            });
        }
        Sharing::PairedKeyResult(status) => {
            v1.r#type = Some(T::PairedKeyResult.into());
            v1.paired_key_result = Some(sn::PairedKeyResultFrame {
                status: Some(status.value()),
            });
        }
        Sharing::Response(status) => {
            v1.r#type = Some(T::Response.into());
            v1.connection_response = Some(sn::ConnectionResponseFrame {
                status: Some(status.value()),
            });
        }
        Sharing::Cancel => v1.r#type = Some(T::Cancel.into()),
        Sharing::CertificateInfo => {
            v1.r#type = Some(T::CertificateInfo.into());
            v1.certificate_info = Some(sn::CertificateInfoFrame::default());
        }
        Sharing::Bare(t) => v1.r#type = Some(t.value()),
        Sharing::Bytes(b) => return b.clone(),
    }
    sn::Frame {
        version: Some(sn::frame::Version::V1.into()),
        v1: Some(v1),
    }
    .encode_to_vec()
}

/// A byte payload's chunks as offline frames.
fn byte_chunks(id: i64, body: &[u8], chunks: Chunks) -> Vec<Vec<u8>> {
    let len = i64::try_from(body.len()).unwrap_or(i64::MAX);
    match chunks {
        Chunks::Whole => vec![payload(PayloadType::Bytes, id, len, 0, body, true)],
        Chunks::Lying(total) => vec![payload(PayloadType::Bytes, id, total, 0, body, true)],
        Chunks::Unfinished => vec![payload(PayloadType::Bytes, id, len, 0, body, false)],
        Chunks::Split(n) => {
            let n = usize::from(n % 8) + 1;
            let size = body.len().div_ceil(n).max(1);
            let mut out = Vec::new();
            let mut offset = 0usize;
            for piece in body.chunks(size) {
                offset += piece.len();
                let at = i64::try_from(offset - piece.len()).unwrap_or(0);
                out.push(payload(PayloadType::Bytes, id, len, at, piece, false));
            }
            // The end, as senders mark it: an empty last chunk.
            out.push(payload(PayloadType::Bytes, id, len, len, &[], true));
            out
        }
    }
}

// ------------------------------------------------------------- the receiver

/// Where the adapter is with the connection.
#[derive(Debug)]
enum Phase {
    /// `receive::handshake`: waiting for the introduction.
    Setup,
    /// `receive::consent`: the user is being asked about an offer that
    /// validated.
    Consent { intro: Introduction },
    /// `receive::receive`: accepted; what is still to come.
    Receiving {
        /// Payload id to (declared size, bytes so far).
        files: HashMap<i64, (i64, i64)>,
        text: Option<i64>,
    },
    /// The adapter has let the connection go.
    Over,
}

/// The receiving side of one connection, driven as the adapter drives it.
pub struct Receiver {
    pub ir: InboundRequest<Wire>,
    pub wire: Wire,
    key: Vec<u8>,
    hmac_key: Vec<u8>,
    /// The sequence number of the sender's next sealed frame.
    seq: i32,
    phase: Phase,
    /// The milestones reached, in order.
    pub reached: Vec<&'static str>,
}

impl Receiver {
    /// A receiver with nothing read yet.
    pub fn fresh() -> Receiver {
        let wire = Wire::default();
        Receiver {
            ir: InboundRequest::new(wire.clone()),
            wire,
            key: Vec::new(),
            hmac_key: Vec::new(),
            seq: 1,
            phase: Phase::Setup,
            reached: Vec::new(),
        }
    }

    /// A receiver past the key exchange, in `state`, under the fixed keys,
    /// told `name` by the connection request and showing `pin`.
    pub fn established(state: TransferState, name: String, pin: Option<String>) -> Receiver {
        let mut r = Receiver::fresh();
        let s: &mut InnerState = &mut r.ir.state;
        s.state = state;
        s.encryption_done = true;
        s.decrypt_key = Some(SENDER_KEY.to_vec());
        s.recv_hmac_key = Some(SENDER_HMAC_KEY.to_vec());
        s.encrypt_key = Some(RECEIVER_KEY.to_vec());
        s.send_hmac_key = Some(RECEIVER_HMAC_KEY.to_vec());
        s.remote_device_info = Some(rqs_lib::utils::RemoteDeviceInfo {
            name,
            device_type: rqs_lib::DeviceType::Phone,
        });
        s.pin_code = pin;
        r.use_keys(SENDER_KEY.to_vec(), SENDER_HMAC_KEY.to_vec());
        r
    }

    /// Seal what follows with these keys: the ones the receiver decrypts
    /// with.
    pub fn use_keys(&mut self, key: Vec<u8>, hmac_key: Vec<u8>) {
        self.key = key;
        self.hmac_key = hmac_key;
    }

    /// Notes a milestone the input reached (see [`record`]).
    pub fn note(&mut self, what: &'static str) {
        record(&mut self.reached, what);
    }

    /// Whether the adapter still holds the connection.
    pub fn open(&self) -> bool {
        !matches!(self.phase, Phase::Over)
    }

    /// Reads and processes the next frame as the adapter does; `false`
    /// when the adapter would have let the connection go.
    pub fn read_next(&mut self) -> bool {
        let before = self.wire.written();
        let event = block_on(self.ir.next_event());
        let wrote = self.wire.written() - before;
        assert!(
            wrote <= MAX_REPLY_BYTES_PER_STEP,
            "{wrote} bytes written in answer to one frame"
        );
        assert!(
            self.ir.state.payload_buffers.len() <= MAX_PENDING_BYTE_PAYLOADS,
            "{} byte payloads buffered",
            self.ir.state.payload_buffers.len()
        );
        match event {
            Err(_) => {
                self.phase = Phase::Over;
                false
            }
            Ok(None) => true,
            Ok(Some(ev)) => self.event(ev),
        }
    }

    /// One sealed offline frame, then the receiver's turn.
    fn sealed(&mut self, offline: &[u8], tamper: Option<Tamper>) -> bool {
        let frame = seal(offline, self.seq, &self.key, &self.hmac_key, tamper);
        self.wire.send_frame(&frame);
        let ok = self.read_next();
        if ok {
            // Accepted, so the receiver counted it.
            self.seq = self.seq.wrapping_add(1);
        }
        ok
    }

    fn offered_id(&self, id: Id) -> i64 {
        let files: Vec<i64> = match &self.phase {
            Phase::Consent { intro, .. } => intro.files.iter().map(|f| f.payload_id).collect(),
            Phase::Receiving { files, .. } => {
                let mut ids: Vec<i64> = files.keys().copied().collect();
                ids.sort_unstable();
                ids
            }
            _ => Vec::new(),
        };
        let text = match &self.phase {
            Phase::Consent { intro, .. } => intro.text.as_ref().map(|t| t.payload_id),
            Phase::Receiving { text, .. } => *text,
            _ => None,
        };
        match id {
            Id::Offered(i) if !files.is_empty() => files[usize::from(i) % files.len()],
            Id::Offered(i) => i64::from(i),
            Id::Text => text.unwrap_or(0),
            Id::Any(n) => n,
        }
    }

    fn file_offset(&self, id: i64) -> i64 {
        match &self.phase {
            Phase::Receiving { files, .. } => files.get(&id).map_or(0, |f| f.1),
            _ => 0,
        }
    }

    /// Runs one step; `false` once the adapter has let the connection go.
    pub fn step(&mut self, step: &Step) -> bool {
        if !self.open() {
            return false;
        }
        match step {
            Step::Sharing { id, frame, chunks } => {
                let body = encode_sharing(frame);
                for chunk in byte_chunks(*id, &body, *chunks) {
                    if !self.sealed(&chunk, None) {
                        return false;
                    }
                }
                true
            }
            Step::File {
                id,
                offset,
                total,
                body,
                last,
            } => {
                let id = self.offered_id(*id);
                let offset = offset.unwrap_or_else(|| self.file_offset(id));
                let total = total.unwrap_or(i64::MAX);
                let frame = payload(PayloadType::File, id, total, offset, body, *last);
                self.sealed(&frame, None)
            }
            Step::Text { id, body, chunks } => {
                let id = self.offered_id(*id);
                for chunk in byte_chunks(id, body, *chunks) {
                    if !self.sealed(&chunk, None) {
                        return false;
                    }
                }
                true
            }
            Step::KeepAlive(ack) => {
                let frame = offline(lnc::V1Frame {
                    r#type: Some(lnc::v1_frame::FrameType::KeepAlive.into()),
                    keep_alive: Some(KeepAliveFrame { ack: Some(*ack) }),
                    ..Default::default()
                });
                self.sealed(&frame.encode_to_vec(), None)
            }
            Step::Disconnection => {
                let frame = offline(lnc::V1Frame {
                    r#type: Some(lnc::v1_frame::FrameType::Disconnection.into()),
                    disconnection: Some(DisconnectionFrame::default()),
                    ..Default::default()
                });
                self.sealed(&frame.encode_to_vec(), None)
            }
            Step::Offline(bytes) => self.sealed(bytes, None),
            Step::Tampered { offline, how } => self.sealed(offline, Some(*how)),
            Step::Unsealed(bytes) => {
                self.wire.send_frame(bytes);
                self.read_next()
            }
            Step::Raw(bytes) => {
                self.wire.send(bytes);
                // As many frames as the bytes hold; a partial one is the
                // end of the stream, and the connection's.
                while self.open() && !self.wire.drained() {
                    self.read_next();
                }
                self.open()
            }
            Step::Accept => self.answer(true),
            Step::Decline => self.answer(false),
        }
    }

    /// The user's answer, as `receive::connection` acts on it. Nothing is
    /// asked outside the consent wait, and a stray answer changes nothing.
    fn answer(&mut self, yes: bool) -> bool {
        if !matches!(self.phase, Phase::Consent { .. }) {
            return self.open();
        }
        let Phase::Consent { intro, .. } = std::mem::replace(&mut self.phase, Phase::Over) else {
            return false;
        };
        if yes {
            block_on(self.ir.accept_transfer()).expect("accepting the introduction it gave");
            record(&mut self.reached, "accepted");
            self.phase = Phase::Receiving {
                files: intro
                    .files
                    .iter()
                    .map(|f| (f.payload_id, (f.size, 0)))
                    .collect(),
                text: intro.text.as_ref().map(|t| t.payload_id),
            };
            true
        } else {
            self.goodbye();
            false
        }
    }

    /// A refusal and a disconnection, as the adapter declines.
    fn goodbye(&mut self) {
        let _ = block_on(self.ir.reject_transfer(None));
        let _ = block_on(self.ir.disconnection());
        self.phase = Phase::Over;
    }

    /// The sender's name as the consent dialog would show it, whether or
    /// not an introduction ever comes: through the adapter's raw offer and
    /// `Offer::validate`, with a file of the adapter's own.
    pub fn check_sender(&self) {
        let intro = Introduction {
            files: vec![rqs_lib::hdl::info::IncomingFile {
                payload_id: 1,
                name: "a".into(),
                size: 1,
                mime_type: "application/octet-stream".into(),
            }],
            text: None,
        };
        let raw = fuzzing::raw_offer(&self.ir, &intro);
        let offer = Offer::validate(raw).expect("one small file under any name validates");
        assert_offer(&offer);
    }

    /// What the adapter does with an event, and what it relies on.
    fn event(&mut self, ev: InboundEvent) -> bool {
        // Only the encrypted channel carries events; one before the key
        // exchange would be a way around it.
        assert!(
            self.ir.state.encryption_done,
            "an event before the key exchange: {ev:?}"
        );
        match &mut self.phase {
            Phase::Setup => match ev {
                InboundEvent::Introduction(intro) => {
                    check_introduction(&intro);
                    assert_eq!(self.ir.state.state, TransferState::WaitingForUserConsent);
                    if check_raw_offer(&self.ir, &intro).is_some() {
                        record(&mut self.reached, "introduction");
                        self.phase = Phase::Consent { intro };
                        true
                    } else {
                        // `Ctx::offer` refuses it before anyone is asked.
                        record(&mut self.reached, "introduction refused");
                        self.goodbye();
                        false
                    }
                }
                // `receive::handshake` gives up on anything else.
                other => {
                    before_consent(&other);
                    self.phase = Phase::Over;
                    false
                }
            },
            Phase::Consent { .. } => {
                // `receive::consent`: any event ends the wait.
                before_consent(&ev);
                self.phase = Phase::Over;
                false
            }
            Phase::Receiving { files, text } => {
                match ev {
                    InboundEvent::FileChunk(c) => {
                        let Some((size, got)) = files.get_mut(&c.payload_id) else {
                            panic!(
                                "a chunk of payload {} nobody offered or already complete",
                                c.payload_id
                            );
                        };
                        assert_eq!(c.offset, *got, "a chunk out of order");
                        let len = i64::try_from(c.body.len()).expect("small");
                        let after = got.checked_add(len).expect("offsets overflowed");
                        assert!(after <= *size, "a file past its declared size");
                        if c.last {
                            assert_eq!(after, *size, "a file ended short");
                            record(&mut self.reached, "file complete");
                            files.remove(&c.payload_id);
                        } else {
                            *got = after;
                        }
                    }
                    InboundEvent::Text {
                        payload_id,
                        text: t,
                    } => {
                        assert_eq!(Some(payload_id), *text, "a text nobody offered");
                        assert!(
                            t.len() <= usize::try_from(MAX_TEXT_PAYLOAD_LENGTH).expect("small"),
                            "a text over the cap"
                        );
                        let shown = fuzzing::text_received(&t);
                        assert_clean("received text", &t, oracle::message_violation(&shown));
                        record(&mut self.reached, "text");
                        *text = None;
                    }
                    InboundEvent::Introduction(_) => {
                        panic!("a second introduction after the answer")
                    }
                    InboundEvent::Cancelled | InboundEvent::Disconnected => {
                        self.phase = Phase::Over;
                        return false;
                    }
                }
                if files.is_empty() && text.is_none() {
                    // Everything arrived: the adapter says goodbye.
                    record(&mut self.reached, "all received");
                    self.phase = Phase::Over;
                    return false;
                }
                true
            }
            Phase::Over => false,
        }
    }
}

/// Notes that an input reached `what`: in `reached`, which the seed tests
/// read, and on stderr when `SUKKULA_FUZZ_TRACE` is set, for checking what
/// a corpus reaches (`fuzz/README.md`, "Seeds").
fn record(reached: &mut Vec<&'static str>, what: &'static str) {
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("SUKKULA_FUZZ_TRACE").is_some()) {
        eprintln!("MILESTONE {what}");
    }
    reached.push(what);
}

/// Steps a `quickshare_frame` input runs at most: more frames only repeat
/// what these reach.
pub const MAX_FRAME_STEPS: usize = 64;

/// A `quickshare_frame` input.
#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub struct FrameInput {
    /// How far the setup got: the paired-key encryption frame is next, the
    /// paired-key result is, or the introduction is.
    pub start: u8,
    /// The name the connection request gave.
    pub name: String,
    /// The PIN the key exchange gave, as rqs_lib derives it: four digits.
    pub pin: Option<u16>,
    pub steps: Vec<Step>,
}

/// Runs a `quickshare_frame` input; the milestones it reached.
pub fn run_frames(input: &FrameInput) -> Vec<&'static str> {
    let state = match input.start % 3 {
        0 => TransferState::SentConnectionResponse,
        1 => TransferState::SentPairedKeyResult,
        _ => TransferState::ReceivedPairedKeyResult,
    };
    let pin = input.pin.map(|p| format!("{:04}", p % 10_000));
    let mut r = Receiver::established(state, input.name.clone(), pin);
    for step in input.steps.iter().take(MAX_FRAME_STEPS) {
        if !r.step(step) {
            break;
        }
    }
    r.reached
}

/// S5: nothing of the payload before the user said yes.
fn before_consent(ev: &InboundEvent) {
    assert!(
        !matches!(ev, InboundEvent::FileChunk(_) | InboundEvent::Text { .. }),
        "payload before consent: {ev:?}"
    );
}

/// What rqs_lib promises of an introduction it hands over.
fn check_introduction(intro: &Introduction) {
    assert!(
        intro.files.is_empty() != intro.text.is_none(),
        "files and a text at once, or neither"
    );
    assert!(
        intro.files.len() <= MAX_INTRODUCTION_FILES,
        "too many files"
    );
    let mut ids = HashSet::new();
    for f in &intro.files {
        assert!(f.size >= 0, "a negative size reached the application");
        assert!(
            ids.insert(f.payload_id),
            "payload id {} twice",
            f.payload_id
        );
    }
    if let Some(t) = &intro.text {
        assert!(
            (0..=MAX_TEXT_PAYLOAD_LENGTH).contains(&t.size),
            "text size {}",
            t.size
        );
    }
}

/// The adapter's raw offer is the introduction as sent, and what validates
/// of it keeps every S-rule.
fn check_raw_offer(ir: &InboundRequest<Wire>, intro: &Introduction) -> Option<Offer> {
    let raw = fuzzing::raw_offer(ir, intro);
    assert_eq!(raw.protocol, Protocol::QuickShare);
    assert_eq!(raw.pin.as_deref(), ir.pin_code());
    assert_eq!(
        raw.sender,
        ir.remote_device_info()
            .map(|r| r.name.clone())
            .unwrap_or_default()
    );
    assert_eq!(raw.files.len(), intro.files.len());
    for (r, f) in raw.files.iter().zip(&intro.files) {
        assert_eq!(r.name, f.name);
        assert_eq!(r.size, i128::from(f.size));
        assert_eq!(r.mime.as_deref(), Some(f.mime_type.as_str()));
    }
    if let Some(preview) = &raw.text {
        assert!(
            preview.len() <= 1024,
            "a preview of {} bytes",
            preview.len()
        );
    }
    assert_eq!(raw.text.is_some(), intro.text.is_some());
    match Offer::validate(raw) {
        Ok(o) => {
            assert_offer(&o);
            assert_eq!(o.files.len(), intro.files.len());
            for (o, f) in o.files.iter().zip(&intro.files) {
                assert_eq!(i128::from(o.size), i128::from(f.size));
            }
            Some(o)
        }
        Err(_) => None,
    }
}

/// The harness itself, on the paths a real sender takes: if these break,
/// the targets fuzz a sender that no longer follows the protocol, and stop
/// reaching what they exist for.
#[cfg(test)]
mod tests {
    use super::*;

    fn intro(files: Vec<FileMeta>, texts: Vec<TextMeta>) -> Step {
        Step::Sharing {
            id: 7,
            frame: Sharing::Introduction(Intro {
                files,
                texts,
                wifi: false,
                required_package: None,
                pad: 0,
                extra: 0,
            }),
            chunks: Chunks::Split(3),
        }
    }

    fn file(id: i64, name: &str, size: i64) -> FileMeta {
        FileMeta {
            name: Some(name.into()),
            payload_id: Some(id),
            size: Some(size),
            mime: Some("application/octet-stream".into()),
            kind: None,
        }
    }

    fn receiver() -> Receiver {
        Receiver::established(
            TransferState::ReceivedPairedKeyResult,
            "Pixel".into(),
            Some("1234".into()),
        )
    }

    #[test]
    fn files_go_through_after_consent() {
        let mut r = receiver();
        assert!(r.step(&intro(
            vec![file(5, "a.bin", 3), file(9, "b.bin", 0)],
            vec![]
        )));
        assert!(matches!(r.phase, Phase::Consent { .. }));
        assert!(r.step(&Step::KeepAlive(false)));
        assert!(r.step(&Step::Accept));
        let chunk = |id, body: &[u8], last| Step::File {
            id: Id::Any(id),
            offset: None,
            total: None,
            body: body.to_vec(),
            last,
        };
        assert!(r.step(&chunk(5, b"ab", false)));
        assert!(r.step(&chunk(5, b"c", true)));
        // The last file done, the adapter says goodbye.
        assert!(!r.step(&chunk(9, b"", true)));
        assert!(!r.open());
    }

    #[test]
    fn a_text_goes_through_after_consent() {
        let mut r = receiver();
        let text = TextMeta {
            title: Some("hello".into()),
            payload_id: Some(3),
            size: Some(5),
            kind: Some(Enum::Small(1)),
        };
        assert!(r.step(&intro(vec![], vec![text])));
        assert!(r.step(&Step::Accept));
        assert!(!r.step(&Step::Text {
            id: Id::Text,
            body: b"hello".to_vec(),
            chunks: Chunks::Split(2),
        }));
        assert!(!r.open());
    }

    #[test]
    fn payload_before_consent_ends_the_connection() {
        let mut r = receiver();
        assert!(r.step(&intro(vec![file(5, "a.bin", 3)], vec![])));
        assert!(!r.step(&Step::File {
            id: Id::Offered(0),
            offset: None,
            total: None,
            body: b"abc".to_vec(),
            last: true,
        }));
    }

    #[test]
    fn the_setup_frames_lead_to_the_introduction() {
        let mut r =
            Receiver::established(TransferState::SentConnectionResponse, "Pixel".into(), None);
        let sharing = |frame| Step::Sharing {
            id: 1,
            frame,
            chunks: Chunks::Whole,
        };
        assert!(r.step(&sharing(Sharing::PairedKeyEncryption {
            signed: vec![1; 72],
            hash: vec![2; 6],
        })));
        assert!(r.step(&sharing(Sharing::PairedKeyResult(Enum::Small(2)))));
        assert!(r.step(&intro(vec![file(5, "../../a.bin", 3)], vec![])));
        assert!(matches!(r.phase, Phase::Consent { .. }));
        assert!(!r.step(&Step::Decline));
    }

    #[test]
    fn spoiled_seals_are_refused() {
        for how in [
            Tamper::Sequence(1),
            Tamper::Signature(3),
            Tamper::Ciphertext(9),
            Tamper::Schemes(Enum::Small(0), Enum::Small(0)),
            Tamper::Iv(8),
        ] {
            let mut r = receiver();
            let frame = offline(lnc::V1Frame {
                r#type: Some(lnc::v1_frame::FrameType::KeepAlive.into()),
                keep_alive: Some(KeepAliveFrame { ack: Some(false) }),
                ..Default::default()
            });
            assert!(
                !r.step(&Step::Tampered {
                    offline: frame.encode_to_vec(),
                    how,
                }),
                "{how:?}"
            );
        }
    }
}
