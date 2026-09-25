//! Hostile-input sweep over every `sukkula-core` entry point that takes peer
//! data (spec §7, "Hostile input").
//!
//! A peer controls every byte of a name, an alias, a message, a size, a
//! digest and a MIME type; the settings file may have been edited by hand.
//! The contract for each entry point is the S-rule it implements, not just
//! "does not crash": a sanitiser that returns a bidi override has not
//! crashed, and is still a bug. So each case is checked against the
//! independent oracle in `common/oracle.rs` as well.
//!
//! Like clove's `crates/clove-core/tests/hostile.rs`: valid seed inputs,
//! mutated thousands of ways by a deterministic PRNG, on every `cargo test`
//! with no nightly toolchain. A failure reproduces from the surface name and
//! round in the panic message. Coverage-guided fuzzing of the same surfaces
//! is in `fuzz/` (see `fuzz/README.md`).

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::disallowed_methods,
    clippy::print_stderr
)]

#[path = "common/oracle.rs"]
mod oracle;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use sukkula_core::config::{MAX_SETTINGS_BYTES, SETTINGS_FILE, Settings};
use sukkula_core::hex;
use sukkula_core::inbox::{Inbox, STAGING_DIR};
use sukkula_core::limits::{
    MAX_ALIAS_CHARS, MAX_FILE_BYTES, MAX_FILES_PER_OFFER, MAX_MODEL_CHARS, MAX_OFFER_BYTES,
    RATE_LIMIT_ENTRIES,
};
use sukkula_core::name::sanitize;
use sukkula_core::offer::{Offer, Protocol, RawFile, RawOffer};
use sukkula_core::reach::{RateLimiter, ReachPolicy};
use sukkula_core::store::Store;
use sukkula_core::text;

/// Mutations per seed set for the pure functions. The whole file stays at a
/// few seconds in a debug build.
const ROUNDS: usize = 4_000;

/// Mutations for the surfaces that touch the file system.
const FS_ROUNDS: usize = 300;

/// xorshift64*: a failing case reproduces exactly from its round.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }
}

/// Tokens a hostile peer would splice into text: separators, dot runs, the
/// bidi and invisible families, marks for stacking, line breaks, blanks,
/// private use, noncharacters, and number edges for size fields.
const TOKENS: &[&str] = &[
    "/",
    "\\",
    "..",
    ".",
    " ",
    "\0",
    "\n",
    "\r\n",
    "\t",
    "\u{85}",
    "\u{2028}",
    "\u{2029}",
    "\u{202E}",
    "\u{202D}",
    "\u{2066}",
    "\u{2069}",
    "\u{200E}",
    "\u{061C}",
    "\u{200B}",
    "\u{200D}",
    "\u{2060}",
    "\u{FEFF}",
    "\u{AD}",
    "\u{34F}",
    "\u{180E}",
    "\u{3164}",
    "\u{115F}",
    "\u{FFA0}",
    "\u{2800}",
    "\u{FE0F}",
    "\u{E0041}",
    "\u{E0100}",
    "\u{1D173}",
    "\u{13430}",
    "\u{FFF9}",
    "\u{301}\u{301}\u{301}\u{301}\u{301}",
    "\u{E49}\u{E49}\u{E49}\u{E49}",
    "\u{596}\u{596}\u{596}\u{596}",
    "\u{20DD}\u{20DD}",
    "\u{E000}",
    "\u{FFFF}",
    "\u{10FFFF}",
    "\u{1B}[2J",
    "\u{9B}",
    "…",
    "é",
    ".partial",
    "received-file",
    ".jpg",
    ".tar.gz",
    "-1",
    "0",
    "8589934592",
    "8589934593",
    "17179869184",
    "18446744073709551615",
    "-9223372036854775808",
    "170141183460469231731687303715884105727",
];

