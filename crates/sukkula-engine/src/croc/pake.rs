//! schollz/pake v3.1.1, the PAKE croc runs twice: on SIEC255 with the
//! public password `{1, 2, 3}` to every relay connection, and between the
//! two peers on the curve the receiver names (`p256` by default), with the
//! code's words as the password.
//!
//! Role 0 (the receiver, or the relay's client) picks `a` and sends
//! `X = w·U + a·G`; role 1 picks `b`, sends `Y = w·V + b·G`, and both get
//! `Z` (`a·(Y − w·V)`, `b·(X − w·U)`) and the key
//! `K = SHA-256(pw ‖ X ‖ Y ‖ Z)`, each coordinate as its shortest
//! big-endian bytes. There is no key confirmation: a wrong password shows
//! up as the first message that does not decrypt.
//!
//! The messages are Go's `json.Marshal` of the whole `Pake` struct: keys
//! in Unicode (`Xᵤ`, `Xᵥ`), coordinates as bare decimal numbers too big
//! for any JSON number type, and every private field as `null`. They are
//! written here byte for byte, and read with only what is used taken:
//! `Role` and the other side's point, which must be on the curve.
//!
//! Only SIEC255 and P-256 are spoken; a receiver asking for `p384`,
//! `p521` or `ed25519` (croc's `--curve`, which nobody changes) is
//! refused.

use crypto_bigint::U256;
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{AffinePoint, EncodedPoint, FieldBytes, ProjectivePoint, Scalar};
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use super::siec;

/// Longest PAKE message read, in bytes. Go's are under 2 KiB.
pub(super) const MAX_PAKE_BYTES: usize = 8 * 1024;

/// Longest decimal coordinate read: 2^256 has 78 digits.
const MAX_DIGITS: usize = 78;

/// The curves spoken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Curve {
    /// croc's own, for every relay connection.
    Siec,
    /// NIST P-256, croc's default between peers.
    P256,
}

impl Curve {
    /// The name croc sends for it.
    pub(super) fn name(self) -> &'static str {
        match self {
            Curve::Siec => "siec",
            Curve::P256 => "p256",
        }
    }

    /// The curve croc names so, if spoken here.
    pub(super) fn from_name(name: &[u8]) -> Option<Curve> {
        match name {
            b"siec" => Some(Curve::Siec),
            b"p256" => Some(Curve::P256),
            _ => None,
        }
    }

    /// pake's fixed points U and V, as it writes them.
    fn uv(self) -> [&'static str; 4] {
        match self {
            Curve::Siec => [
                "793136080485469241208656611513609866400481671853",
                "18458907634222644275952014841865282643645472623913459400556233196838128612339",
                "1086685267857089638167386722555472967068468061489",
                "19593504966619549205903364028255899745298716108914514072669075231742699650911",
            ],
            Curve::P256 => [
                "793136080485469241208656611513609866400481671852",
                "59748757929350367369315811184980635230185250460108398961713395032485227207304",
                "1086685267857089638167386722555472967068468061489",
                "9157340230202296554417312816309453883742349874205386245733062928888341584123",
            ],
        }
    }

    /// Which curve a peer's message is on, from its U: the one thing that
    /// tells a LAN probe's curve, which the probe does not name.
    pub(super) fn of_message(json: &[u8]) -> Option<Curve> {
        let wire: Wire<'_> = serde_json::from_slice(json).ok()?;
        let u = wire.ux?.get();
        [Curve::Siec, Curve::P256]
            .into_iter()
            .find(|c| c.uv().first() == Some(&u))
    }
}

/// Why a PAKE failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PakeError {
    /// The message is not pake's JSON, or is too long.
    Malformed,
    /// It came from the same role, or is not the answer expected.
    WrongRole,
    /// Its point is not on the curve.
    NotOnCurve,
}

/// A point on one of the curves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    Siec(siec::Point),
    P256(ProjectivePoint),
}

/// A scalar for one of the curves, already reduced.
#[derive(Clone, Copy)]
enum Secret {
    Siec(U256),
    P256(Scalar),
}

