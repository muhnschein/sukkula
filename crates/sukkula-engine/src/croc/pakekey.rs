//! croc 11's key schedule between the peers, "PAKE protocol version 2"
//! (`src/pakekey/pakekey.go`).
//!
//! The PAKE's points are croc 10's; what changed is around them:
//!
//! - the session key binds who is who: the room, what the exchange is for,
//!   and which side is the receiver (party A) and which the sender (B),
//!   framed into two identities ([`identities`], [`super::pake::Pake::bound_key`]);
//! - HKDF-SHA256 of that key, under a 32-byte salt from the sender, over a
//!   transcript of the exchange -- the two PAKE messages byte for byte as
//!   they went, the curve, the room -- gives the traffic key and a
//!   confirmation key for each side ([`derive`]);
//! - before anything is sealed, each side proves it has the key: the
//!   receiver's tag first, then the sender's (`pake-confirm`). A wrong code
//!   fails there, and not at the first sealed message.
//!
//! croc 10 has none of it, and the two refuse each other's exchange: a
//! croc 11 peer marks its PAKE messages `"v": 2`, and one that is not
//! marked is croc 10's.

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::pake::Pake;

/// The version croc 11's PAKE messages carry in `v`.
pub(super) const VERSION: i64 = 2;

/// Bytes of the sender's salt.
pub(super) const SALT_BYTES: usize = 32;

const IDENTITY_DOMAIN: &[u8] = b"croc/spake2/participant/v2";
const TRANSCRIPT_DOMAIN: &[u8] = b"croc/spake2/channel/v2";

/// The version, as it is framed: eight bytes, little-endian.
const VERSION_FIELD: [u8; 8] = 2u64.to_le_bytes();

/// What an exchange is for; the same code gives different keys for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Purpose {
    /// The transfer itself.
    Transfer,
    /// croc's LAN probe, before the transfer's own exchange.
    #[cfg(test)]
    Probe,
}

impl Purpose {
    fn name(self) -> &'static [u8] {
        match self {
            Purpose::Transfer => b"peer-transfer",
            #[cfg(test)]
            Purpose::Probe => b"local-ip-probe",
        }
    }
}

/// Which side of the exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    /// Party A, who speaks first.
    Receiver,
    /// Party B.
    Sender,
}

/// Each field as its length, eight bytes little-endian, then itself.
pub(super) fn frame(fields: &[&[u8]]) -> Vec<u8> {
    let total = fields
        .iter()
        .map(|f| f.len().saturating_add(8))
        .fold(0usize, usize::saturating_add);
    let mut out = Vec::with_capacity(total);
    for f in fields {
        let len = u64::try_from(f.len()).unwrap_or(u64::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(f);
    }
    out
}

/// The receiver's and the sender's identities for an exchange in `room`.
/// They never go on the wire; both sides know them.
pub(super) fn identities(purpose: Purpose, room: &str) -> (Vec<u8>, Vec<u8>) {
    let id = |side: &[u8]| {
        frame(&[
            IDENTITY_DOMAIN,
            &VERSION_FIELD,
            purpose.name(),
            room.as_bytes(),
            side,
        ])
    };
    (id(b"receiver"), id(b"sender"))
}

/// What the keys are bound to.
pub(super) struct Context<'a> {
    pub(super) purpose: Purpose,
    /// The room, as the relay knows it.
    pub(super) room: &'a str,
    /// The curve, by the name the receiver gave it.
    pub(super) curve: &'a str,
    /// The receiver's PAKE message, exactly as sent.
    pub(super) initiator: &'a [u8],
    /// The sender's, exactly as sent.
    pub(super) responder: &'a [u8],
    /// The sender's salt.
    pub(super) salt: &'a [u8; SALT_BYTES],
}

/// The traffic key and what proves each side has it.
pub(super) struct Keys {
    encryption: [u8; 32],
    confirm_receiver: [u8; 32],
    confirm_sender: [u8; 32],
    transcript: Vec<u8>,
}

impl std::fmt::Debug for Keys {
    /// S9: no key shows.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(<secret>)")
    }
}

