//! The transfer protocol v1 messages (the "text-or-file-xfer" app), as the
//! Python reference client and magic-wormhole's `transfer/v1.rs` exchange
//! them over the encrypted mailbox:
//!
//! ```text
//! {"transit": {"abilities-v1": [...], "hints-v1": [...]}}
//! {"offer": {"message": "..."}}
//! {"offer": {"file": {"filename": "...", "filesize": N}}}
//! {"offer": {"directory": {"dirname": "...", "mode": "zipfile/deflated",
//!            "zipsize": N, "numbytes": N, "numfiles": N}}}
//! {"answer": {"message_ack": "ok"}} / {"answer": {"file_ack": "ok"}}
//! {"error": "..."}
//! ```
//!
//! and, as the last transit record, `{"ack": "ok", "sha256": "<hex>"}`.
//!
//! Everything here parses peer bytes, so nothing here trusts them: the
//! message is at most [`super::MAX_PEER_MESSAGE_BYTES`] before it gets here,
//! sizes stay signed 128-bit so a negative or oversized one survives to
//! [`sukkula_core::offer::Offer::validate`], lists are scanned only to a
//! bound, and a hint that does not parse is dropped rather than failing the
//! transfer.

use std::net::IpAddr;

use magic_wormhole::transit::{Abilities, Hints, RelayHint};
use serde_json::{Map, Value, json};
use sukkula_core::Protocol;
use sukkula_core::offer::{RawFile, RawOffer};

use super::session::{Endpoint, protocol};
use super::{MAX_DIRECT_HINTS, MAX_RELAY_HINTS, PEER_LABEL};
use crate::api::ErrorInfo;

/// Hints scanned in one transit message; the rest are ignored.
const MAX_HINTS_SCANNED: usize = 64;

/// Abilities scanned in one transit message.
const MAX_ABILITIES_SCANNED: usize = 16;

/// Endpoints kept per relay hint, as the library does.
const MAX_RELAY_ENDPOINTS: usize = 3;

/// Longest host name kept from a hint (RFC 1035's limit).
const MAX_HOST_BYTES: usize = 253;

/// A peer message, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PeerMsg {
    /// Its transit abilities and hints.
    Transit(TheirTransit),
    /// Something it offers.
    Offer(OfferMsg),
    /// Its answer to our offer.
    Answer(Answer),
    /// It gave up, or declined. The reason is its text and is dropped.
    Error,
    /// A message this version does not know; ignored, but counted.
    Other,
}

/// What the peer offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OfferMsg {
    /// A text message.
    Text(String),
    /// One file, or a folder the sender packed into one file (F-MW3): the
    /// Python client zips folders (`directory` offers), magic-wormhole's own
    /// `send_folder` tars them into a plain `file` offer.
    File {
        /// The name as the peer gave it.
        name: String,
        /// The size as the peer gave it.
        size: i128,
    },
    /// An offer kind this adapter does not take.
    Unsupported,
}

/// The peer's answer to our offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// `message_ack`, and whether it said `ok`.
    Message(bool),
    /// `file_ack`, and whether it said `ok`.
    File(bool),
    /// Something else.
    Other,
}

/// The peer's transit message, reduced to what we use and bounded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TheirTransit {
    /// It announced `direct-tcp-v1`.
    pub direct: bool,
    /// It announced `relay-v1`.
    pub relay: bool,
    /// Its direct hints: IP literals only, as the library requires, at most
    /// [`MAX_DIRECT_HINTS`], deduplicated.
    pub direct_hints: Vec<(IpAddr, u16)>,
    /// Its relays' TCP endpoints: at most [`MAX_RELAY_HINTS`] relays with
    /// at most three endpoints each.
    pub relays: Vec<Vec<Endpoint>>,
}

/// Parses one decrypted peer message.
///
/// # Errors
///
/// Not JSON, not an object with exactly one key, or a known message whose
/// shape is wrong.
pub(crate) fn parse(bytes: &[u8]) -> Result<PeerMsg, ErrorInfo> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| protocol("the peer sent malformed JSON"))?;
    let (key, body) = single_entry(&value).ok_or_else(|| malformed("message"))?;
    match key {
        "transit" => parse_transit(body).map(PeerMsg::Transit),
        "offer" => parse_offer(body).map(PeerMsg::Offer),
        "answer" => Ok(PeerMsg::Answer(parse_answer(body))),
        "error" => Ok(PeerMsg::Error),
        _ => Ok(PeerMsg::Other),
    }
}

