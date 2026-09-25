//! The sender `quickshare_handshake` plays. Quick Share from the first
//! byte: the plaintext handshake a stranger on the
//! LAN reaches before anything is authenticated -- the frame reader's
//! limits, the connection request and the endpoint info inside it (the
//! sender's name), UKEY2's client init and finish, the peer's P-256 key --
//! and then, when the input completes the exchange, the encrypted channel
//! under the keys it derived (spec §7 "Quick Share frames").
//!
//! Two kinds of input. `Stream` is bytes on the socket as they are, length
//! prefixes included. `Scripted` is a sender that follows the protocol
//! wherever the input does not say otherwise: it commits to the client
//! finish it will really send, so the commitment check passes and the
//! peer's key -- a real one with its coordinates encoded every way Java's
//! `BigInteger` or a careless sender could, or any bytes -- reaches
//! `decode_p256_point` and the key derivation.
//!
//! Asserted: no event of any kind before the key exchange; the name the
//! connection request carried is the one handed over, and the consent
//! dialog shows it S2-clean; a finished exchange yields a four-digit PIN;
//! and after it everything `quickshare_frame` asserts.

use arbitrary::Arbitrary;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use prost::Message;
use rqs_lib::hdl::TransferState;
use rqs_lib::location_nearby_connections::{self as lnc, OfflineFrame};
use rqs_lib::securegcm::ukey2_client_init::CipherCommitment;
use rqs_lib::securegcm::{
    Ukey2ClientFinished, Ukey2ClientInit, Ukey2HandshakeCipher, Ukey2Message, ukey2_message,
};
use rqs_lib::securemessage::{EcP256PublicKey, GenericPublicKey, PublicKeyType};
use sha2::{Digest, Sha256, Sha512};

use crate::quickshare::{Enum, Receiver, Step};

/// Steps run at most after the handshake.
pub const MAX_STEPS: usize = 32;

const NEXT_PROTOCOL: &str = "AES_256_CBC-HMAC_SHA256";

#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Stream(Vec<u8>),
    Scripted(Box<Script>),
}

#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub request: Request,
    pub init: Init,
    pub finish: Finish,
    /// The connection response, or `None` for a proper one.
    pub response: Option<Vec<u8>>,
    pub steps: Vec<Step>,
}

#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Raw(Vec<u8>),
    Built {
        /// Version, visibility, device type.
        flags: u8,
        identity: [u8; 16],
        name: Vec<u8>,
        /// The name's length byte, when it lies.
        length: Option<u8>,
        /// Cut the endpoint info to this many bytes.
        cut: Option<u8>,
        frame_type: Option<Enum>,
    },
}

#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub enum Init {
    Raw(Vec<u8>),
    Built {
        message_type: Option<Enum>,
        version: Option<Enum>,
        random: Option<Vec<u8>>,
        /// Other ciphers offered, before P-256.
        others: Vec<Enum>,
        /// Offer P-256 at all.
        p256: bool,
        /// Commit to something else than the finish that follows.
        wrong_commitment: bool,
        next_protocol: Option<String>,
    },
}

#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    Raw(Vec<u8>),
    Built {
        message_type: Option<Enum>,
        key: Key,
    },
}

#[derive(Arbitrary, Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A real key, from a seed, its coordinates spelt as `pad` says.
    Valid { seed: u64, x: Pad, y: Pad },
    /// Any coordinates.
    Coordinates {
        x: Vec<u8>,
        y: Vec<u8>,
        key_type: Option<Enum>,
    },
    /// Bytes that are meant to be a `GenericPublicKey`.
    Bytes(Vec<u8>),
    /// No key at all.
    Missing,
}

/// How a coordinate is written.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pad {
    /// As Java's `BigInteger.toByteArray()`: a sign byte when the top bit
    /// is set, leading zeros dropped (rqs_lib's own `encode_point`).
    Java,
    /// Exactly 32 bytes.
    Fixed,
    /// With this many more leading zeros (at most 8).
    Zeros(u8),
    /// With a leading byte that is not a zero.
    Junk(u8),
}