/// The keys from the PAKE's session key `shared`.
pub(super) fn derive(shared: &[u8; 32], c: &Context<'_>) -> Keys {
    let (id_a, id_b) = identities(c.purpose, c.room);
    let transcript = frame(&[
        TRANSCRIPT_DOMAIN,
        &VERSION_FIELD,
        c.purpose.name(),
        c.room.as_bytes(),
        c.curve.as_bytes(),
        &id_a,
        &id_b,
        c.initiator,
        c.responder,
        c.salt,
    ]);
    let mut okm = [0u8; 96];
    // 96 bytes is far below HKDF-SHA256's limit of 8160.
    let _ = Hkdf::<Sha256>::new(Some(c.salt), shared).expand(&transcript, &mut okm);
    let part = |from: usize| -> [u8; 32] {
        okm.get(from..from.saturating_add(32))
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 32])
    };
    Keys {
        encryption: part(0),
        confirm_receiver: part(32),
        confirm_sender: part(64),
        transcript,
    }
}

/// The keys of a transfer's exchange in `room`, once `pake` has both
/// messages: `initiator` the receiver's and `responder` the sender's,
/// exactly as they went.
pub(super) fn for_transfer(
    pake: &Pake,
    room: &str,
    initiator: &[u8],
    responder: &[u8],
    salt: &[u8; SALT_BYTES],
) -> Option<Keys> {
    let (id_a, id_b) = identities(Purpose::Transfer, room);
    let shared = pake.bound_key((&id_a, &id_b))?;
    Some(derive(
        &shared,
        &Context {
            purpose: Purpose::Transfer,
            room,
            curve: pake.curve().name(),
            initiator,
            responder,
            salt,
        },
    ))
}

impl Keys {
    /// The AES-256-GCM key for everything after the exchange, both ways.
    pub(super) fn encryption(&self) -> &[u8; 32] {
        &self.encryption
    }

    fn mac(&self, side: Side) -> Hmac<Sha256> {
        let key = match side {
            Side::Receiver => &self.confirm_receiver,
            Side::Sender => &self.confirm_sender,
        };
        // HMAC takes a key of any length.
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
            .unwrap_or_else(|_| <Hmac<Sha256> as Mac>::new(&Default::default()));
        mac.update(&self.transcript);
        mac
    }

    /// `side`'s proof that it has the key.
    pub(super) fn tag(&self, side: Side) -> [u8; 32] {
        self.mac(side).finalize().into_bytes().into()
    }

