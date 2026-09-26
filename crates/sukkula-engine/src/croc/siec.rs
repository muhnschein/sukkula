//! SIEC255, the curve croc's relay handshake runs its PAKE on: `y² = x³ +
//! 19` over a 255-bit prime, of prime order, with base point (5, 12).
//!
//! Every connection to a croc relay starts with a PAKE on this curve with
//! the public password `{1, 2, 3}` (schollz/pake v3, `github.com/tscholl2/
//! siec`), so a croc client has to speak it whatever curve the two peers
//! then use between themselves. The password is public, so nothing here
//! needs to be constant-time, and the Go original is not either.
//!
//! Textbook affine arithmetic modulo p, with crypto-bigint's Montgomery
//! residues; inversion by Fermat. The group order n is prime and the
//! cofactor 1, so a point that satisfies the equation is in the group:
//! the curve check is the whole check.

use crypto_bigint::modular::runtime_mod::{DynResidue, DynResidueParams};
use crypto_bigint::{Encoding, U256};

/// The field prime.
const P: U256 =
    U256::from_be_hex("4000000000000000000000000200104080000000000000000004004103082041");

/// The group order.
const N: U256 =
    U256::from_be_hex("4000000000000000000000000200103f800000000000000000040040ff07ffc1");

/// The curve's `b`.
const B: u64 = 19;

type Fe = DynResidue<4>;

fn params() -> DynResidueParams<4> {
    DynResidueParams::new(&P)
}

fn fe(v: &U256) -> Fe {
    DynResidue::new(v, params())
}

fn small(v: u64) -> Fe {
    fe(&U256::from_u64(v))
}

/// A point on SIEC255, or the point at infinity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Point {
    /// The identity.
    Infinity,
    /// (x, y), both reduced modulo p.
    Affine(U256, U256),
}

impl Point {
    /// The base point (5, 12).
    pub(super) fn generator() -> Point {
        Point::Affine(U256::from_u64(5), U256::from_u64(12))
    }

    /// The point (x, y), if both are below p and it is on the curve.
    pub(super) fn from_affine(x: U256, y: U256) -> Option<Point> {
        let point = Point::Affine(x, y);
        (x < P && y < P && point.is_on_curve()).then_some(point)
    }

    fn is_on_curve(&self) -> bool {
        match self {
            Point::Infinity => false,
            Point::Affine(x, y) => {
                let x = fe(x);
                let y = fe(y);
                y.square() == x.square().mul(&x).add(&small(B))
            }
        }
    }

    /// Its coordinates, or `None` for the identity.
    pub(super) fn coordinates(&self) -> Option<(U256, U256)> {
        match self {
            Point::Infinity => None,
            Point::Affine(x, y) => Some((*x, *y)),
        }
    }

    /// `-self`.
    pub(super) fn negate(&self) -> Point {
        match self {
            Point::Infinity => Point::Infinity,
            Point::Affine(x, y) => Point::Affine(*x, fe(y).neg().retrieve()),
        }
    }

    /// `self + other`.
    pub(super) fn add(&self, other: &Point) -> Point {
        let (Point::Affine(x1, y1), Point::Affine(x2, y2)) = (self, other) else {
            return if *self == Point::Infinity {
                *other
            } else {
                *self
            };
        };
        let (fx1, fy1, fx2, fy2) = (fe(x1), fe(y1), fe(x2), fe(y2));
        if fx1 == fx2 {
            return if fy1.add(&fy2) == Fe::zero(params()) {
                Point::Infinity
            } else {
                self.double()
            };
        }
        let lambda = fy2.sub(&fy1).mul(&inverse(&fx2.sub(&fx1)));
        let x3 = lambda.square().sub(&fx1).sub(&fx2);
        let y3 = lambda.mul(&fx1.sub(&x3)).sub(&fy1);
        Point::Affine(x3.retrieve(), y3.retrieve())
    }