fn malformed(what: &'static str) -> ErrorInfo {
    protocol(match what {
        "offer" => "the peer sent a malformed offer",
        "transit" => "the peer sent malformed transit hints",
        _ => "the peer sent a malformed message",
    })
}

/// The one key and value of an object that has exactly one.
fn single_entry(v: &Value) -> Option<(&str, &Value)> {
    let map: &Map<String, Value> = v.as_object()?;
    if map.len() != 1 {
        return None;
    }
    map.iter().next().map(|(k, v)| (k.as_str(), v))
}

fn parse_offer(v: &Value) -> Result<OfferMsg, ErrorInfo> {
    let (kind, body) = single_entry(v).ok_or_else(|| malformed("offer"))?;
    let text = |field: &str| body.get(field).and_then(Value::as_str);
    match kind {
        "message" => body
            .as_str()
            .map(|t| OfferMsg::Text(t.to_owned()))
            .ok_or_else(|| malformed("offer")),
        "file" => {
            let name = text("filename").ok_or_else(|| malformed("offer"))?;
            let size = body
                .get("filesize")
                .and_then(declared_size)
                .ok_or_else(|| malformed("offer"))?;
            Ok(OfferMsg::File {
                name: name.to_owned(),
                size,
            })
        }
        "directory" => {
            // Saved as the zip it is, unopened (F-MW3); the name says so.
            let name = text("dirname").ok_or_else(|| malformed("offer"))?;
            let size = body
                .get("zipsize")
                .and_then(declared_size)
                .ok_or_else(|| malformed("offer"))?;
            Ok(OfferMsg::File {
                name: format!("{name}.zip"),
                size,
            })
        }
        _ => Ok(OfferMsg::Unsupported),
    }
}

/// A size as the peer declared it, widened without loss. Integers are
/// exact; a float that is integral and beyond any 64-bit integer saturates
/// in its own direction, so it is refused for its size, not for its
/// spelling. Any other float (`5.5`, or `5.0`, which no sender writes) is
/// malformed.
pub(crate) fn declared_size(v: &Value) -> Option<i128> {
    if let Some(u) = v.as_u64() {
        return Some(i128::from(u));
    }
    if let Some(i) = v.as_i64() {
        return Some(i128::from(i));
    }
    let f = v.as_f64()?;
    // 2^64: every integral float with a smaller magnitude would have
    // parsed as an integer above if the peer had meant one.
    const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;
    if f.is_finite() && f.fract() == 0.0 && f.abs() >= TWO_POW_64 {
        Some(if f > 0.0 { i128::MAX } else { i128::MIN })
    } else {
        None
    }
}

fn parse_answer(v: &Value) -> Answer {
    match single_entry(v) {
        Some(("message_ack", ok)) => Answer::Message(ok.as_str() == Some("ok")),
        Some(("file_ack", ok)) => Answer::File(ok.as_str() == Some("ok")),
        _ => Answer::Other,
    }
}

