//! What every fuzz target shares: the independent statement of S1 and S2
//! that `crates/sukkula-core/tests/` holds the real one to, compiled from the
//! same file so the three layers (property tests, hostile sweep, fuzzing)
//! can never disagree about what "safe" means; the S-rules every checked
//! offer and listed peer keep, stated once for the protocol targets; and
//! the Quick Share sender the two Quick Share targets play.

#[path = "../../crates/sukkula-core/tests/common/oracle.rs"]
pub mod oracle;

pub mod handshake;
pub mod quickshare;
#[cfg(test)]
mod seeds;

use sukkula_core::limits::{
    MAX_ALIAS_CHARS, MAX_FILE_BYTES, MAX_FILES_PER_OFFER, MAX_MIME_BYTES, MAX_MODEL_CHARS,
    MAX_OFFER_BYTES, MAX_PIN_CHARS,
};
use sukkula_core::offer::Offer;
use sukkula_engine::api::Peer;

/// Asserts `violation` is `None`, naming the input when it is not. A
/// violation is a finding as real as a crash: libFuzzer saves the input.
pub fn assert_clean(what: &str, input: &str, violation: Option<String>) {
    if let Some(v) = violation {
        panic!("{what}: {v} for input {input:?}");
    }
}

/// `data` as a `serde_json::Value`, for restating JSON the engine took;
/// `None` for the JSON the engine may take and `Value` may not: inside
/// something skipped (an unknown key, a raw value kept and never read)
/// serde_json checks neither a lone surrogate escape (Go reads it as
/// U+FFFD) nor a number's range (`1e999`), and `Value` refuses both. Any
/// other failure is the engine taking what is not JSON, and panics as
/// `what`.
#[must_use]
pub fn value_of(data: &[u8], what: &str) -> Option<serde_json::Value> {
    match serde_json::from_slice(data) {
        Ok(v) => Some(v),
        Err(e)
            if ["surrogate", "code point", "number out of range"]
                .iter()
                .any(|k| e.to_string().contains(k)) =>
        {
            None
        }
        Err(e) => panic!("{what} from no JSON: {e}"),
    }
}

/// `v` as serde's derive reads a struct: an object, or its fields as an
/// array in declaration order, `fields`. A restatement that reads keys
/// has to see both as the same object, or it reports the harness.
#[must_use]
pub fn as_struct(v: serde_json::Value, fields: &[&str]) -> serde_json::Value {
    match v {
        serde_json::Value::Array(items) => {
            serde_json::Value::Object(fields.iter().map(|f| (*f).to_owned()).zip(items).collect())
        }
        v => v,
    }
}

/// What every offer the consent dialog is shown keeps, whichever protocol
/// it came over: S1 on every name, S2 on the sender, model and message,
/// S4/S6 on every size and the count, a clean MIME type and PIN. The same
/// statement as `offer_validate`'s, for the protocol targets, which reach
/// `Offer::validate` through their adapters' conversions.
pub fn assert_offer(o: &Offer) {
    assert!(o.files.len() <= MAX_FILES_PER_OFFER, "too many files");
    assert!(!o.files.is_empty() || o.text.is_some(), "an empty offer");
    let mut total: u64 = 0;
    for f in &o.files {
        assert!(f.size <= MAX_FILE_BYTES, "oversized file accepted");
        total = total.checked_add(f.size).expect("total overflowed");
        assert_clean(
            "file name",
            f.name.as_str(),
            oracle::name_violation(f.name.as_str()),
        );
        if let Some(m) = &f.mime {
            assert!(m.len() <= MAX_MIME_BYTES && m.contains('/'), "MIME {m:?}");
            assert!(
                m.bytes()
                    .all(|b| b.is_ascii_graphic() && !b.is_ascii_uppercase()),
                "MIME {m:?}"
            );
        }
    }
    assert_eq!(o.total_bytes, total, "the total is not the sum");
    assert!(o.total_bytes <= MAX_OFFER_BYTES, "offer cap broken");
    assert!(!o.sender.is_empty(), "no sender");
    assert_clean(
        "sender",
        &o.sender,
        oracle::display_violation(&o.sender, MAX_ALIAS_CHARS),
    );
    if let Some(m) = &o.model {
        assert!(!m.is_empty(), "an empty model");
        assert_clean("model", m, oracle::display_violation(m, MAX_MODEL_CHARS));
    }
    if let Some(t) = &o.text {
        assert!(!t.is_empty(), "an empty text");
        assert_clean("text", t, oracle::message_violation(t));
    }
    if let Some(p) = &o.pin {
        assert!(!p.is_empty() && p.len() <= MAX_PIN_CHARS, "PIN {p:?}");
        assert!(p.bytes().all(|b| b.is_ascii_digit()), "PIN {p:?}");
    }
}

/// What every peer in the UI's list keeps: a plain ASCII id, a name that is
/// S2-clean, capped and never empty, and a model that is the same when it
/// is there.
pub fn assert_peer(p: &Peer) {
    assert!(
        !p.id.is_empty() && p.id.bytes().all(|b| b.is_ascii_graphic()),
        "peer id {:?}",
        p.id
    );
    assert!(!p.name.is_empty(), "a peer without a name");
    assert_clean(
        "peer name",
        &p.name,
        oracle::display_violation(&p.name, MAX_ALIAS_CHARS),
    );
    if let Some(m) = &p.model {
        assert!(!m.is_empty(), "an empty model");
        assert_clean(
            "peer model",
            m,
            oracle::display_violation(m, MAX_MODEL_CHARS),
        );
    }
}