impl Curve {
    fn generator(self) -> Point {
        match self {
            Curve::Siec => Point::Siec(siec::Point::generator()),
            Curve::P256 => Point::P256(ProjectivePoint::GENERATOR),
        }
    }

    fn fixed(self, which: usize) -> Option<Point> {
        let uv = self.uv();
        let x = uv.get(which.checked_mul(2)?)?;
        let y = uv.get(which.checked_mul(2)?.checked_add(1)?)?;
        self.point(x, y)
    }

    /// The point with these decimal coordinates, if on the curve.
    fn point(self, x: &str, y: &str) -> Option<Point> {
        let x = decimal_to_bytes(x)?;
        let y = decimal_to_bytes(y)?;
        match self {
            Curve::Siec => siec::Point::from_affine(siec::from_bytes(&x), siec::from_bytes(&y))
                .map(Point::Siec),
            Curve::P256 => {
                let encoded = EncodedPoint::from_affine_coordinates(
                    FieldBytes::from_slice(&x),
                    FieldBytes::from_slice(&y),
                    false,
                );
                Option::<AffinePoint>::from(AffinePoint::from_encoded_point(&encoded))
                    .map(|p| Point::P256(ProjectivePoint::from(p)))
            }
        }
    }

    /// `bytes`, big-endian and of any length, as a scalar modulo the order.
    fn scalar(self, bytes: &[u8]) -> Secret {
        match self {
            Curve::Siec => Secret::Siec(siec::scalar(bytes)),
            Curve::P256 => Secret::P256(p256_scalar(bytes)),
        }
    }
}

// Group and field operations are modular: nothing here can overflow.
#[allow(clippy::arithmetic_side_effects)]
fn p256_scalar(bytes: &[u8]) -> Scalar {
    let base = Scalar::from(256u64);
    bytes.iter().fold(Scalar::ZERO, |acc, b| {
        acc * base + Scalar::from(u64::from(*b))
    })
}

impl Point {
    // Group operations: modular, no overflow.
    #[allow(clippy::arithmetic_side_effects)]
    fn add(self, other: Point) -> Option<Point> {
        match (self, other) {
            (Point::Siec(a), Point::Siec(b)) => Some(Point::Siec(a.add(&b))),
            (Point::P256(a), Point::P256(b)) => Some(Point::P256(a + b)),
            _ => None,
        }
    }

    #[allow(clippy::arithmetic_side_effects)]
    fn negate(self) -> Point {
        match self {
            Point::Siec(a) => Point::Siec(a.negate()),
            Point::P256(a) => Point::P256(-a),
        }
    }

    #[allow(clippy::arithmetic_side_effects)]
    fn mul(self, k: Secret) -> Option<Point> {
        match (self, k) {
            (Point::Siec(p), Secret::Siec(k)) => Some(Point::Siec(p.mul(&k))),
            (Point::P256(p), Secret::P256(k)) => Some(Point::P256(p * k)),
            _ => None,
        }
    }

    /// Its coordinates as 32 big-endian bytes each; `None` for the
    /// identity, which no honest exchange reaches.
    fn coordinates(self) -> Option<([u8; 32], [u8; 32])> {
        match self {
            Point::Siec(p) => {
                let (x, y) = p.coordinates()?;
                Some((siec::to_bytes(&x), siec::to_bytes(&y)))
            }
            Point::P256(p) => {
                let encoded = p.to_affine().to_encoded_point(false);
                let x: [u8; 32] = encoded.x()?.as_slice().try_into().ok()?;
                let y: [u8; 32] = encoded.y()?.as_slice().try_into().ok()?;
                Some((x, y))
            }
        }
    }
}

/// One side of a PAKE.
pub(super) struct Pake {
    curve: Curve,
    role: u8,
    pw: Vec<u8>,
    secret: Secret,
    /// Role 0's own X, or the X role 1 received.
    x: Option<([u8; 32], [u8; 32])>,
    /// Role 1's own Y.
    y: Option<([u8; 32], [u8; 32])>,
    key: Option<[u8; 32]>,
}