/// Damage `input`: flip a bit, set an interesting byte, truncate, splice in
/// a token, cut a run, duplicate a run.
fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    const INTERESTING: [u8; 12] = [
        0x00, 0x01, 0x7F, 0x80, 0xC0, 0xE2, 0xFF, b'.', b'/', b'"', b'\\', b'-',
    ];
    let mut out = input.to_vec();
    match rng.below(7) {
        0 if !out.is_empty() => {
            let at = rng.below(out.len());
            out[at] ^= 1 << rng.below(8);
        }
        1 if !out.is_empty() => {
            let at = rng.below(out.len());
            out[at] = INTERESTING[rng.below(INTERESTING.len())];
        }
        2 if !out.is_empty() => {
            let keep = rng.below(out.len());
            out.truncate(keep);
        }
        3 | 4 => {
            let at = rng.below(out.len() + 1);
            let token = TOKENS[rng.below(TOKENS.len())].as_bytes();
            let flood = rng.below(8) == 0;
            let n = 1 + rng.below(if flood { 64 } else { 3 });
            let spliced: Vec<u8> = token.repeat(n);
            out.splice(at..at, spliced);
        }
        5 if out.len() > 2 => {
            let at = rng.below(out.len() - 1);
            let len = 1 + rng.below(out.len() - at - 1);
            out.drain(at..at + len);
        }
        _ if !out.is_empty() => {
            let at = rng.below(out.len());
            let len = 1 + rng.below(out.len() - at);
            let run = out[at..at + len].to_vec();
            out.extend_from_slice(&run);
        }
        _ => {}
    }
    out
}

/// Feed `check` the seeds, then `rounds` compounded mutations of them.
fn sweep(name: &str, seeds: &[Vec<u8>], seed: u64, rounds: usize, mut check: impl FnMut(&[u8])) {
    assert!(!seeds.is_empty(), "{name}: no seeds");
    let mut rng = Rng(seed);
    for (i, s) in seeds.iter().enumerate() {
        let mut guard = Reporter(name, "seed", i, s, true);
        check(s);
        guard.4 = false;
    }
    for round in 0..rounds {
        let base = &seeds[rng.below(seeds.len())];
        let mut case = mutate(&mut rng, base);
        for _ in 0..rng.below(4) {
            case = mutate(&mut rng, &case);
        }
        // Speaks only if `check` panics, which unwinds through here.
        let mut guard = Reporter(name, "round", round, &case, true);
        check(&case);
        guard.4 = false;
    }
}

/// Names the failing surface, case and input on the way out of a panic;
/// disarmed (the last field) once the case has passed.
struct Reporter<'a>(&'a str, &'a str, usize, &'a [u8], bool);

impl Drop for Reporter<'_> {
    fn drop(&mut self) {
        let Reporter(name, what, n, input, armed) = self;
        if *armed {
            eprintln!("hostile sweep failed: {name} {what} {n}, input {input:02x?}");
        }
    }
}