    /// `2·self`.
    pub(super) fn double(&self) -> Point {
        let Point::Affine(x, y) = self else {
            return Point::Infinity;
        };
        let (fx, fy) = (fe(x), fe(y));
        if fy == Fe::zero(params()) {
            return Point::Infinity;
        }
        // a = 0: λ = 3x² / 2y.
        let lambda = small(3).mul(&fx.square()).mul(&inverse(&fy.add(&fy)));
        let x3 = lambda.square().sub(&fx).sub(&fx);
        let y3 = lambda.mul(&fx.sub(&x3)).sub(&fy);
        Point::Affine(x3.retrieve(), y3.retrieve())
    }

    /// `k·self`, for a scalar already reduced modulo n.
    pub(super) fn mul(&self, k: &U256) -> Point {
        let mut acc = Point::Infinity;
        let bits = k.bits_vartime();
        for i in (0..bits).rev() {
            acc = acc.double();
            if k.bit_vartime(i) {
                acc = acc.add(self);
            }
        }
        acc
    }
}

fn inverse(v: &Fe) -> Fe {
    // p is prime and v is never zero where this is called (x2 ≠ x1, y ≠ 0),
    // so the inverse exists; `invert` is Fermat's.
    v.invert().0
}

/// `bytes`, read big-endian and of any length, modulo n: Go's ScalarMult
/// runs over every bit of the byte string, and since n·G is the identity
/// that is the same point as the reduced scalar gives.
pub(super) fn scalar(bytes: &[u8]) -> U256 {
    let n = DynResidueParams::new(&N);
    let base = DynResidue::new(&U256::from_u64(256), n);
    let mut acc = DynResidue::zero(n);
    for b in bytes {
        acc = acc
            .mul(&base)
            .add(&DynResidue::new(&U256::from_u64(u64::from(*b)), n));
    }
    acc.retrieve()
}

/// A field element's big-endian bytes, 32 of them.
pub(super) fn to_bytes(v: &U256) -> [u8; 32] {
    v.to_be_bytes()
}

/// 32 big-endian bytes as a number, whatever its size.
pub(super) fn from_bytes(bytes: &[u8; 32]) -> U256 {
    U256::from_be_slice(bytes)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;

    #[test]
    fn the_curve_is_what_croc_uses() {
        let g = Point::generator();
        assert!(g.is_on_curve());
        // n·G is the identity: the order is n, the cofactor 1.
        assert_eq!(g.mul(&N), Point::Infinity);
        assert_ne!(g.mul(&N.wrapping_sub(&U256::ONE)), Point::Infinity);
        // (n-1)·G = -G.
        assert_eq!(g.mul(&N.wrapping_sub(&U256::ONE)), g.negate());
        assert_eq!(g.add(&g.negate()), Point::Infinity);
        assert_eq!(g.add(&Point::Infinity), g);
        assert_eq!(Point::Infinity.add(&g), g);
        assert_eq!(g.add(&g), g.double());
        let three = g.mul(&U256::from_u64(3));
        assert_eq!(three, g.double().add(&g));
        assert!(three.is_on_curve());
    }

    #[test]
    fn points_off_the_curve_or_out_of_range_are_refused() {
        assert!(Point::from_affine(U256::from_u64(5), U256::from_u64(12)).is_some());
        assert!(Point::from_affine(U256::from_u64(5), U256::from_u64(13)).is_none());
        assert!(Point::from_affine(U256::ZERO, U256::ZERO).is_none());
        // The same point written with x + p is not canonical.
        let x = U256::from_u64(5).wrapping_add(&P);
        assert!(Point::from_affine(x, U256::from_u64(12)).is_none());
    }

    #[test]
    fn scalars_are_reduced_modulo_n() {
        assert_eq!(scalar(&[1, 2, 3]), U256::from_u64(0x0001_0203));
        assert_eq!(scalar(&[]), U256::ZERO);
        let n = to_bytes(&N);
        assert_eq!(scalar(&n), U256::ZERO);
        let mut longer = vec![0xff; 40];
        longer.extend_from_slice(&[7]);
        let g = Point::generator();
        // Reduction changes nothing about the point.
        let reduced = scalar(&longer);
        assert!(reduced < N);
        let mut direct = Point::Infinity;
        for b in &longer {
            for bit in (0..8).rev() {
                direct = direct.double();
                if (b >> bit) & 1 == 1 {
                    direct = direct.add(&g);
                }
            }
        }
        assert_eq!(g.mul(&reduced), direct);
    }
}