impl std::fmt::Debug for Pake {
    /// S9: the password and the secret never show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pake")
            .field("curve", &self.curve)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl Pake {
    /// Role 0, the side that speaks first: croc's receiver, or a relay's
    /// client. `random` is 32 bytes from the system's generator.
    pub(super) fn start(curve: Curve, pw: &[u8], random: [u8; 32]) -> Option<Pake> {
        let w = curve.scalar(pw);
        let a = curve.scalar(&random);
        let x = curve
            .fixed(0)?
            .mul(w)?
            .add(curve.generator().mul(a)?)?
            .coordinates()?;
        Some(Pake {
            curve,
            role: 0,
            pw: pw.to_vec(),
            secret: a,
            x: Some(x),
            y: None,
            key: None,
        })
    }

    /// Role 1, the side that answers: croc's sender, or a relay. Takes
    /// role 0's message and has the key at once.
    ///
    /// # Errors
    ///
    /// The message is malformed, from role 1, or off the curve.
    pub(super) fn answer(
        curve: Curve,
        pw: &[u8],
        random: [u8; 32],
        theirs: &[u8],
    ) -> Result<Pake, PakeError> {
        let wire = read(theirs)?;
        if wire.role != 0 {
            return Err(PakeError::WrongRole);
        }
        let x_point = point_of(curve, wire.xu, wire.xv)?;
        let w = curve.scalar(pw);
        let b = curve.scalar(&random);
        let off = PakeError::NotOnCurve;
        let y_point = curve
            .fixed(1)
            .and_then(|v| v.mul(w))
            .and_then(|wv| wv.add(curve.generator().mul(b)?))
            .ok_or(off)?;
        let wu = curve.fixed(0).and_then(|u| u.mul(w)).ok_or(off)?;
        let z = x_point
            .add(wu.negate())
            .and_then(|d| d.mul(b))
            .and_then(Point::coordinates)
            .ok_or(off)?;
        let x = x_point.coordinates().ok_or(off)?;
        let y = y_point.coordinates().ok_or(off)?;
        let key = session_key(pw, &x, &y, &z);
        Ok(Pake {
            curve,
            role: 1,
            pw: pw.to_vec(),
            secret: b,
            x: Some(x),
            y: Some(y),
            key: Some(key),
        })
    }

    /// Role 0 takes role 1's answer and has the key.
    ///
    /// # Errors
    ///
    /// The answer is malformed, not from role 1, or off the curve.
    pub(super) fn finish(&mut self, theirs: &[u8]) -> Result<(), PakeError> {
        if self.role != 0 || self.key.is_some() {
            return Err(PakeError::WrongRole);
        }
        let wire = read(theirs)?;
        if wire.role != 1 {
            return Err(PakeError::WrongRole);
        }
        let off = PakeError::NotOnCurve;
        let y_point = point_of(self.curve, wire.yu, wire.yv)?;
        let w = self.curve.scalar(&self.pw);
        let wv = self.curve.fixed(1).and_then(|v| v.mul(w)).ok_or(off)?;
        let z = y_point
            .add(wv.negate())
            .and_then(|d| d.mul(self.secret))
            .and_then(Point::coordinates)
            .ok_or(off)?;
        let y = y_point.coordinates().ok_or(off)?;
        // Role 0 hashes its own X, whatever role 1 echoed.
        let x = self.x.ok_or(off)?;
        self.key = Some(session_key(&self.pw, &x, &y, &z));
        Ok(())
    }

    /// The shared key, once both sides have spoken.
    pub(super) fn key(&self) -> Option<[u8; 32]> {
        self.key
    }

    /// This side's message, as Go's `json.Marshal` writes it.
    pub(super) fn message(&self) -> Vec<u8> {
        let [ux, uy, vx, vy] = self.curve.uv();
        let pair = |p: Option<([u8; 32], [u8; 32])>| match p {
            Some((x, y)) => (bytes_to_decimal(&x), bytes_to_decimal(&y)),
            None => ("null".to_owned(), "null".to_owned()),
        };
        let (xx, xy) = pair(self.x);
        let (yx, yy) = pair(self.y);
        format!(
            "{{\"Role\":{},\"Uᵤ\":{ux},\"Uᵥ\":{uy},\"Vᵤ\":{vx},\"Vᵥ\":{vy},\"Xᵤ\":{xx},\"Xᵥ\":{xy},\
             \"Yᵤ\":{yx},\"Yᵥ\":{yy},\"P\":null,\"Pw\":null,\"Vpwᵤ\":null,\"Vpwᵥ\":null,\
             \"Upwᵤ\":null,\"Upwᵥ\":null,\"Aα\":null,\"Aαᵤ\":null,\"Aαᵥ\":null,\"Zᵤ\":null,\
             \"Zᵥ\":null,\"K\":null}}",
            self.role
        )
        .into_bytes()
    }
}

/// What is read of a PAKE message. Everything else in it is ignored, as
/// Go ignores it; a key given twice is refused.
#[derive(Deserialize)]
struct Wire<'a> {
    #[serde(rename = "Role")]
    role: u8,
    #[serde(rename = "Uᵤ", borrow, default)]
    ux: Option<&'a RawValue>,
    #[serde(rename = "Xᵤ", borrow, default)]
    xu: Option<&'a RawValue>,
    #[serde(rename = "Xᵥ", borrow, default)]
    xv: Option<&'a RawValue>,
    #[serde(rename = "Yᵤ", borrow, default)]
    yu: Option<&'a RawValue>,
    #[serde(rename = "Yᵥ", borrow, default)]
    yv: Option<&'a RawValue>,
}