fn seeds(list: &[&str]) -> Vec<Vec<u8>> {
    list.iter().map(|s| s.as_bytes().to_vec()).collect()
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn names() {
    let s = seeds(&[
        "holiday.jpg",
        "../../.bashrc",
        "C:\\Windows\\evil.dll",
        "photo\u{202E}gpj.exe",
        ".hidden",
        "name. . .",
        "Kesä 2026 (1).tar.gz",
        &format!("{}.jpeg", "a".repeat(400)),
        &format!("{}.pdf", "x".repeat(797)),
        "\u{301}.\u{301}x",
        "..",
        "",
    ]);
    sweep("names", &s, 0x5EED_0001, ROUNDS, |b| {
        let raw = lossy(b);
        let out = sanitize(&raw);
        assert_eq!(
            oracle::name_violation(out.as_str()),
            None,
            "{raw:?} -> {out:?}"
        );
        assert_eq!(sanitize(out.as_str()), out, "not idempotent: {raw:?}");
        let n = out.numbered(1 + (b.len() as u32 % 999));
        assert_eq!(oracle::name_violation(n.as_str()), None, "{n:?}");
        assert_eq!(sanitize(n.as_str()), n);
    });
}

#[test]
fn display_text() {
    let s = seeds(&[
        "Alice's Jolla",
        "Pixel 9 Pro",
        "a\u{202E}b\u{2066}c",
        "\u{E2A}\u{E49}\u{E49}\u{E49}",
        "  spaced  out\talias\n",
        "Unknown device",
        &"x".repeat(100),
    ]);
    sweep("display", &s, 0x5EED_0002, ROUNDS, |b| {
        let raw = lossy(b);
        for max in [0, 1, 2, MAX_ALIAS_CHARS, MAX_MODEL_CHARS, 200] {
            let out = text::display(&raw, max);
            assert_eq!(
                oracle::display_violation(&out, max),
                None,
                "{raw:?} -> {out:?}"
            );
            assert_eq!(text::display(&out, max), out, "not idempotent");
        }
    });
}

#[test]
fn messages() {
    let s = seeds(&[
        "hello\nworld",
        "https://example.org/\u{202E}moc.live",
        "a\r\n\r\n\r\n\r\n\r\nb",
        "\u{591}\u{591}\u{591}\u{591}\u{591}",
        "line\u{2028}sep\u{2029}para",
    ]);
    sweep("message", &s, 0x5EED_0003, ROUNDS, |b| {
        let raw = lossy(b);
        let out = text::message(&raw);
        assert_eq!(oracle::message_violation(&out), None, "{raw:?} -> {out:?}");
        assert_eq!(text::message(&out), out, "not idempotent");
    });
}

/// A tiny wire format for offers, so that byte mutations reach every field:
/// fields separated by 0x1F, files by 0x1E. `sender 1F model 1F text 1F pin
/// 1E name 1F size 1F mime 1F sha 1E ...`. A size that is not a decimal
/// number is read as the little-endian bytes of an `i128`.
fn decode_offer(b: &[u8]) -> RawOffer {
    let mut records = b.split(|&c| c == 0x1E);
    let head: Vec<&[u8]> = records
        .next()
        .unwrap_or(&[])
        .split(|&c| c == 0x1F)
        .collect();
    let field = |v: &[&[u8]], i: usize| v.get(i).map(|f| lossy(f));
    let protocol = Protocol::ALL[b.len() % Protocol::ALL.len()];
    let mut raw = RawOffer::new(protocol, field(&head, 0).unwrap_or_default());
    raw.model = field(&head, 1);
    raw.text = field(&head, 2).filter(|t| !t.is_empty());
    raw.pin = field(&head, 3);
    for rec in records.take(MAX_FILES_PER_OFFER + 5) {
        let f: Vec<&[u8]> = rec.split(|&c| c == 0x1F).collect();
        let size_field = f.get(1).copied().unwrap_or(&[]);
        let size = std::str::from_utf8(size_field)
            .ok()
            .and_then(|s| s.parse::<i128>().ok())
            .unwrap_or_else(|| {
                let mut le = [0u8; 16];
                for (d, s) in le.iter_mut().zip(size_field) {
                    *d = *s;
                }
                i128::from_le_bytes(le)
            });
        raw.files.push(RawFile {
            name: field(&f, 0).unwrap_or_default(),
            size,
            mime: field(&f, 2),
            sha256: field(&f, 3),
        });
    }
    raw
}

#[test]
fn offers() {
    let sha = hex::encode(&Sha256::digest(b"x"));
    let s = seeds(&[
        &format!("Alice\x1fJolla C2\x1f\x1f1234\x1ea.jpg\x1f10\x1fimage/jpeg\x1f{sha}"),
        "Bob\x1f\x1fhello there\x1f",
        "\u{202E}Mallory\x1f\x1f\x1f\x1e../../x\x1f-1\x1f\x1f\x1eb\x1f8589934592\x1f\x1f",
        "p\x1f\x1f\x1f\x1ea\x1f8589934592\x1f\x1f\x1eb\x1f8589934592\x1f\x1f\x1ec\x1f1\x1f\x1f",
        "p\x1f\x1f\x1f\x1ea\x1f18446744073709551615\x1ftext/plain; charset=utf-8\x1fzz",
        &format!("p{}", "\x1ef\x1f1".repeat(MAX_FILES_PER_OFFER + 1)),
    ]);
    sweep("offers", &s, 0x5EED_0004, ROUNDS, |b| {
        let raw = decode_offer(b);
        let sizes: Vec<i128> = raw.files.iter().map(|f| f.size).collect();
        let Ok(o) = Offer::validate(raw) else {
            return;
        };
        assert!(o.files.len() <= MAX_FILES_PER_OFFER);
        let mut total = 0u64;
        for (f, raw) in o.files.iter().zip(&sizes) {
            assert!(*raw >= 0 && f.size <= MAX_FILE_BYTES);
            assert_eq!(i128::from(f.size), *raw);
            total = total.checked_add(f.size).unwrap();
            assert_eq!(oracle::name_violation(f.name.as_str()), None);
        }
        assert_eq!(total, o.total_bytes);
        assert!(total <= MAX_OFFER_BYTES);
        assert_eq!(oracle::display_violation(&o.sender, MAX_ALIAS_CHARS), None);
        if let Some(m) = &o.model {
            assert_eq!(oracle::display_violation(m, MAX_MODEL_CHARS), None);
        }
        if let Some(t) = &o.text {
            assert_eq!(oracle::message_violation(t), None);
        }
        if let Some(p) = &o.pin {
            assert!(p.bytes().all(|c| c.is_ascii_digit()));
        }
    });
}

#[test]
fn digests() {
    let s = seeds(&[&"ab".repeat(32), &"AB".repeat(32), &"0".repeat(64)]);
    sweep("hex", &s, 0x5EED_0005, ROUNDS, |b| {
        let raw = lossy(b);
        match hex::decode_32(&raw) {
            Some(d) => {
                assert_eq!(raw.len(), 64);
                assert_eq!(hex::encode(&d), raw.to_ascii_lowercase());
            }
            None => assert!(raw.len() != 64 || !raw.bytes().all(|c| c.is_ascii_hexdigit())),
        }
    });
}

#[test]
fn settings_json() {
    let s = seeds(&[
        "{}",
        r#"{"device_name":"Pekka","logging":false}"#,
        r#"{"localsend":{"enabled":true,"pin":"1234"},"quickshare":{"enabled":false,"visibility":"hidden","ble_nudge":false}}"#,
        r#"{"wormhole":{"mailbox_url":"wss://relay.example/v1","relay_url":"tcp://relay.example:4001"},"bluetooth":{"enabled":true}}"#,
        r#"{"device_name":"\u202eevil\u200b","auto_accept":true}"#,
    ]);
    sweep("settings", &s, 0x5EED_0006, ROUNDS, |b| {
        let Ok(parsed) = serde_json::from_slice::<Settings>(b) else {
            return;
        };
        let Ok(v) = parsed.validate() else {
            return;
        };
        assert_eq!(
            oracle::display_violation(&v.device_name, MAX_ALIAS_CHARS),
            None
        );
        assert_eq!(v.clone().validate().unwrap(), v, "not idempotent");
        let back: Settings = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back, v);
    });
}

