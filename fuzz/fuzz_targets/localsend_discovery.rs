//! LocalSend's other peer JSON: the multicast announcement anyone on the
//! LAN can send, the `POST /register` anyone can make, the answers to our
//! own `register` and `info` requests, and a receiver's answer to our
//! `prepare-upload` -- through upstream's DTOs and the checks discovery and
//! sending apply: `sukkula_engine::localsend::from_peer` (spec §7 "LocalSend
//! DTOs").
//!
//! The first byte picks the DTO, the rest is the body. Asserted, against
//! the JSON read independently through `serde_json::Value`:
//!
//! - an announcement is answered only if it says HTTPS (F-LS2), names a
//!   port, and carries a 64-hex-digit fingerprint that is not ours; the
//!   answer is pinned to exactly that fingerprint (F-LS3);
//! - a registering peer is listed only if it says HTTPS, gave a port, and
//!   claimed the fingerprint its certificate proved;
//! - every listed peer is S2-clean (`sukkula_fuzz::assert_peer`) and known
//!   by the fingerprint its certificate proved, never one it claimed;
//! - an upload goes only to `/api/localsend/v2/upload` with exactly the
//!   session, file and token query parameters, each of them short and
//!   plain, and exactly the values the receiver gave: no header, path or
//!   parameter smuggled in through a token.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use sukkula_core::Protocol;
use sukkula_engine::localsend::{FromPeer, PeerDto, from_peer};
use sukkula_fuzz::assert_peer;

/// Our fingerprint, and the one the peer's certificate proved. The
/// dictionary has both, so the comparisons behind them are reached.
const OWN: &str = "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF";
const PROVEN: &str = "FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210";

/// The file ids a send offered.
const OFFERED: [&str; 3] = ["f1", "f2", "photo-3"];

const KINDS: [PeerDto; 5] = [
    PeerDto::Announcement,
    PeerDto::Register,
    PeerDto::RegisterResponse,
    PeerDto::InfoResponse,
    PeerDto::PrepareUploadResponse,
];

/// A token as a query value may carry one: short, and only characters
/// that need no escaping to mean themselves.
fn plain_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
}

/// `application/x-www-form-urlencoded` value decoding, for the checks.
fn form_decode(s: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hi = char::from(bytes.next()?).to_digit(16)?;
                let lo = char::from(bytes.next()?).to_digit(16)?;
                out.push(u8::try_from(hi * 16 + lo).ok()?);
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}

fn fingerprint_of(v: &Value) -> Option<String> {
    let f = v["fingerprint"].as_str()?.trim();
    (f.len() == 64 && f.bytes().all(|b| b.is_ascii_hexdigit())).then(|| f.to_ascii_uppercase())
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else {
        return;
    };
    let kind = KINDS[usize::from(pick) % KINDS.len()];
    let Some(out) = from_peer(kind, body, OWN, PROVEN, &OFFERED) else {
        return;
    };
    // `Value` refuses numbers that overflow an f64, which the DTOs skip
    // over in fields they ignore; only what `Value` can hold is compared.
    let v = serde_json::from_slice::<Value>(body).ok();
    match out {
        FromPeer::Answer { port, fingerprint } => {
            assert_eq!(kind, PeerDto::Announcement);
            assert_ne!(port, 0);
            assert_eq!(fingerprint.len(), 64);
            assert!(
                fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)),
                "fingerprint {fingerprint:?}"
            );
            assert_ne!(fingerprint, OWN, "we answered our own announcement");
            if let Some(v) = v {
                assert_eq!(
                    v["protocol"], "https",
                    "a plain-HTTP announcer answered (F-LS2)"
                );
                assert_eq!(v["port"].as_u64(), Some(u64::from(port)));
                assert_eq!(fingerprint_of(&v).as_deref(), Some(fingerprint.as_str()));
            }
        }
        FromPeer::Peer { peer, port } => {
            assert_peer(&peer);
            assert_eq!(peer.protocol, Protocol::LocalSend);
            assert_eq!(peer.id, format!("ls:{PROVEN}"), "a peer known by a claim");
            match kind {
                PeerDto::Register => {
                    let port = port.expect("a registering peer's port");
                    assert_ne!(port, 0);
                    if let Some(v) = v {
                        assert_eq!(v["protocol"], "https", "a plain-HTTP peer listed (F-LS2)");
                        assert_eq!(v["port"].as_u64(), Some(u64::from(port)));
                        assert_eq!(
                            fingerprint_of(&v).as_deref(),
                            Some(PROVEN),
                            "listed on a fingerprint it did not prove"
                        );
                    }
                }
                PeerDto::RegisterResponse | PeerDto::InfoResponse => assert_eq!(port, None),
                other => panic!("a peer from {other:?}"),
            }
        }
        FromPeer::Uploads(paths) => {
            assert_eq!(kind, PeerDto::PrepareUploadResponse);
            assert!(paths.len() <= OFFERED.len());
            let v = v.unwrap_or(Value::Null);
            let mut offered = OFFERED.iter().filter(|id| !v["files"][**id].is_null());
            for path in &paths {
                let query = path
                    .strip_prefix("/api/localsend/v2/upload?")
                    .unwrap_or_else(|| panic!("an upload to {path:?}"));
                assert!(
                    path.bytes().all(|b| b.is_ascii_graphic() && b != b'#'),
                    "path {path:?}"
                );
                let pairs: Vec<(&str, &str)> = query
                    .split('&')
                    .map(|p| p.split_once('=').unwrap_or((p, "")))
                    .collect();
                let [("sessionId", s), ("fileId", f), ("token", t)] = pairs[..] else {
                    panic!("query {query:?}");
                };
                let (s, f, t) = (
                    form_decode(s).expect("decodes"),
                    form_decode(f).expect("decodes"),
                    form_decode(t).expect("decodes"),
                );
                assert!(plain_token(&s) && plain_token(&t), "query {query:?}");
                if !v.is_null() {
                    let id = offered.next().expect("an upload for a file never offered");
                    assert_eq!(f, *id);
                    assert_eq!(v["sessionId"].as_str(), Some(s.as_str()));
                    assert_eq!(v["files"][*id].as_str(), Some(t.as_str()));
                }
            }
            if !v.is_null() {
                assert!(
                    offered.next().is_none(),
                    "a file the receiver asked for was skipped"
                );
            }
        }
    }
});