    /// Whether `tag` is `side`'s proof: 32 bytes, compared in constant
    /// time.
    pub(super) fn verify(&self, side: Side, tag: &[u8]) -> bool {
        self.mac(side).verify_slice(tag).is_ok()
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // Test scenes.
mod tests {
    use sha2::Digest;

    use super::*;
    use crate::croc::hexs;
    use crate::croc::pake::{Curve, Pake};

    const ROOM: &str = "1cdafe5c70a87f001f2f437fa6773c0ee726d10b7c3f86184dc68fa709c2f355";

    /// croc-v11-delta.md A.2, from croc 11.5.4's own pakekey.
    #[test]
    fn identities_are_croc_11_s() {
        let (a, b) = identities(Purpose::Transfer, ROOM);
        let prefix = "1a0000000000000063726f632f7370616b65322f7061727469636970616e742f76320800000000\
                      00000002000000000000000d00000000000000706565722d7472616e73666572400000000000\
                      0000316364616665356337306138376630303166326634333766613637373363306565373236\
                      64313062376333663836313834646336386661373039633266333535";
        assert_eq!(
            hexs(&a),
            format!("{prefix}08000000000000007265636569766572")
        );
        assert_eq!(hexs(&b), format!("{prefix}060000000000000073656e646572"));
        assert_eq!((a.len(), b.len()), (159, 157));
    }

    #[test]
    fn tags_prove_the_key_and_nothing_else() {
        let salt = [5u8; SALT_BYTES];
        let c = Context {
            purpose: Purpose::Transfer,
            room: ROOM,
            curve: "p256",
            initiator: b"{\"Role\":0}",
            responder: b"{\"Role\":1}",
            salt: &salt,
        };
        let k = derive(&[1; 32], &c);
        let tag = k.tag(Side::Receiver);
        assert!(k.verify(Side::Receiver, &tag));
        assert!(!k.verify(Side::Sender, &tag), "a tag is its side's");
        assert!(!k.verify(Side::Receiver, &tag[..31]));
        assert!(!k.verify(Side::Receiver, &[tag.as_slice(), &[0]].concat()));
        let other = derive(&[2; 32], &c);
        assert!(!other.verify(Side::Receiver, &tag), "another key");
        let probe = derive(
            &[1; 32],
            &Context {
                purpose: Purpose::Probe,
                ..c
            },
        );
        assert_ne!(probe.encryption(), k.encryption());
        assert_eq!(format!("{k:?}"), "Keys(<secret>)");
    }

    /// croc-v11-delta.md A.3: croc 11.5.4's pakekey, end to end on P-256,
    /// with the secrets of croc-protocol.md A.2.
    #[test]
    fn keys_are_croc_11_s() {
        let fixed = |s: &str| -> [u8; 32] { Sha256::digest(s.as_bytes()).into() };
        let pw = b"alpha-bravo-charlie";
        let mut a = Pake::start(Curve::P256, pw, fixed("croc-vector-a")).unwrap();
        let initiator = a.message();
        let b = Pake::answer(Curve::P256, pw, fixed("croc-vector-b"), &initiator).unwrap();
        let responder = b.message();
        a.finish(&responder).unwrap();
        assert_eq!((initiator.len(), responder.len()), (634, 780));
        assert_eq!(a.message(), initiator, "role 0 says the same after");
        let (id_a, id_b) = identities(Purpose::Transfer, ROOM);
        let k = "dceadea24d54e82cf17d9269c5ef1add38757910cab7d884765d209d73eb43d6";
        assert_eq!(hexs(&a.bound_key((&id_a, &id_b)).unwrap()), k);
        assert_eq!(hexs(&b.bound_key((&id_a, &id_b)).unwrap()), k);
        let old = "1c6a21df3e4dba06e84c7eef4f35a716837cb7ef7711351a95ef63bf94e2c813";
        assert_eq!(
            hexs(&a.key().unwrap()),
            old,
            "croc 10's, from the same points"
        );

        let salt: [u8; SALT_BYTES] = std::array::from_fn(|i| u8::try_from(i).unwrap());
        let c = Context {
            purpose: Purpose::Transfer,
            room: ROOM,
            curve: "p256",
            initiator: &initiator,
            responder: &responder,
            salt: &salt,
        };
        let keys = derive(&a.bound_key((&id_a, &id_b)).unwrap(), &c);
        assert_eq!(keys.transcript.len(), 1953);
        assert_eq!(
            hexs(&Sha256::digest(&keys.transcript)),
            "08e6bdc84492f57f1c75257a4bd098f3ae2924b85b9806345664ff82d4c02e25"
        );
        assert_eq!(
            hexs(keys.encryption()),
            "5dadcd1b74abbd3e026d4a73289202e39fe15a882e2c1f8e62f0e19a8ff59817"
        );
        assert_eq!(
            hexs(&keys.tag(Side::Receiver)),
            "5a0debe0fe0559a6a0dd93cee715987e6c05a3837cb0c235419536e150557b5a"
        );
        assert_eq!(
            hexs(&keys.tag(Side::Sender)),
            "06365539d19cb7370b89f78ed868634228cbafb8c4f00cff65854b6893dd0dee"
        );
        let both = [&a, &b].map(|p| {
            for_transfer(p, ROOM, &initiator, &responder, &salt)
                .unwrap()
                .tag(Side::Receiver)
        });
        assert_eq!(both, [keys.tag(Side::Receiver); 2], "either side's");

        // The same exchange as a LAN probe.
        let (id_a, id_b) = identities(Purpose::Probe, ROOM);
        let k = a.bound_key((&id_a, &id_b)).unwrap();
        assert_eq!(
            hexs(&k),
            "c0fc6e7a1a6415c1593d08c72e79857146bf2212e11649c635eb771408a6724a"
        );
        let probe = derive(
            &k,
            &Context {
                purpose: Purpose::Probe,
                ..c
            },
        );
        assert_eq!(
            hexs(probe.encryption()),
            "8034d0ae43795fcbab34752292eeacaf74a81cfd20cb5c0d65df871218d72ad8"
        );
        assert_eq!(
            hexs(&probe.tag(Side::Receiver)),
            "4bcc29f1dc8e1664d26522070dc0ff559479a927d1a817517e7451872e11653a"
        );
    }
}