#[test]
fn addresses_and_rate_limits() {
    let s: Vec<Vec<u8>> = vec![
        vec![192, 168, 1, 2],
        vec![10, 0, 0, 1],
        vec![172, 16, 0, 1],
        vec![169, 254, 1, 1],
        vec![100, 64, 0, 1],
        vec![127, 0, 0, 1],
        "fe80::1".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
        "fd00::1".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
        "::ffff:192.168.1.1"
            .parse::<Ipv6Addr>()
            .unwrap()
            .octets()
            .to_vec(),
        "::1".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
    ];
    let strict = ReachPolicy::default();
    let mut limiter = RateLimiter::new(3, Duration::from_secs(10));
    let t0 = Instant::now();
    let mut tick = 0u64;
    sweep("reach", &s, 0x5EED_0007, ROUNDS * 4, |b| {
        let ip = match b.len() {
            4 => IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3])),
            n if n >= 16 => {
                let mut o = [0u8; 16];
                o.copy_from_slice(&b[..16]);
                IpAddr::V6(Ipv6Addr::from(o))
            }
            _ => return,
        };
        let permitted = strict.permits(ip);
        let (v4, v6) = match ip {
            IpAddr::V4(v4) => (Some(v4), None),
            IpAddr::V6(v6) => (v6.to_ipv4_mapped(), Some(v6)),
        };
        match (v4, v6) {
            (Some(v4), _) => assert_eq!(permitted, v4.is_private() || v4.is_link_local(), "{ip}"),
            (None, v6) => {
                let first = v6.map_or(0, |v6| u128::from(v6) >> 112);
                assert_eq!(
                    permitted,
                    (0xFC00..=0xFDFF).contains(&first) || (0xFE80..=0xFEBF).contains(&first),
                    "{ip}"
                );
            }
        }
        tick += 1;
        limiter.allow(ip, t0 + Duration::from_millis(tick));
        assert!(limiter.len() <= RATE_LIMIT_ENTRIES);
    });
}

