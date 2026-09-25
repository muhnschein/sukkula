use std::net::{IpAddr, Ipv4Addr};
use std::sync::LazyLock;

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use hkdf::Hkdf;
use num_bigint::{BigUint, ToBigInt};
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::FromEncodedPoint;
use p256::{EncodedPoint, PublicKey, SecretKey};
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum DeviceType {
    Unknown = 0,
    Phone = 1,
    Tablet = 2,
    Laptop = 3,
}

#[allow(dead_code)]
impl DeviceType {
    pub fn from_raw_value(value: u8) -> Self {
        match value {
            0 => DeviceType::Unknown,
            1 => DeviceType::Phone,
            2 => DeviceType::Tablet,
            3 => DeviceType::Laptop,
            _ => DeviceType::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDeviceInfo {
    pub name: String,
    pub device_type: DeviceType,
}

/// Longest device name announced, in bytes. The endpoint info travels in
/// an mDNS TXT string (at most 255 bytes, base64) and in BLE adverts; a
/// longer name used to be cut mid-character (or its one-byte length to
/// wrap) and make the whole record unusable.
pub const MAX_ENDPOINT_NAME_BYTES: usize = 128;

/// `name`, cut to at most [`MAX_ENDPOINT_NAME_BYTES`] on a character
/// boundary.
fn endpoint_name(name: &str) -> &str {
    let mut end = name.len().min(MAX_ENDPOINT_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

impl RemoteDeviceInfo {
    pub fn serialize(&self) -> Vec<u8> {
        // 1 byte: Version(3 bits)|Visibility(1 bit)|Device Type(3 bits)|Reserved(1 bit)
        let mut endpoint_info: Vec<u8> = vec![((self.device_type.clone() as u8) << 1) & 0b1110];

        // 16 bytes: unknown random bytes
        endpoint_info.extend((0..16).map(|_| rand::rng().random_range(0..=255)));

        // Device name in UTF-8 prefixed with 1-byte length
        let name = endpoint_name(&self.name).as_bytes();
        endpoint_info.push(name.len() as u8);
        endpoint_info.extend_from_slice(name);

        endpoint_info
    }
}

pub fn gen_mdns_name(endpoint_id: [u8; 4]) -> String {
    let mut name_b = Vec::new();

    let pcp: [u8; 1] = [0x23];
    name_b.extend_from_slice(&pcp);

    name_b.extend_from_slice(&endpoint_id);

    let service_id: [u8; 3] = [0xFC, 0x9F, 0x5E];
    name_b.extend_from_slice(&service_id);

    let unknown_bytes: [u8; 2] = [0x00, 0x00];
    name_b.extend_from_slice(&unknown_bytes);

    URL_SAFE_NO_PAD.encode(&name_b)
}

/// The endpoint id in a Quick Share service instance name (or full name),
/// if it is one: base64url of `[0x23, id (4 bytes), 0xFC 0x9F 0x5E, 0, 0]`,
/// as [`gen_mdns_name`] makes it.
pub fn parse_mdns_name(fullname: &str) -> Option<[u8; 4]> {
    let instance = fullname.split('.').next()?;
    let bytes = URL_SAFE_NO_PAD.decode(instance).ok()?;
    match bytes.as_slice() {
        [0x23, a, b, c, d, 0xFC, 0x9F, 0x5E, _, _] => Some([*a, *b, *c, *d]),
        _ => None,
    }
}

/// The 16 identity bytes (2-byte salt + 14-byte metadata-key hash) this run
/// presents in every advertisement. Random per run.
pub static ENDPOINT_IDENTITY: LazyLock<[u8; 16]> = LazyLock::new(|| rand::rng().random());

pub fn gen_mdns_endpoint_info(device_type: u8, device_name: &str) -> String {
    let mut record = Vec::new();

    // 1 byte: Version(3 bits)|Visibility(1 bit)|Device Type(3 bits)|Reserved(1 bits)
    // Device types: unknown=0, phone=1, tablet=2, laptop=3
    record.push(device_type << 1);

    record.extend_from_slice(ENDPOINT_IDENTITY.as_slice());

    let device_name = endpoint_name(device_name).as_bytes();
    record.push(device_name.len() as u8);
    record.extend_from_slice(device_name);

    URL_SAFE_NO_PAD.encode(&record)
}

pub fn parse_mdns_endpoint_info(encoded_str: &str) -> Result<(DeviceType, String), anyhow::Error> {
    let decoded_bytes = URL_SAFE_NO_PAD.decode(encoded_str)?;
    let (Some(&flags), Some(&name_length)) = (decoded_bytes.first(), decoded_bytes.get(17)) else {
        return Err(anyhow!("Invalid data length"));
    };

    let device_type = (flags >> 1) & 0x7;
    let device_name_bytes = decoded_bytes
        .get(18..18 + usize::from(name_length))
        .ok_or_else(|| anyhow!("Invalid name length"))?;
    let device_name = String::from_utf8(device_name_bytes.to_vec())?;

    Ok((DeviceType::from_raw_value(device_type), device_name))
}

pub fn gen_ecdsa_keypair() -> (SecretKey, PublicKey) {
    let secret_key = SecretKey::random(&mut OsRng);
    let public_key = secret_key.public_key();

    (secret_key, public_key)
}

pub fn encode_point(unsigned: Bytes) -> Result<Vec<u8>, anyhow::Error> {
    let big_int = BigUint::from_bytes_be(&unsigned)
        .to_bigint()
        .ok_or_else(|| anyhow!("Failed to convert to bigint"))?;

    Ok(big_int.to_signed_bytes_be())
}

/// The peer's P-256 public key from the two coordinates of a UKEY2
/// `EcP256PublicKey`: big-endian two's-complement integers, as Java's
/// `BigInteger.toByteArray()` writes them -- so 33 bytes when the top bit is
/// set and fewer than 32 when the value has leading zero bytes.
///
/// Upstream kept only the last 32 bytes of a longer coordinate (accepting
/// any leading garbage), used a shorter one as is (so about one handshake
/// in 128 failed on a coordinate with a leading zero byte), and then
/// `unwrap()`ped `PublicKey::from_encoded_point`, which is `None` for any
/// point not on the curve: a peer panicked the receiver with one frame,
/// before anything was authenticated.
pub fn decode_p256_point(x: &[u8], y: &[u8]) -> Result<PublicKey, anyhow::Error> {
    fn coordinate(v: &[u8]) -> Result<[u8; 32], anyhow::Error> {
        let start = v.iter().position(|b| *b != 0).unwrap_or(v.len());
        let digits = &v[start..];
        if digits.len() > 32 {
            return Err(anyhow!("P-256 coordinate too long"));
        }
        let mut out = [0u8; 32];
        out[32 - digits.len()..].copy_from_slice(digits);
        Ok(out)
    }
    let point = EncodedPoint::from_affine_coordinates(
        &coordinate(x)?.into(),
        &coordinate(y)?.into(),
        false,
    );
    Option::from(PublicKey::from_encoded_point(&point))
        .ok_or_else(|| anyhow!("the peer's key is not a point on P-256"))
}

/// AES-256-CBC with PKCS#7 padding, as the D2D channel uses.
pub fn aes_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, anyhow::Error> {
    let enc = cbc::Encryptor::<aes::Aes256>::new_from_slices(key, iv)
        .map_err(|_| anyhow!("bad AES key or IV length"))?;
    Ok(enc.encrypt_padded_vec_mut::<Pkcs7>(data))
}

/// Decrypts AES-256-CBC, checking the IV length and the padding. libaes,
/// used before, indexed the IV without checking its length (a peer-chosen
/// IV of under 16 bytes panicked the connection), accepted any padding,
/// and is a table-based AES whose timing depends on the key.
pub fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, anyhow::Error> {
    let dec = cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv)
        .map_err(|_| anyhow!("bad AES key or IV length"))?;
    dec.decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| anyhow!("bad padding"))
}

pub fn hkdf_extract_expand(
    salt: &[u8],
    input: &[u8],
    info: &[u8],
    output_len: usize,
) -> Result<Vec<u8>, anyhow::Error> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), input);
    let mut okm = vec![0u8; output_len];
    hkdf.expand(info, &mut okm)
        .map_err(|e| anyhow!("HKDF expand failed: {}", e))?;
    Ok(okm)
}