fn coordinate(v: &[u8], pad: Pad) -> Vec<u8> {
    match pad {
        Pad::Java => {
            // A positive two's-complement integer, shortest form.
            let start = v.iter().position(|b| *b != 0).unwrap_or(v.len());
            let mut out = Vec::with_capacity(33);
            if v.get(start).is_none_or(|b| b & 0x80 != 0) {
                out.push(0);
            }
            out.extend_from_slice(&v[start..]);
            out
        }
        Pad::Fixed => v.to_vec(),
        Pad::Zeros(n) => {
            let mut out = vec![0; usize::from(n % 9)];
            out.extend_from_slice(v);
            out
        }
        Pad::Junk(b) => {
            let mut out = vec![b | 1];
            out.extend_from_slice(v);
            out
        }
    }
}

fn request(r: &Request) -> (Vec<u8>, Option<String>) {
    let (flags, identity, name, length, cut, frame_type) = match r {
        Request::Raw(b) => return (b.clone(), None),
        Request::Built {
            flags,
            identity,
            name,
            length,
            cut,
            frame_type,
        } => (flags, identity, name, length, cut, frame_type),
    };
    let name = &name[..name.len().min(255)];
    let mut info = vec![*flags];
    info.extend_from_slice(identity);
    info.push(length.unwrap_or(u8::try_from(name.len()).unwrap_or(u8::MAX)));
    info.extend_from_slice(name);
    let honest = length.is_none() && cut.is_none_or(|c| usize::from(c) >= info.len());
    if let Some(c) = cut {
        info.truncate(usize::from(*c));
    }
    let frame = OfflineFrame {
        version: Some(lnc::offline_frame::Version::V1.into()),
        v1: Some(lnc::V1Frame {
            r#type: Some(frame_type.map_or(
                lnc::v1_frame::FrameType::ConnectionRequest.into(),
                Enum::value,
            )),
            connection_request: Some(lnc::ConnectionRequestFrame {
                endpoint_id: Some("ABCD".into()),
                endpoint_info: Some(info),
                ..Default::default()
            }),
            ..Default::default()
        }),
    };
    // What the receiver must have understood, when the frame is honest.
    let expected = (honest && frame_type.is_none())
        .then(|| std::str::from_utf8(name).ok().map(str::to_owned))
        .flatten();
    (frame.encode_to_vec(), expected)
}

fn finish(f: &Finish) -> Vec<u8> {
    let (message_type, key) = match f {
        Finish::Raw(b) => return b.clone(),
        Finish::Built { message_type, key } => (message_type, key),
    };
    let public_key = match key {
        Key::Valid { seed, x, y } => {
            let scalar = Sha256::digest(seed.to_le_bytes());
            let secret = p256::SecretKey::from_slice(&scalar).expect("a digest is a valid scalar");
            let point = secret.public_key().to_encoded_point(false);
            let (Some(px), Some(py)) = (point.x(), point.y()) else {
                panic!("an uncompressed point without coordinates")
            };
            Some(
                GenericPublicKey {
                    r#type: PublicKeyType::EcP256.into(),
                    ec_p256_public_key: Some(EcP256PublicKey {
                        x: coordinate(px, *x),
                        y: coordinate(py, *y),
                    }),
                    ..Default::default()
                }
                .encode_to_vec(),
            )
        }
        Key::Coordinates { x, y, key_type } => Some(
            GenericPublicKey {
                r#type: key_type.map_or(PublicKeyType::EcP256.into(), Enum::value),
                ec_p256_public_key: Some(EcP256PublicKey {
                    x: x.clone(),
                    y: y.clone(),
                }),
                ..Default::default()
            }
            .encode_to_vec(),
        ),
        Key::Bytes(b) => Some(b.clone()),
        Key::Missing => None,
    };
    Ukey2Message {
        message_type: Some(
            message_type.map_or(ukey2_message::Type::ClientFinish.into(), Enum::value),
        ),
        message_data: Some(Ukey2ClientFinished { public_key }.encode_to_vec()),
    }
    .encode_to_vec()
}