fn read(json: &[u8]) -> Result<Wire<'_>, PakeError> {
    if json.len() > MAX_PAKE_BYTES {
        return Err(PakeError::Malformed);
    }
    let wire: Wire<'_> = serde_json::from_slice(json).map_err(|_| PakeError::Malformed)?;
    if wire.role > 1 {
        return Err(PakeError::Malformed);
    }
    Ok(wire)
}

fn point_of(curve: Curve, x: Option<&RawValue>, y: Option<&RawValue>) -> Result<Point, PakeError> {
    let (Some(x), Some(y)) = (x, y) else {
        return Err(PakeError::Malformed);
    };
    curve.point(x.get(), y.get()).ok_or(PakeError::NotOnCurve)
}

fn session_key(
    pw: &[u8],
    x: &([u8; 32], [u8; 32]),
    y: &([u8; 32], [u8; 32]),
    z: &([u8; 32], [u8; 32]),
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(pw);
    for c in [&x.0, &x.1, &y.0, &y.1, &z.0, &z.1] {
        h.update(shortest(c));
    }
    h.finalize().into()
}

/// Go's `big.Int.Bytes()`: no leading zero bytes, empty for zero.
fn shortest(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    bytes.get(first..).unwrap_or_default()
}

/// A non-negative decimal number below 2^256, as 32 big-endian bytes.
/// Digits only: no sign, point, exponent or leading zero.
fn decimal_to_bytes(digits: &str) -> Option<[u8; 32]> {
    if digits.is_empty()
        || digits.len() > MAX_DIGITS
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for d in digits.bytes() {
        let mut carry = u32::from(d.wrapping_sub(b'0'));
        for byte in out.iter_mut().rev() {
            let v = u32::from(*byte).wrapping_mul(10).wrapping_add(carry);
            *byte = v.to_le_bytes().first().copied().unwrap_or(0);
            carry = v.wrapping_shr(8);
        }
        if carry != 0 {
            return None;
        }
    }
    Some(out)
}