pub fn to_four_digit_string(bytes: &Vec<u8>) -> String {
    let k_hash_modulo = 9973;
    let k_hash_base_multiplier = 31;

    let mut hash = 0;
    let mut multiplier = 1;
    for &byte in bytes {
        let byte = byte as i8 as i32;
        hash = (hash + byte * multiplier) % k_hash_modulo;
        multiplier = (multiplier * k_hash_base_multiplier) % k_hash_modulo;
    }

    format!("{:04}", hash.abs())
}

pub fn gen_random(size: usize) -> Vec<u8> {
    let mut data = vec![0; size];
    rand::rng().fill_bytes(&mut data);

    data
}

pub fn is_not_self_ip(ip_address: &Ipv4Addr) -> bool {
    if let Ok(if_addrs) = if_addrs::get_if_addrs() {
        for if_addr in if_addrs {
            if if_addr.ip() == IpAddr::V4(*ip_address) {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p256_coordinates_in_every_encoding() {
        let (_, public) = gen_ecdsa_keypair();
        let point = p256::elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(&public, false);
        let x = point.x().unwrap().to_vec();
        let y = point.y().unwrap().to_vec();
        assert_eq!(decode_p256_point(&x, &y).unwrap(), public);
        // Java-style: a sign byte in front, or leading zeros dropped.
        let mut x33 = vec![0u8];
        x33.extend_from_slice(&x);
        assert_eq!(decode_p256_point(&x33, &y).unwrap(), public);
        let x_short: Vec<u8> = x.iter().copied().skip_while(|b| *b == 0).collect();
        assert_eq!(decode_p256_point(&x_short, &y).unwrap(), public);
        // Not canonical, not on the curve, empty: errors, never panics.
        let mut x_garbage = vec![1u8];
        x_garbage.extend_from_slice(&x);
        assert!(decode_p256_point(&x_garbage, &y).is_err());
        assert!(decode_p256_point(&[1], &[1]).is_err());
        assert!(decode_p256_point(&[], &[]).is_err());
        assert!(decode_p256_point(&[0xff; 64], &y).is_err());
    }

    #[test]
    fn aes_cbc_round_trips_and_checks_its_inputs() {
        let key = [7u8; 32];
        let iv = [9u8; 16];
        let ct = aes_cbc_encrypt(&key, &iv, b"hello").unwrap();
        assert_eq!(aes_cbc_decrypt(&key, &iv, &ct).unwrap(), b"hello");
        assert!(aes_cbc_decrypt(&key, &[], &ct).is_err(), "short IV");
        assert!(aes_cbc_decrypt(&key, &iv[..15], &ct).is_err());
        assert!(aes_cbc_decrypt(&key[..16], &iv, &ct).is_err(), "short key");
        assert!(
            aes_cbc_decrypt(&key, &iv, &ct[..15]).is_err(),
            "not whole blocks"
        );
        assert!(
            aes_cbc_decrypt(&[8u8; 32], &iv, &ct).is_err(),
            "wrong key: bad padding"
        );
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary() {
        let name = "ä".repeat(200);
        let info = gen_mdns_endpoint_info(DeviceType::Phone as u8, &name);
        let (_, parsed) = parse_mdns_endpoint_info(&info).unwrap();
        assert!(parsed.len() <= MAX_ENDPOINT_NAME_BYTES);
        assert!(name.starts_with(&parsed));
        let rdi = RemoteDeviceInfo {
            name,
            device_type: DeviceType::Phone,
        };
        let bytes = rdi.serialize();
        assert_eq!(usize::from(bytes[17]) + 18, bytes.len());
        assert!(std::str::from_utf8(&bytes[18..]).is_ok());
    }

    #[test]
    fn hostile_endpoint_info_is_an_error() {
        for s in ["", "AA", "____", &URL_SAFE_NO_PAD.encode([0u8; 17])] {
            assert!(parse_mdns_endpoint_info(s).is_err(), "{s}");
        }
        let mut record = vec![0u8; 18];
        record[17] = 200;
        assert!(parse_mdns_endpoint_info(&URL_SAFE_NO_PAD.encode(&record)).is_err());
        record.truncate(17);
        record.push(2);
        record.extend_from_slice(&[0xff, 0xfe]);
        assert!(parse_mdns_endpoint_info(&URL_SAFE_NO_PAD.encode(&record)).is_err());
    }

    #[test]
    fn mdns_names_carry_the_endpoint_id() {
        let name = gen_mdns_name(*b"Ab3z");
        assert_eq!(parse_mdns_name(&name), Some(*b"Ab3z"));
        let full = format!("{name}._FC9F5ED42C8A._tcp.local.");
        assert_eq!(parse_mdns_name(&full), Some(*b"Ab3z"));
        // Wrong service id bytes, wrong length, not base64, not a name.
        let wrong_service = URL_SAFE_NO_PAD.encode([0x23, 1, 2, 3, 4, 0, 0, 0, 0, 0]);
        let too_short = URL_SAFE_NO_PAD.encode([0x23, 1, 2, 3, 4]);
        for bad in [
            "",
            ".",
            "printer._ipp._tcp.local.",
            "not base64!",
            &wrong_service,
            &too_short,
        ] {
            assert!(parse_mdns_name(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn test_gen_and_parse_mdns_info() {
        let device_name = "a_device_name";
        let device_type = DeviceType::Laptop;

        dbg!(&device_type);
        dbg!(device_type.clone() as u8);

        let info = gen_mdns_endpoint_info(device_type.clone() as u8, device_name);
        let parse_info = parse_mdns_endpoint_info(&info).unwrap();

        assert_eq!(parse_info.1, device_name);
        assert_eq!(parse_info.0, device_type);
    }
}