/// Chunked writes against a declared size, as a peer would drive them:
/// the first two bytes of the case are the declared size, the rest is split
/// into chunks at every 0xFF, and the last byte picks the ending.
#[test]
fn inbox_writes() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let s: Vec<Vec<u8>> = vec![
        vec![0, 5, b'h', b'e', 0xFF, b'l', b'l', b'o', 1],
        vec![0, 0, 1],
        vec![0, 3, b'a', b'b', b'c', b'd', 1],
        vec![0, 9, b'a', 0xFF, 0xFF, b'b', 0],
        vec![1, 0, 7, 7, 7, 0xFF, 7, 2],
    ];
    let mut placed = 0usize;
    sweep("inbox", &s, 0x5EED_0008, FS_ROUNDS, |b| {
        rt.block_on(async {
            let declared = u64::from(b.first().copied().unwrap_or(0)) << 8
                | u64::from(b.get(1).copied().unwrap_or(0));
            let ending = b.last().copied().unwrap_or(0) % 3;
            let body = b.get(2..b.len().saturating_sub(1)).unwrap_or(&[]);
            let chunks: Vec<&[u8]> = body.split(|&c| c == 0xFF).collect();
            let sent: Vec<u8> = chunks.concat();
            let digest: Option<[u8; 32]> = (ending == 2)
                .then(|| Sha256::digest(&sent[..sent.len().min(declared as usize)]).into());
            let mut f = inbox
                .begin(&sanitize("hostile.bin"), declared, digest)
                .await
                .unwrap();
            let mut ok = true;
            let mut written = Vec::new();
            for c in &chunks {
                if f.write(c).await.is_err() {
                    ok = false;
                    break;
                }
                written.extend_from_slice(c);
            }
            assert!(written.len() as u64 <= declared);
            let before = std::fs::read_dir(dir.path()).unwrap().count();
            let result = if ending == 0 {
                drop(f);
                None
            } else {
                Some(f.commit().await)
            };
            let after = std::fs::read_dir(dir.path()).unwrap().count();
            match result {
                Some(Ok(saved)) => {
                    assert!(ok && written.len() as u64 == declared);
                    assert_eq!(std::fs::read(&saved.path).unwrap(), written);
                    assert_eq!(after, before + 1);
                    placed += 1;
                }
                _ => assert_eq!(after, before, "a failed file left something"),
            }
            assert_eq!(
                std::fs::read_dir(dir.path().join(STAGING_DIR))
                    .unwrap()
                    .count(),
                0
            );
        });
    });
    assert!(
        placed > 0,
        "no case ever placed a file: the sweep is not reaching commit"
    );
}

/// The settings file as a hand-edited or tampered file on disk, through the
/// store's size cap and the settings parser.
#[test]
fn settings_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let s = seeds(&[
        "{}",
        r#"{"device_name":"Pekka","localsend":{"enabled":true,"pin":"1234"}}"#,
        &format!(r#"{{"device_name":"{}"}}"#, "x".repeat(MAX_SETTINGS_BYTES)),
    ]);
    sweep("settings file", &s, 0x5EED_0009, FS_ROUNDS, |b| {
        store.write(SETTINGS_FILE, b).unwrap();
        match store.read_json::<Settings>(SETTINGS_FILE, MAX_SETTINGS_BYTES) {
            Ok(Some(settings)) => {
                assert!(b.len() <= MAX_SETTINGS_BYTES);
                if let Ok(v) = settings.validate() {
                    assert_eq!(
                        oracle::display_violation(&v.device_name, MAX_ALIAS_CHARS),
                        None
                    );
                }
            }
            Ok(None) => panic!("written but not found"),
            Err(_) => {}
        }
    });
}
