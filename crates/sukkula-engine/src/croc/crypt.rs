//! croc's symmetric layer: a PBKDF2 key, AES-256-GCM, and raw DEFLATE.
//!
//! - The key for a connection is PBKDF2-HMAC-SHA256 of the PAKE's key
//!   with an 8-byte salt, 100 rounds, 32 bytes (`src/crypt/crypt.go`).
//! - Every sealed message is a fresh 12-byte nonce, then AES-256-GCM of
//!   the message with no associated data, tag last. Both directions use
//!   the same key, and nothing numbers the messages: the relay can replay,
//!   drop or reflect them, though not forge or read them. The state
//!   machines in `send.rs` and `receive.rs` take only the next message
//!   they expect, and never one of their own.
//! - Control messages are always raw DEFLATE (RFC 1951) before sealing,
//!   and file chunks unless the sender says otherwise. Ours are stored
//!   blocks, which every inflater reads; theirs are inflated with a hard
//!   cap on the output (a DEFLATE stream can expand a thousandfold).

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Bytes of a salt.
pub(super) const SALT_BYTES: usize = 8;

/// Bytes of a nonce.
const NONCE_BYTES: usize = 12;

/// Bytes of a tag.
const TAG_BYTES: usize = 16;

/// The most a stored DEFLATE block holds.
const STORED_BLOCK: usize = 65_535;

/// croc's PBKDF2 rounds.
const ROUNDS: u32 = 100;

/// PBKDF2-HMAC-SHA256 of `password` and `salt`, croc's 100 rounds, one
/// 32-byte block.
pub(super) fn derive(password: &[u8], salt: &[u8]) -> [u8; 32] {
    let prf = |data: &[&[u8]]| -> [u8; 32] {
        // HMAC takes a key of any length.
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(password)
            .unwrap_or_else(|_| <Hmac<Sha256> as Mac>::new(&Default::default()));
        for d in data {
            mac.update(d);
        }
        mac.finalize().into_bytes().into()
    };
    let mut u = prf(&[salt, &1u32.to_be_bytes()]);
    let mut t = u;
    for _ in 1..ROUNDS {
        u = prf(&[&u]);
        for (t, u) in t.iter_mut().zip(u.iter()) {
            *t ^= u;
        }
    }
    t
}

/// A connection's cipher.
#[derive(Clone)]
pub(super) struct Cipher {
    aes: Aes256Gcm,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cipher(<key>)")
    }
}

impl Cipher {
    /// The cipher for a 32-byte key.
    pub(super) fn new(key: &[u8; 32]) -> Cipher {
        Cipher {
            aes: Aes256Gcm::new(key.into()),
        }
    }

    /// Seals `plaintext` under a nonce from `random`.
    pub(super) fn seal(&self, plaintext: &[u8], random: [u8; NONCE_BYTES]) -> Option<Vec<u8>> {
        let sealed = self
            .aes
            .encrypt(Nonce::from_slice(&random), plaintext)
            .ok()?;
        let mut out = Vec::with_capacity(NONCE_BYTES.checked_add(sealed.len())?);
        out.extend_from_slice(&random);
        out.extend_from_slice(&sealed);
        Some(out)
    }

    /// Opens a sealed message, or `None` if it does not authenticate.
    pub(super) fn open(&self, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < NONCE_BYTES.checked_add(TAG_BYTES)? {
            return None;
        }
        let (nonce, body) = sealed.split_at(NONCE_BYTES);
        self.aes.decrypt(Nonce::from_slice(nonce), body).ok()
    }
}

/// What sealing adds to a message.
#[cfg(test)]
pub(super) const SEAL_OVERHEAD: usize = NONCE_BYTES + TAG_BYTES;

/// `data` as raw DEFLATE in stored blocks.
pub(super) fn deflate(data: &[u8]) -> Vec<u8> {
    let blocks = data.len().div_ceil(STORED_BLOCK).max(1);
    let mut out = Vec::with_capacity(data.len().saturating_add(blocks.saturating_mul(5)));
    let mut chunks = data.chunks(STORED_BLOCK).peekable();
    if chunks.peek().is_none() {
        // One final, empty block.
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
        return out;
    }
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.push(u8::from(last));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// A raw DEFLATE stream's content, if it is whole, well-formed and at most
/// `limit` bytes.
pub(super) fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_with_limit(data, limit).ok()
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;
    use crate::croc::{hexs, unhex};

    #[test]
    fn go_vectors() {
        // croc-protocol.md A.1: the relay key and the sealed password.
        let k = unhex("216f7c556c2ea3aabc774a807909a7e2bac5217e0019e85586404e7c3a7a1eae");
        let kr = derive(&k, &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            hexs(&kr),
            "9fac499df660d78796f063eba76895fea5fd6e6db484a974c1676206569f38fd"
        );
        let nonce: [u8; 12] = [
            0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
        ];
        let sealed = Cipher::new(&kr).seal(b"pass123", nonce).unwrap();
        assert_eq!(
            hexs(&sealed),
            "a0a1a2a3a4a5a6a7a8a9aaab04278cbaf2e53b68e567289c7b6fa0348801559cf898c7"
        );
        assert_eq!(Cipher::new(&kr).open(&sealed).unwrap(), b"pass123");
        // A.2: the peers' key.
        let k = unhex("1c6a21df3e4dba06e84c7eef4f35a716837cb7ef7711351a95ef63bf94e2c813");
        let salt = unhex("1122334455667788");
        assert_eq!(
            hexs(&derive(&k, &salt)),
            "d12cfd5ab29e29bfd57de532872f21b956a93ea81a226ea2a459ae0453ca2df5"
        );
    }

    #[test]
    fn tampering_is_caught() {
        let c = Cipher::new(&[7; 32]);
        let mut sealed = c.seal(b"hello", [1; 12]).unwrap();
        assert_eq!(sealed.len(), 5 + SEAL_OVERHEAD);
        for i in 0..sealed.len() {
            sealed[i] ^= 1;
            assert!(c.open(&sealed).is_none(), "{i}");
            sealed[i] ^= 1;
        }
        assert!(c.open(&sealed[..27]).is_none());
        assert!(Cipher::new(&[8; 32]).open(&sealed).is_none());
    }

    #[test]
    fn deflate_round_trips_and_inflate_is_capped() {
        // Go's own output for {"t":"finished"} (A.6) inflates.
        let go = unhex("001000efff7b2274223a2266696e6973686564227d010000ffff");
        assert_eq!(inflate(&go, 100).unwrap(), br#"{"t":"finished"}"#);
        for len in [0usize, 1, 65_535, 65_536, 200_000] {
            let data: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
            let packed = deflate(&data);
            assert_eq!(inflate(&packed, len).unwrap(), data, "{len}");
            if len > 0 {
                assert!(inflate(&packed, len - 1).is_none(), "{len}");
            }
        }
        // A bomb: a megabyte of zeros in a few hundred bytes.
        let mut bomb = Vec::new();
        {
            let zeros = vec![0u8; 1 << 20];
            bomb.extend(miniz_oxide::deflate::compress_to_vec(&zeros, 9));
        }
        assert!(bomb.len() < 4096);
        assert!(inflate(&bomb, 64 * 1024).is_none());
        // Truncated, or not DEFLATE at all.
        let packed = deflate(b"hello world");
        assert!(inflate(&packed[..packed.len() - 1], 100).is_none());
        assert!(inflate(&[0xff, 0xff, 0xff], 100).is_none());
    }
}