fn parse_transit(v: &Value) -> Result<TheirTransit, ErrorInfo> {
    let obj = v.as_object().ok_or_else(|| malformed("transit"))?;
    let mut out = TheirTransit::default();
    if let Some(list) = obj.get("abilities-v1").and_then(Value::as_array) {
        for a in list.iter().take(MAX_ABILITIES_SCANNED) {
            match a.get("type").and_then(Value::as_str) {
                Some("direct-tcp-v1") => out.direct = true,
                Some("relay-v1") => out.relay = true,
                _ => {}
            }
        }
    }
    if let Some(list) = obj.get("hints-v1").and_then(Value::as_array) {
        for h in list.iter().take(MAX_HINTS_SCANNED) {
            match h.get("type").and_then(Value::as_str) {
                Some("direct-tcp-v1") => {
                    if out.direct_hints.len() < MAX_DIRECT_HINTS
                        && let Some((host, port)) = tcp_hint(h)
                        && let Ok(ip) = host.parse::<IpAddr>()
                        && !out.direct_hints.contains(&(ip, port))
                    {
                        out.direct_hints.push((ip, port));
                    }
                }
                Some("relay-v1") if out.relays.len() < MAX_RELAY_HINTS => {
                    let endpoints = relay_endpoints(h);
                    if !endpoints.is_empty() {
                        out.relays.push(endpoints);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

fn relay_endpoints(h: &Value) -> Vec<Endpoint> {
    let mut out: Vec<Endpoint> = Vec::new();
    let Some(list) = h.get("hints").and_then(Value::as_array) else {
        return out;
    };
    for e in list.iter().take(MAX_HINTS_SCANNED) {
        if out.len() >= MAX_RELAY_ENDPOINTS {
            break;
        }
        if e.get("type").and_then(Value::as_str) != Some("direct-tcp-v1") {
            continue;
        }
        if let Some((host, port)) = tcp_hint(e)
            && is_host(host)
        {
            let ep = Endpoint {
                host: host.to_owned(),
                port,
            };
            if !out.contains(&ep) {
                out.push(ep);
            }
        }
    }
    out
}

/// `hostname` and a non-zero `port` from a `direct-tcp-v1` hint.
fn tcp_hint(h: &Value) -> Option<(&str, u16)> {
    let host = h.get("hostname").and_then(Value::as_str)?;
    let port = h.get("port").and_then(Value::as_u64)?;
    let port = u16::try_from(port).ok().filter(|p| *p != 0)?;
    Some((host, port))
}

/// A DNS name or an IP literal, nothing else: no spaces, no path, no
/// brackets, no scheme.
fn is_host(h: &str) -> bool {
    if h.parse::<IpAddr>().is_ok() {
        return true;
    }
    !h.is_empty()
        && h.len() <= MAX_HOST_BYTES
        && h.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

// ------------------------------------------------------------- building

fn to_bytes(v: &Value) -> Vec<u8> {
    // A `Value` always serialises; the empty fallback would only make the
    // peer see a malformed message.
    serde_json::to_vec(v).unwrap_or_default()
}

/// Our transit message. It names our relay and no direct hints: this
/// adapter never listens (see "Why guards" in the module docs), but it does
/// connect out to the peer's direct hints, so it announces both abilities.
pub(crate) fn transit(relay: RelayHint) -> Vec<u8> {
    let abilities = serde_json::to_value(Abilities::ALL).unwrap_or_else(|_| json!([]));
    let hints = serde_json::to_value(Hints::new([], [relay])).unwrap_or_else(|_| json!([]));
    to_bytes(&json!({ "transit": { "abilities-v1": abilities, "hints-v1": hints } }))
}

/// A text offer.
pub(crate) fn offer_text(text: &str) -> Vec<u8> {
    to_bytes(&json!({ "offer": { "message": text } }))
}

/// A file offer.
pub(crate) fn offer_file(name: &str, size: u64) -> Vec<u8> {
    to_bytes(&json!({ "offer": { "file": { "filename": name, "filesize": size } } }))
}

/// Our acceptance of a text.
pub(crate) fn message_ack() -> Vec<u8> {
    to_bytes(&json!({ "answer": { "message_ack": "ok" } }))
}

/// Our acceptance of a file.
pub(crate) fn file_ack() -> Vec<u8> {
    to_bytes(&json!({ "answer": { "file_ack": "ok" } }))
}

/// An error, which also serves as the v1 decline.
pub(crate) fn error(reason: &str) -> Vec<u8> {
    to_bytes(&json!({ "error": reason }))
}

/// The last transit record: our digest of what arrived.
pub(crate) fn transit_ack(sha256_hex: &str) -> Vec<u8> {
    to_bytes(&json!({ "ack": "ok", "sha256": sha256_hex }))
}

/// Whether the peer's transit ack says it received exactly `sha256_hex`.
pub(crate) fn transit_ack_matches(record: &[u8], sha256_hex: &str) -> bool {
    let Ok(v) = serde_json::from_slice::<Value>(record) else {
        return false;
    };
    v.get("ack").and_then(Value::as_str) == Some("ok")
        && v.get("sha256")
            .and_then(Value::as_str)
            .is_some_and(|h| h.eq_ignore_ascii_case(sha256_hex))
}

/// The consent dialog's view of an offer. Values stay as the peer sent
/// them; `Offer::validate` judges them.
pub(crate) fn raw_offer(offer: &OfferMsg) -> Option<RawOffer> {
    let mut raw = RawOffer::new(Protocol::Wormhole, PEER_LABEL);
    match offer {
        OfferMsg::Text(t) => raw.text = Some(t.clone()),
        OfferMsg::File { name, size } => raw.files.push(RawFile {
            name: name.clone(),
            size: *size,
            mime: None,
            sha256: None,
        }),
        OfferMsg::Unsupported => return None,
    }
    Some(raw)
}

/// The library's own serialisation of a relay hint, to compare ours with.
#[cfg(test)]
fn relay_hint_json(relay: RelayHint) -> Value {
    serde_json::to_value(Hints::new([], [relay])).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use magic_wormhole::transit::DirectHint;

    fn p(s: &str) -> Result<PeerMsg, ErrorInfo> {
        parse(s.as_bytes())
    }

    #[test]
    fn offers_parse_with_sizes_kept_signed() {
        assert_eq!(
            p(r#"{"offer":{"file":{"filename":"../../etc/passwd","filesize":12}}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::File {
                name: "../../etc/passwd".into(),
                size: 12
            })
        );
        assert_eq!(
            p(r#"{"offer":{"file":{"filename":"a","filesize":-1}}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::File {
                name: "a".into(),
                size: -1
            })
        );
        assert_eq!(
            p(r#"{"offer":{"file":{"filename":"a","filesize":18446744073709551615}}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::File {
                name: "a".into(),
                size: i128::from(u64::MAX)
            })
        );
        assert_eq!(
            p(r#"{"offer":{"file":{"filename":"a","filesize":1e30}}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::File {
                name: "a".into(),
                size: i128::MAX
            })
        );
        assert_eq!(
            p(r#"{"offer":{"file":{"filename":"a","filesize":-1e30}}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::File {
                name: "a".into(),
                size: i128::MIN
            })
        );
        for bad in [
            r#"{"offer":{"file":{"filename":"a","filesize":5.5}}}"#,
            r#"{"offer":{"file":{"filename":"a","filesize":5.0}}}"#,
            r#"{"offer":{"file":{"filename":"a","filesize":"5"}}}"#,
            r#"{"offer":{"file":{"filename":7,"filesize":5}}}"#,
            r#"{"offer":{"file":{"filesize":5}}}"#,
            r#"{"offer":{"message":7}}"#,
            r#"{"offer":{"message":"a","file":{}}}"#,
            r#"{"offer":[]}"#,
        ] {
            assert!(p(bad).is_err(), "{bad}");
        }
        assert_eq!(
            p(r#"{"offer":{"directory":{"dirname":"photos","mode":"zipfile/deflated","zipsize":100,"numbytes":200,"numfiles":3}}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::File { name: "photos.zip".into(), size: 100 })
        );
        assert_eq!(
            p(r#"{"offer":{"hologram":{}}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::Unsupported)
        );
        assert_eq!(
            p(r#"{"offer":{"message":"hi"}}"#).unwrap(),
            PeerMsg::Offer(OfferMsg::Text("hi".into()))
        );
    }

    #[test]
    fn messages_must_have_exactly_one_key() {
        assert!(p("{}").is_err());
        assert!(p(r#"{"error":"x","offer":{}}"#).is_err());
        assert!(p("[1]").is_err());
        assert!(p("nonsense").is_err());
        assert_eq!(p(r#"{"error":"x"}"#).unwrap(), PeerMsg::Error);
        assert_eq!(p(r#"{"dilate":{}}"#).unwrap(), PeerMsg::Other);
        assert_eq!(
            p(r#"{"answer":{"file_ack":"ok"}}"#).unwrap(),
            PeerMsg::Answer(Answer::File(true))
        );
        assert_eq!(
            p(r#"{"answer":{"file_ack":"no"}}"#).unwrap(),
            PeerMsg::Answer(Answer::File(false))
        );
        assert_eq!(
            p(r#"{"answer":{"message_ack":"ok"}}"#).unwrap(),
            PeerMsg::Answer(Answer::Message(true))
        );
        assert_eq!(
            p(r#"{"answer":7}"#).unwrap(),
            PeerMsg::Answer(Answer::Other)
        );
    }

    #[test]
    fn transit_hints_are_bounded_and_filtered() {
        let mut hints = vec![json!({"type":"direct-tcp-v1","hostname":"192.168.1.5","port":4000})];
        for i in 0..200u16 {
            hints.push(json!({"type":"direct-tcp-v1","hostname":format!("10.0.{}.{}", i / 250, i % 250),"port":1000 + i}));
        }
        hints.push(json!({"type":"direct-tcp-v1","hostname":"example.com","port":1}));
        hints.push(json!({"type":"direct-tcp-v1","hostname":"192.168.1.5","port":0}));
        hints.push(json!({"type":"direct-tcp-v1","hostname":"192.168.1.5","port":70000}));
        let msg = json!({"transit":{"abilities-v1":[{"type":"direct-tcp-v1"},{"type":"relay-v1"},{"type":"x"}],"hints-v1":hints}});
        let PeerMsg::Transit(t) = parse(&serde_json::to_vec(&msg).unwrap()).unwrap() else {
            panic!()
        };
        assert!(t.direct && t.relay);
        assert_eq!(t.direct_hints.len(), MAX_DIRECT_HINTS);
        assert_eq!(t.direct_hints[0], ("192.168.1.5".parse().unwrap(), 4000));

        let relay = |host: &str| json!({"type":"relay-v1","name":null,"hints":[{"type":"direct-tcp-v1","hostname":host,"port":4001},{"type":"websocket","url":"ws://x/"}]});
        let msg = json!({"transit":{"abilities-v1":[{"type":"relay-v1"}],"hints-v1":[
            relay("bad host"), relay("-x.example"), relay("transit.example"), relay("::1"), relay("third.example"),
        ]}});
        let PeerMsg::Transit(t) = parse(&serde_json::to_vec(&msg).unwrap()).unwrap() else {
            panic!()
        };
        assert!(!t.direct && t.relay);
        assert_eq!(
            t.relays,
            vec![
                vec![Endpoint {
                    host: "transit.example".into(),
                    port: 4001
                }],
                vec![Endpoint {
                    host: "::1".into(),
                    port: 4001
                }],
            ]
        );
        assert!(p(r#"{"transit":[]}"#).is_err());
        assert_eq!(
            p(r#"{"transit":{}}"#).unwrap(),
            PeerMsg::Transit(TheirTransit::default())
        );
    }

    #[test]
    fn our_messages_match_the_reference_shapes() {
        let relay = RelayHint::new(
            Some("transit.magic-wormhole.io".into()),
            [DirectHint::new("transit.magic-wormhole.io", 4001)],
            [],
        );
        let v: Value = serde_json::from_slice(&transit(relay.clone())).unwrap();
        assert_eq!(
            v,
            json!({"transit": {
                "abilities-v1": [{"type":"direct-tcp-v1"},{"type":"relay-v1"}],
                "hints-v1": relay_hint_json(relay),
            }})
        );
        let v: Value = serde_json::from_slice(&offer_file("a.jpg", 34556)).unwrap();
        assert_eq!(
            v,
            json!({"offer":{"file":{"filename":"a.jpg","filesize":34556}}})
        );
        let v: Value = serde_json::from_slice(&offer_text("hello")).unwrap();
        assert_eq!(v, json!({"offer":{"message":"hello"}}));
        assert_eq!(&file_ack(), br#"{"answer":{"file_ack":"ok"}}"#);
        assert_eq!(&message_ack(), br#"{"answer":{"message_ack":"ok"}}"#);
        assert!(transit_ack_matches(&transit_ack("abcd"), "ABCD"));
        assert!(!transit_ack_matches(
            br#"{"ack":"no","sha256":"abcd"}"#,
            "abcd"
        ));
        assert!(!transit_ack_matches(b"garbage", "abcd"));
    }

    #[test]
    fn raw_offers_keep_what_the_peer_said() {
        let r = raw_offer(&OfferMsg::File {
            name: "../x".into(),
            size: -5,
        })
        .unwrap();
        assert_eq!(r.files[0].size, -5);
        assert_eq!(r.files[0].name, "../x");
        assert_eq!(r.protocol, Protocol::Wormhole);
        assert!(raw_offer(&OfferMsg::Unsupported).is_none());
    }
}
