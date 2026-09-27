//! XXH64 with seed 0, the hash croc's senders announce by default
//! (`HashAlgorithm: "xxhash"`, cespare/xxhash's `Sum`), written out big-
//! endian. Streaming, so a file is hashed as it is read or received.
//!
//! Not a cryptographic hash, and not used as one: every chunk is already
//! authenticated by AES-GCM. What it catches is a chunk the relay moved
//! from one file to another, which per-chunk authentication does not.

const P1: u64 = 11_400_714_785_074_694_791;
const P2: u64 = 14_029_467_366_897_019_727;
const P3: u64 = 1_609_587_929_392_839_161;
const P4: u64 = 9_650_029_242_287_828_579;
const P5: u64 = 2_870_177_450_012_600_261;

fn round(acc: u64, lane: u64) -> u64 {
    acc.wrapping_add(lane.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(acc: u64, v: u64) -> u64 {
    (acc ^ round(0, v)).wrapping_mul(P1).wrapping_add(P4)
}

fn lane(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    for (d, s) in b.iter_mut().zip(bytes) {
        *d = *s;
    }
    u64::from_le_bytes(b)
}

/// A running XXH64.
#[derive(Clone, Debug)]
pub(super) struct Xxh64 {
    v: [u64; 4],
    buf: [u8; 32],
    held: usize,
    total: u64,
}

impl Default for Xxh64 {
    fn default() -> Self {
        Xxh64 {
            v: [P1.wrapping_add(P2), P2, 0, 0u64.wrapping_sub(P1)],
            buf: [0; 32],
            held: 0,
            total: 0,
        }
    }
}

impl Xxh64 {
    fn stripe(&mut self, stripe: &[u8]) {
        for (i, v) in self.v.iter_mut().enumerate() {
            let at = i.wrapping_mul(8);
            *v = round(*v, lane(stripe.get(at..).unwrap_or_default()));
        }
    }

    /// Adds `data`.
    pub(super) fn update(&mut self, mut data: &[u8]) {
        self.total = self
            .total
            .wrapping_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        if self.held > 0 {
            let want = 32usize.saturating_sub(self.held);
            let take = want.min(data.len());
            let (now, rest) = data.split_at(take);
            if let Some(dst) = self.buf.get_mut(self.held..self.held.saturating_add(take)) {
                dst.copy_from_slice(now);
            }
            self.held = self.held.saturating_add(take);
            data = rest;
            if self.held < 32 {
                return;
            }
            let buf = self.buf;
            self.stripe(&buf);
            self.held = 0;
        }
        let mut stripes = data.chunks_exact(32);
        for s in &mut stripes {
            self.stripe(s);
        }
        let tail = stripes.remainder();
        if let Some(dst) = self.buf.get_mut(..tail.len()) {
            dst.copy_from_slice(tail);
        }
        self.held = tail.len();
    }

    /// The hash, as croc sends it: eight bytes, big-endian.
    pub(super) fn finish(&self) -> [u8; 8] {
        let [v1, v2, v3, v4] = self.v;
        let mut h = if self.total >= 32 {
            let mut h = v1
                .rotate_left(1)
                .wrapping_add(v2.rotate_left(7))
                .wrapping_add(v3.rotate_left(12))
                .wrapping_add(v4.rotate_left(18));
            for v in [v1, v2, v3, v4] {
                h = merge(h, v);
            }
            h
        } else {
            P5
        };
        h = h.wrapping_add(self.total);
        let mut rest = self.buf.get(..self.held).unwrap_or_default();
        while rest.len() >= 8 {
            let (w, r) = rest.split_at(8);
            h = (h ^ round(0, lane(w)))
                .rotate_left(27)
                .wrapping_mul(P1)
                .wrapping_add(P4);
            rest = r;
        }
        if rest.len() >= 4 {
            let (w, r) = rest.split_at(4);
            h = (h ^ lane(w).wrapping_mul(P1))
                .rotate_left(23)
                .wrapping_mul(P2)
                .wrapping_add(P3);
            rest = r;
        }
        for b in rest {
            h = (h ^ u64::from(*b).wrapping_mul(P5))
                .rotate_left(11)
                .wrapping_mul(P1);
        }
        h ^= h.wrapping_shr(33);
        h = h.wrapping_mul(P2);
        h ^= h.wrapping_shr(29);
        h = h.wrapping_mul(P3);
        h ^= h.wrapping_shr(32);
        h.to_be_bytes()
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;
    use crate::croc::hexs;

    fn once(data: &[u8]) -> String {
        let mut h = Xxh64::default();
        h.update(data);
        hexs(&h.finish())
    }

    #[test]
    fn known_values() {
        assert_eq!(once(b""), "ef46db3751d8e999");
        assert_eq!(once(b"a"), "d24ec4f1a98c6e5b");
        assert_eq!(once(b"abc"), "44bc2cf5ad770999");
        // croc-protocol.md A.5, from cespare/xxhash.
        assert_eq!(once(b"hello world\n"), "5215e13b207d6d8c");
        let big: Vec<u8> = (0..(10 * 1024 * 1024 + 7))
            .map(|i: u32| u8::try_from(i.wrapping_mul(7) & 0xff).unwrap())
            .collect();
        assert_eq!(once(&big), "cb7b9cd4f5792434");
    }

    #[test]
    fn pieces_hash_as_the_whole() {
        let data: Vec<u8> = (0..1000u32)
            .map(|i| u8::try_from(i % 256).unwrap())
            .collect();
        let whole = once(&data);
        for split in [1usize, 3, 31, 32, 33, 64, 999] {
            let mut h = Xxh64::default();
            for piece in data.chunks(split) {
                h.update(piece);
            }
            assert_eq!(hexs(&h.finish()), whole, "{split}");
        }
    }
}