fn init(i: &Init, finish: &[u8]) -> Vec<u8> {
    let (message_type, version, random, others, p256, wrong_commitment, next_protocol) = match i {
        Init::Raw(b) => return b.clone(),
        Init::Built {
            message_type,
            version,
            random,
            others,
            p256,
            wrong_commitment,
            next_protocol,
        } => (
            message_type,
            version,
            random,
            others,
            p256,
            wrong_commitment,
            next_protocol,
        ),
    };
    let mut commitment = Sha512::digest(finish).to_vec();
    if *wrong_commitment {
        commitment[0] ^= 1;
    }
    let mut ciphers: Vec<CipherCommitment> = others
        .iter()
        .take(8)
        .map(|c| CipherCommitment {
            handshake_cipher: Some(c.value()),
            commitment: Some(vec![0; 64]),
        })
        .collect();
    if *p256 {
        ciphers.push(CipherCommitment {
            handshake_cipher: Some(Ukey2HandshakeCipher::P256Sha512.into()),
            commitment: Some(commitment),
        });
    }
    let client_init = Ukey2ClientInit {
        version: Some(version.map_or(1, Enum::value)),
        random: Some(random.clone().unwrap_or_else(|| vec![0x5A; 32])),
        cipher_commitments: ciphers,
        next_protocol: Some(
            next_protocol
                .clone()
                .unwrap_or_else(|| NEXT_PROTOCOL.to_owned()),
        ),
    };
    Ukey2Message {
        message_type: Some(
            message_type.map_or(ukey2_message::Type::ClientInit.into(), Enum::value),
        ),
        message_data: Some(client_init.encode_to_vec()),
    }
    .encode_to_vec()
}

fn response(r: Option<&Vec<u8>>) -> Vec<u8> {
    if let Some(b) = r {
        return b.clone();
    }
    OfflineFrame {
        version: Some(lnc::offline_frame::Version::V1.into()),
        v1: Some(lnc::V1Frame {
            r#type: Some(lnc::v1_frame::FrameType::ConnectionResponse.into()),
            connection_response: Some(lnc::ConnectionResponseFrame {
                response: Some(lnc::connection_response_frame::ResponseStatus::Accept.into()),
                ..Default::default()
            }),
            ..Default::default()
        }),
    }
    .encode_to_vec()
}

/// One plaintext frame; `false` once the connection is over.
fn plaintext(r: &mut Receiver, frame: &[u8]) -> bool {
    r.wire.send_frame(frame);
    r.read_next()
}

fn scripted(r: &mut Receiver, s: &Script) {
    let (req, expected_name) = request(&s.request);
    let ok = plaintext(r, &req);
    if let Some(name) = expected_name {
        assert!(ok, "an honest connection request was refused");
        assert_eq!(
            r.ir.remote_device_info().map(|d| d.name.as_str()),
            Some(name.as_str()),
            "the name was misread"
        );
    }
    if r.ir.remote_device_info().is_some() {
        r.check_sender();
    }
    if !ok {
        return;
    }
    r.note("connection request");
    let fin = finish(&s.finish);
    if !plaintext(r, &init(&s.init, &fin)) {
        return;
    }
    r.note("client init");
    if !plaintext(r, &fin) {
        return;
    }
    // The finish was taken, so the keys are there.
    assert!(
        r.ir.state.encryption_done,
        "a finish taken without a key exchange"
    );
    let pin = r.ir.pin_code().expect("a finished exchange has a PIN");
    assert!(
        pin.len() == 4 && pin.bytes().all(|b| b.is_ascii_digit()),
        "PIN {pin:?}"
    );
    r.note("key exchange");
    if !plaintext(r, &response(s.response.as_ref())) {
        return;
    }
    assert_eq!(r.ir.state.state, TransferState::SentConnectionResponse);
    r.note("channel open");
    let (Some(key), Some(mac)) = (
        r.ir.state.decrypt_key.clone(),
        r.ir.state.recv_hmac_key.clone(),
    ) else {
        panic!("past the handshake without keys");
    };
    r.use_keys(key, mac);
    for step in s.steps.iter().take(MAX_STEPS) {
        if !r.step(step) {
            break;
        }
    }
}

/// Runs one input; the milestones it reached.
pub fn run(input: &Input) -> Vec<&'static str> {
    let mut r = Receiver::fresh();
    match input {
        Input::Stream(bytes) => {
            r.wire.send(bytes);
            while r.open() && !r.wire.drained() {
                r.read_next();
            }
            if r.ir.remote_device_info().is_some() {
                r.check_sender();
            }
        }
        Input::Scripted(s) => scripted(&mut r, s),
    }
    r.reached
}