/// Big-endian bytes as a decimal number, as Go writes a `big.Int`.
fn bytes_to_decimal(bytes: &[u8]) -> String {
    let mut n: Vec<u8> = shortest(bytes).to_vec();
    let mut digits = Vec::new();
    while !n.is_empty() {
        let mut rem = 0u32;
        for byte in &mut n {
            let cur = rem.wrapping_shl(8).wrapping_add(u32::from(*byte));
            *byte = cur
                .checked_div(10)
                .and_then(|q| u8::try_from(q).ok())
                .unwrap_or(0);
            rem = cur.checked_rem(10).unwrap_or(0);
        }
        digits.push(char::from(
            b'0'.wrapping_add(u8::try_from(rem).unwrap_or(0)),
        ));
        n = shortest(&n).to_vec();
    }
    if digits.is_empty() {
        return "0".to_owned();
    }
    digits.iter().rev().collect()
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;
    use crate::croc::hexs;

    fn fixed(s: &str) -> [u8; 32] {
        let digest = Sha256::digest(s.as_bytes());
        digest.into()
    }

    /// croc-protocol.md A.1 and A.2: fixed secrets `a` and `b` are the
    /// SHA-256 of "croc-vector-a" and "croc-vector-b", as generated by a Go
    /// program checked against the real pake v3.1.1.
    #[test]
    fn go_vectors() {
        // A.1: the relay handshake, SIEC, password {1, 2, 3}.
        let mut a = Pake::start(Curve::Siec, &[1, 2, 3], fixed("croc-vector-a")).unwrap();
        let expected_x = "{\"Role\":0,\"Uᵤ\":793136080485469241208656611513609866400481671853,\
            \"Uᵥ\":18458907634222644275952014841865282643645472623913459400556233196838128612339,\
            \"Vᵤ\":1086685267857089638167386722555472967068468061489,\
            \"Vᵥ\":19593504966619549205903364028255899745298716108914514072669075231742699650911,\
            \"Xᵤ\":5633704656006211133580926226641580291303874869166156690972471052437044143455,\
            \"Xᵥ\":5568602253916900023409587281462480261952498506279037544955668022381262656404,\
            \"Yᵤ\":null,\"Yᵥ\":null,\"P\":null,\"Pw\":null,\"Vpwᵤ\":null,\"Vpwᵥ\":null,\
            \"Upwᵤ\":null,\"Upwᵥ\":null,\"Aα\":null,\"Aαᵤ\":null,\"Aαᵥ\":null,\"Zᵤ\":null,\
            \"Zᵥ\":null,\"K\":null}";
        assert_eq!(String::from_utf8(a.message()).unwrap(), expected_x);
        let b = Pake::answer(
            Curve::Siec,
            &[1, 2, 3],
            fixed("croc-vector-b"),
            &a.message(),
        )
        .unwrap();
        let reply = String::from_utf8(b.message()).unwrap();
        assert!(reply.contains(
            "\"Yᵤ\":9080542243294411600430919373058018009409103076559338519783691898967338041811,\
             \"Yᵥ\":1521832525987881265019740730322656938292482916578058869767977749992497092698"
        ));
        assert!(reply.starts_with("{\"Role\":1,"));
        a.finish(&b.message()).unwrap();
        let k = "216f7c556c2ea3aabc774a807909a7e2bac5217e0019e85586404e7c3a7a1eae";
        assert_eq!(hexs(&a.key().unwrap()), k);
        assert_eq!(hexs(&b.key().unwrap()), k);

        // A.2: between peers, P-256, the words of 8123-alpha-bravo-charlie.
        let pw = b"alpha-bravo-charlie";
        let mut a = Pake::start(Curve::P256, pw, fixed("croc-vector-a")).unwrap();
        assert!(String::from_utf8(a.message()).unwrap().contains(
            "\"Xᵤ\":82485295943025638528575497232044638747577828331433727908582028481249783334660,\
             \"Xᵥ\":109178262122709344473445817776912190285412631988058359859823355608104734631581"
        ));
        let b = Pake::answer(Curve::P256, pw, fixed("croc-vector-b"), &a.message()).unwrap();
        assert!(String::from_utf8(b.message()).unwrap().contains(
            "\"Yᵤ\":53239331987804511771316662971896869523828877277788126819257697498096954938206,\
             \"Yᵥ\":46966144410653806264068858398530044253525139726366355808296253071513750629288"
        ));
        a.finish(&b.message()).unwrap();
        let k = "1c6a21df3e4dba06e84c7eef4f35a716837cb7ef7711351a95ef63bf94e2c813";
        assert_eq!(hexs(&a.key().unwrap()), k);
        assert_eq!(hexs(&b.key().unwrap()), k);
    }

    #[test]
    fn a_wrong_password_gives_another_key() {
        for curve in [Curve::Siec, Curve::P256] {
            let mut a = Pake::start(curve, b"right", [7; 32]).unwrap();
            let b = Pake::answer(curve, b"wrong", [9; 32], &a.message()).unwrap();
            a.finish(&b.message()).unwrap();
            assert_ne!(a.key(), b.key());
        }
    }

    #[test]
    fn messages_that_do_not_hold_are_refused() {
        let a = Pake::start(Curve::P256, b"pw", [7; 32]).unwrap();
        let good = String::from_utf8(a.message()).unwrap();
        // Role 1 cannot answer role 1, nor role 0 take role 0.
        let b = Pake::answer(Curve::P256, b"pw", [9; 32], good.as_bytes()).unwrap();
        assert_eq!(
            Pake::answer(Curve::P256, b"pw", [9; 32], &b.message()).unwrap_err(),
            PakeError::WrongRole
        );
        let mut again = Pake::start(Curve::P256, b"pw", [1; 32]).unwrap();
        assert_eq!(
            again.finish(good.as_bytes()).unwrap_err(),
            PakeError::WrongRole
        );
        // A point on the other curve, or on none.
        assert_eq!(
            Pake::answer(Curve::Siec, b"pw", [9; 32], good.as_bytes()).unwrap_err(),
            PakeError::NotOnCurve
        );
        for (from, to) in [
            ("\"Xᵥ\":", "\"Xᵥ\":1"),
            ("\"Xᵤ\":", "\"Xᵤ\":-"),
            ("\"Xᵤ\":", "\"Xᵤ\":0"),
            (",\"Xᵥ\":", ".5,\"Xᵥ\":"),
            (",\"Xᵥ\":", "e0,\"Xᵥ\":"),
            ("\"Role\":0", "\"Role\":2"),
            ("\"Role\":0", "\"Role\":0,\"Role\":0"),
            ("\"Xᵤ\":", "\"Xᵤ\":\""),
        ] {
            let bad = good.replacen(from, to, 1);
            assert_ne!(bad, good, "{from}");
            assert!(
                Pake::answer(Curve::P256, b"pw", [9; 32], bad.as_bytes()).is_err(),
                "{to}"
            );
        }
        let huge = format!("{{\"Role\":0,\"Xᵤ\":{},\"Xᵥ\":1}}", "9".repeat(79));
        assert!(Pake::answer(Curve::P256, b"pw", [9; 32], huge.as_bytes()).is_err());
        let null = good.replacen("\"Xᵤ\":", "\"Xᵤ\":null,\"z\":", 1);
        assert_eq!(
            Pake::answer(Curve::P256, b"pw", [9; 32], null.as_bytes()).unwrap_err(),
            PakeError::Malformed
        );
        assert!(
            Pake::answer(Curve::P256, b"pw", [9; 32], &vec![b' '; MAX_PAKE_BYTES + 1]).is_err()
        );
    }

    #[test]
    fn a_message_says_its_curve() {
        let siec = Pake::start(Curve::Siec, b"pw", [3; 32]).unwrap();
        let p256 = Pake::start(Curve::P256, b"pw", [3; 32]).unwrap();
        assert_eq!(Curve::of_message(&siec.message()), Some(Curve::Siec));
        assert_eq!(Curve::of_message(&p256.message()), Some(Curve::P256));
        assert_eq!(Curve::of_message(b"{\"Role\":0,\"Uu\":1}"), None);
        assert_eq!(Curve::from_name(b"p256"), Some(Curve::P256));
        assert_eq!(Curve::from_name(b"p384"), None);
    }

    #[test]
    fn decimals_round_trip() {
        for d in [
            "0",
            "1",
            "255",
            "256",
            "65535",
            "793136080485469241208656611513609866400481671853",
        ] {
            let b = decimal_to_bytes(d).unwrap();
            assert_eq!(bytes_to_decimal(&b), d);
        }
        let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(decimal_to_bytes(max), Some([0xff; 32]));
        let over = "115792089237316195423570985008687907853269984665640564039457584007913129639936";
        assert_eq!(decimal_to_bytes(over), None);
        assert_eq!(shortest(&[0, 0, 1, 0]), &[1, 0]);
        assert!(shortest(&[0, 0]).is_empty());
    }
}
