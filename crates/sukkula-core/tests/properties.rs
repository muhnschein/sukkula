//! Every S-rule `sukkula-core` enforces, as a property over arbitrary input
//! (spec §7, "Core: every S-rule as a property test").
//!
//! | Rule | Properties |
//! | --- | --- |
//! | S1 | `sanitize` gives one normal path component, non-empty, at most 200 bytes, nothing forbidden, no leading dot, no trailing dot or space, idempotent; `numbered` keeps all of that; a short clean extension survives shortening |
//! | S2 | `display` and `message` never emit a forbidden character, respect their caps, collapse whitespace, cap mark stacks, and are idempotent |
//! | S3 | the inbox never has more on disk than declared, places exactly the bytes sent or nothing, and always cleans up staging, on commit, error and drop alike |
//! | S4, S6 | `Offer::validate` never accepts a negative or oversized size, never overflows its total, never more than 500 files, and everything it passes on is S1/S2-clean |
//! | S5 | the consent queue never holds more than its cap, closes every offer exactly once, and an answer is delivered only to an offer that is waiting |
//! | S7 | `ReachPolicy` agrees with an independent statement of the ranges; the rate limiter stays bounded and, below its capacity, behaves exactly as an unbounded one |
//!
//! The oracle in `common/oracle.rs` is a second, independent statement of
//! the character rules; `hex` and `config` get round-trip properties.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::disallowed_methods,
    clippy::type_complexity,
    clippy::field_reassign_with_default
)]

#[path = "common/oracle.rs"]
mod oracle;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use proptest::collection::vec;
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use sukkula_core::config::Settings;
use sukkula_core::consent::{Closed, ConsentBroker, ConsentEvent, Decision, Refusal};
use sukkula_core::hex;
use sukkula_core::inbox::{Inbox, STAGING_DIR};
use sukkula_core::limits::{
    MAX_ALIAS_CHARS, MAX_EXTENSION_BYTES, MAX_FILE_BYTES, MAX_FILES_PER_OFFER, MAX_MESSAGE_BYTES,
    MAX_MIME_BYTES, MAX_MODEL_CHARS, MAX_NAME_BYTES, MAX_OFFER_BYTES, MAX_PIN_CHARS,
    RATE_LIMIT_ENTRIES,
};
use sukkula_core::name::{self, sanitize};
use sukkula_core::offer::{Offer, OfferError, Protocol, RawFile, RawOffer};
use sukkula_core::reach::{RateLimiter, ReachPolicy};
use sukkula_core::text;

/// Characters that have broken, or could break, a sanitiser: every class
/// the rules name, the edges of the tables, and multi-byte ordinary text.
const NASTY: &[char] = &[
    '\u{0}',
    '\u{1B}',
    '\u{7F}',
    '\u{85}',
    '\u{9B}',
    '\u{AD}',
    '\u{A0}',
    '\u{34F}',
    '\u{61C}',
    '\u{115F}',
    '\u{1160}',
    '\u{17B4}',
    '\u{180E}',
    '\u{200B}',
    '\u{200D}',
    '\u{200E}',
    '\u{2028}',
    '\u{2029}',
    '\u{202E}',
    '\u{2066}',
    '\u{2069}',
    '\u{2064}',
    '\u{2800}',
    '\u{3000}',
    '\u{3164}',
    '\u{FE0F}',
    '\u{FEFF}',
    '\u{FFA0}',
    '\u{FFF9}',
    '\u{FFFC}',
    '\u{FFFD}',
    '\u{FFFF}',
    '\u{E000}',
    '\u{E0001}',
    '\u{E0041}',
    '\u{E0100}',
    '\u{10FFFF}',
    '\u{1D173}',
    '\u{13430}',
    '\u{301}',
    '\u{36F}',
    '\u{E49}',
    '\u{591}',
    '\u{64B}',
    '\u{20DD}',
    '\u{1AB0}',
    '\u{FE20}',
    '…',
    'é',
    '€',
    '𝕏',
    'ß',
    'ﬁ',
    '\u{130}',
    '/',
    '\\',
    '.',
    ' ',
    '\t',
    '\n',
    '\r',
];

fn nasty_char() -> impl Strategy<Value = char> {
    prop_oneof![
        4 => proptest::char::range('a', 'z'),
        3 => proptest::sample::select(NASTY),
        1 => Just('.'),
        1 => Just(' '),
        2 => any::<char>(),
    ]
}

fn nasty_string(max: usize) -> impl Strategy<Value = String> {
    vec(nasty_char(), 0..max).prop_map(|v| v.into_iter().collect())
}

/// A name long enough to take the "cut mid-scan" path, with a nasty tail.
fn long_name() -> impl Strategy<Value = String> {
    (700usize..1000, nasty_string(40)).prop_map(|(n, tail)| format!("{}{tail}", "x".repeat(n)))
}

fn any_name() -> impl Strategy<Value = String> {
    prop_oneof![4 => nasty_string(80), 1 => long_name()]
}

// ---------------------------------------------------------------- S1 names

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn sanitize_gives_one_safe_component(raw in any_name()) {
        let out = sanitize(&raw);
        let s = out.as_str();
        prop_assert_eq!(oracle::name_violation(s), None, "{:?} -> {:?}", raw, s);
        prop_assert!(!s.chars().any(text::is_forbidden), "{:?}", s);
        prop_assert!(s != STAGING_DIR);
        prop_assert!(name::is_safe(s));
        // Idempotent: checked at two boundaries reads as checked at one.
        let twice = sanitize(s);
        prop_assert_eq!(twice.as_str(), s);
    }

    #[test]
    fn numbered_names_stay_safe(raw in any_name(), n in any::<u32>()) {
        let base = sanitize(&raw);
        let numbered = base.numbered(n);
        let s = numbered.as_str();
        prop_assert_eq!(oracle::name_violation(s), None, "{:?} ({}) -> {:?}", raw, n, s);
        prop_assert!(name::is_safe(s), "{:?}", s);
        if n > 0 {
            prop_assert!(s != base.as_str());
            let suffix = format!(" ({n})");
            prop_assert!(s.contains(&suffix));
        }
    }

    #[test]
    fn a_clean_extension_survives_shortening(
        stem_len in 150usize..3000,
        ext in "[a-zA-Z0-9]{1,16}",
        junk in nasty_string(20).prop_filter("one segment", |j| !j.contains(['/', '\\'])),
    ) {
        let raw = format!("{}{junk}.{ext}", "s".repeat(stem_len));
        let out = sanitize(&raw);
        let dotted = format!(".{ext}");
        prop_assert!(out.as_str().ends_with(&dotted), "{:?}", out);
        prop_assert!(out.as_str().len() <= MAX_NAME_BYTES);
        prop_assert!(ext.len() <= MAX_EXTENSION_BYTES);
    }

    #[test]
    fn path_structure_never_survives(parts in vec(nasty_string(12), 1..6), sep in prop_oneof![Just('/'), Just('\\')]) {
        let raw: String = parts.join(&sep.to_string());
        let out = sanitize(&raw);
        let last = raw.rsplit(['/', '\\']).next().unwrap();
        // Only the last segment can contribute: its sanitised form, unless
        // that is empty.
        let alone = sanitize(last);
        prop_assert_eq!(out, alone);
    }
}

// ------------------------------------------------------- S2 display text

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn display_keeps_its_promises(raw in nasty_string(200), max in 0usize..80) {
        let out = text::display(&raw, max);
        prop_assert_eq!(oracle::display_violation(&out, max), None, "{:?} -> {:?}", raw, out);
        prop_assert!(!out.chars().any(text::is_forbidden));
        prop_assert_eq!(text::display(&out, max), out.clone(), "not idempotent");
        if max == 0 {
            prop_assert!(out.is_empty());
        }
    }

    #[test]
    fn display_leaves_short_clean_text_alone(raw in "[a-zA-Z0-9äöü€ ]{0,64}") {
        let clean: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        prop_assert_eq!(text::display(&clean, MAX_ALIAS_CHARS), clean);
    }

    #[test]
    fn message_keeps_its_promises(raw in nasty_string(400)) {
        let out = text::message(&raw);
        prop_assert_eq!(oracle::message_violation(&out), None, "{:?} -> {:?}", raw, out);
        prop_assert!(!out.chars().any(text::is_forbidden_in_message));
        prop_assert_eq!(text::message(&out), out.clone(), "not idempotent");
    }

}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn huge_messages_are_capped(unit in nasty_string(8), reps in 8_000usize..20_000) {
        let raw = unit.repeat(reps);
        let out = text::message(&raw);
        prop_assert!(out.len() <= MAX_MESSAGE_BYTES);
        prop_assert_eq!(oracle::message_violation(&out), None);
    }

    #[test]
    fn truncate_bytes_is_a_prefix_on_a_boundary(raw in nasty_string(60), max in 0usize..200) {
        let t = text::truncate_bytes(&raw, max);
        prop_assert!(t.len() <= max);
        prop_assert!(raw.starts_with(t));
        // The longest such prefix: one more character would not fit.
        if let Some(c) = raw.get(t.len()..).and_then(|rest| rest.chars().next()) {
            prop_assert!(t.len() + c.len_utf8() > max);
        }
    }
}

// ---------------------------------------------------------- S4/S6 offers

fn size() -> impl Strategy<Value = i128> {
    let max = i128::from(MAX_FILE_BYTES);
    prop_oneof![
        4 => 0i128..10_000,
        1 => Just(max),
        1 => Just(max + 1),
        1 => Just(-1i128),
        1 => Just(i128::from(i64::MIN)),
        1 => Just(i128::from(u64::MAX)),
        1 => Just(i128::MAX),
        1 => Just(i128::MIN),
        2 => (max - 4)..(max + 4),
        1 => any::<i128>(),
    ]
}

fn raw_file() -> impl Strategy<Value = RawFile> {
    (
        any_name(),
        size(),
        proptest::option::of(prop_oneof![
            Just("image/JPEG".to_owned()),
            Just("text/plain; charset=utf-8".to_owned()),
            nasty_string(30),
        ]),
        proptest::option::of(prop_oneof![
            "[0-9a-fA-F]{64}",
            "[0-9a-f]{0,70}",
            nasty_string(64),
        ]),
    )
        .prop_map(|(name, size, mime, sha256)| RawFile {
            name,
            size,
            mime,
            sha256,
        })
}

fn raw_offer() -> impl Strategy<Value = RawOffer> {
    (
        proptest::sample::select(Protocol::ALL.to_vec()),
        nasty_string(120),
        proptest::option::of(nasty_string(100)),
        vec(raw_file(), 0..8),
        proptest::option::of(prop_oneof![
            nasty_string(200),
            (MAX_MESSAGE_BYTES - 2..MAX_MESSAGE_BYTES + 3).prop_map(|n| "m".repeat(n)),
        ]),
        proptest::option::of(prop_oneof!["[0-9]{0,18}", nasty_string(8)]),
    )
        .prop_map(|(protocol, sender, model, files, text, pin)| RawOffer {
            protocol,
            sender,
            model,
            files,
            text,
            pin,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn validate_never_passes_a_bad_offer(raw in raw_offer()) {
        let sizes: Vec<i128> = raw.files.iter().map(|f| f.size).collect();
        let sum: i128 = sizes.iter().copied().fold(0i128, i128::saturating_add);
        let file_count = raw.files.len();
        let text_len = raw.text.as_ref().map(String::len);
        match Offer::validate(raw) {
            Ok(o) => {
                prop_assert!(o.files.len() <= MAX_FILES_PER_OFFER);
                prop_assert!(!o.files.is_empty() || o.text.is_some());
                let mut total: u64 = 0;
                for (f, raw_size) in o.files.iter().zip(&sizes) {
                    prop_assert!(*raw_size >= 0);
                    prop_assert!(f.size <= MAX_FILE_BYTES);
                    prop_assert_eq!(i128::from(f.size), *raw_size);
                    total = total.checked_add(f.size).unwrap();
                    prop_assert_eq!(oracle::name_violation(f.name.as_str()), None);
                    if let Some(m) = &f.mime {
                        prop_assert!(m.len() <= MAX_MIME_BYTES);
                        prop_assert!(m.bytes().all(|b| b.is_ascii_graphic()));
                        prop_assert!(!m.bytes().any(|b| b.is_ascii_uppercase()));
                    }
                }
                prop_assert_eq!(o.total_bytes, total);
                prop_assert_eq!(i128::from(total), sum);
                prop_assert!(o.total_bytes <= MAX_OFFER_BYTES);
                prop_assert!(!o.sender.is_empty());
                prop_assert_eq!(oracle::display_violation(&o.sender, MAX_ALIAS_CHARS), None);
                if let Some(m) = &o.model {
                    prop_assert!(!m.is_empty());
                    prop_assert_eq!(oracle::display_violation(m, MAX_MODEL_CHARS), None);
                }
                if let Some(t) = &o.text {
                    prop_assert!(!t.is_empty());
                    prop_assert_eq!(oracle::message_violation(t), None);
                }
                if let Some(p) = &o.pin {
                    prop_assert!(!p.is_empty() && p.len() <= MAX_PIN_CHARS);
                    prop_assert!(p.bytes().all(|b| b.is_ascii_digit()));
                }
            }
            Err(e) => {
                // Refusals name a real problem.
                match e {
                    OfferError::NegativeSize(i) => prop_assert!(sizes[i] < 0),
                    OfferError::FileTooLarge(i) => {
                        prop_assert!(sizes[i] > i128::from(MAX_FILE_BYTES));
                    }
                    OfferError::TooManyFiles(n) => {
                        prop_assert_eq!(n, file_count);
                        prop_assert!(n > MAX_FILES_PER_OFFER);
                    }
                    OfferError::OfferTooLarge => {
                        // Some prefix of valid sizes runs over the total cap.
                        let mut running = 0i128;
                        let over = sizes.iter().any(|&s| {
                            running = running.saturating_add(s);
                            running > i128::from(MAX_OFFER_BYTES)
                        });
                        prop_assert!(over);
                    }
                    OfferError::TextTooLarge => prop_assert!(text_len.unwrap() > MAX_MESSAGE_BYTES),
                    OfferError::Empty | OfferError::BadDigest(_) | OfferError::BadPin => {}
                }
            }
        }
    }

    #[test]
    fn the_file_count_is_capped(n in MAX_FILES_PER_OFFER - 3..MAX_FILES_PER_OFFER + 3, size in 0i128..1000) {
        let files = (0..n)
            .map(|i| RawFile { name: format!("{i}.bin"), size, ..RawFile::default() })
            .collect();
        let raw = RawOffer { files, ..RawOffer::new(Protocol::Wormhole, "w") };
        match Offer::validate(raw) {
            Ok(o) => {
                prop_assert!(n <= MAX_FILES_PER_OFFER);
                prop_assert_eq!(o.files.len(), n);
                prop_assert_eq!(i128::from(o.total_bytes), size * n as i128);
            }
            Err(e) => {
                prop_assert!(n > MAX_FILES_PER_OFFER || n == 0);
                if n > 0 {
                    prop_assert_eq!(e, OfferError::TooManyFiles(n));
                }
            }
        }
    }

    #[test]
    fn negative_or_oversized_anywhere_is_refused(
        mut files in vec(raw_file(), 1..6),
        at in any::<prop::sample::Index>(),
        bad in prop_oneof![
            (i128::MIN..0i128),
            (i128::from(MAX_FILE_BYTES) + 1..=i128::MAX),
        ],
    ) {
        let i = at.index(files.len());
        files[i].size = bad;
        let raw = RawOffer { files, ..RawOffer::new(Protocol::QuickShare, "p") };
        prop_assert!(Offer::validate(raw).is_err());
    }
}

// ------------------------------------------------------------- S3 inbox

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n != STAGING_DIR)
        .collect();
    v.sort();
    v
}

fn staged_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir.join(STAGING_DIR))
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn the_inbox_writes_what_was_declared_or_nothing(
        declared in prop_oneof![0u64..400, Just(0u64)],
        chunks in vec(vec(any::<u8>(), 0..150), 0..6),
        digest_mode in 0u8..3,
        end in 0u8..3,
        raw_name in any_name(),
    ) {
        rt().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let inbox = Inbox::open(dir.path()).unwrap();
            let all: Vec<u8> = chunks.concat();
            let prefix = &all[..all.len().min(declared as usize)];
            let digest = match digest_mode {
                0 => None,
                1 => Some(Sha256::digest(prefix).into()),
                _ => Some([0x42u8; 32]),
            };
            let name = sanitize(&raw_name);
            let mut f = inbox.begin(&name, declared, digest).await.unwrap();
            let mut sent = Vec::new();
            let mut failed = false;
            for c in &chunks {
                match f.write(c).await {
                    Ok(()) => sent.extend_from_slice(c),
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
                // Never more on disk than declared, at any point.
                prop_assert!(staged_bytes(dir.path()) <= declared);
                prop_assert_eq!(f.written(), sent.len() as u64);
            }
            prop_assert!(sent.len() as u64 <= declared);
            let result = match end {
                0 => {
                    drop(f);
                    None
                }
                _ => Some(f.commit().await),
            };
            prop_assert_eq!(std::fs::read_dir(dir.path().join(STAGING_DIR)).unwrap().count(), 0);
            let placed = entries(dir.path());
            match result {
                Some(Ok(saved)) => {
                    prop_assert!(!failed);
                    prop_assert_eq!(sent.len() as u64, declared);
                    prop_assert_eq!(saved.size, declared);
                    prop_assert_eq!(std::fs::read(&saved.path).unwrap(), sent.clone());
                    prop_assert_eq!(placed, vec![saved.name.as_str().to_owned()]);
                    // A wrong digest never places anything.
                    prop_assert!(digest_mode != 2);
                    if digest_mode == 1 {
                        let d: [u8; 32] = Sha256::digest(&sent).into();
                        prop_assert_eq!(Some(d), digest);
                    }
                    let meta = std::fs::symlink_metadata(&saved.path).unwrap();
                    prop_assert!(meta.file_type().is_file());
                    prop_assert_eq!(std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777, 0o600);
                }
                _ => prop_assert!(placed.is_empty(), "{:?}", placed),
            }
            Ok(())
        })?;
    }
}

// ----------------------------------------------------------- S5 consent

#[derive(Clone, Debug)]
enum Op {
    Ask,
    Answer(usize, bool),
    AnswerUnknown(u64),
    Withdraw(usize),
    Advance(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Ask),
        3 => (any::<usize>(), any::<bool>()).prop_map(|(i, a)| Op::Answer(i, a)),
        1 => (1000u64..u64::MAX).prop_map(Op::AnswerUnknown),
        1 => any::<usize>().prop_map(Op::Withdraw),
        2 => (0u64..70).prop_map(Op::Advance),
    ]
}

fn simple_offer() -> Offer {
    let mut raw = RawOffer::new(Protocol::LocalSend, "Alice");
    raw.files.push(RawFile {
        name: "a".into(),
        size: 1,
        ..RawFile::default()
    });
    Offer::validate(raw).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn consent_accounting_holds_under_any_schedule(ops in vec(op(), 1..40), max_pending in 1usize..4) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(async {
            let log: Arc<Mutex<Vec<ConsentEvent>>> = Arc::default();
            let l = log.clone();
            let broker = ConsentBroker::with_limits(
                Arc::new(move |e| l.lock().unwrap().push(e)),
                Duration::from_secs(60),
                max_pending,
            );
            let offer = Arc::new(simple_offer());
            let mut tasks: Vec<(Option<u64>, tokio::task::JoinHandle<Result<u64, Refusal>>)> = Vec::new();
            let settle = || async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            };
            for op in ops {
                let seen_before = log.lock().unwrap().len();
                match op {
                    Op::Ask => {
                        let b = broker.clone();
                        let o = offer.clone();
                        let h = tokio::spawn(async move { b.ask(&o).await });
                        settle().await;
                        let new_id = log.lock().unwrap()[seen_before..].iter().find_map(|e| match e {
                            ConsentEvent::Pending { id, .. } => Some(*id),
                            ConsentEvent::Closed { .. } => None,
                        });
                        if new_id.is_none() {
                            prop_assert!(h.is_finished());
                        }
                        tasks.push((new_id, h));
                    }
                    Op::Answer(i, accept) if !tasks.is_empty() => {
                        let (id, _) = &tasks[i % tasks.len()];
                        if let Some(id) = *id {
                            let open = is_open(&log.lock().unwrap(), id);
                            let d = if accept { Decision::Accept } else { Decision::Decline };
                            let delivered = broker.answer(id, d);
                            // An offer that is waiting takes an answer, and
                            // only one that is waiting.
                            prop_assert_eq!(delivered, open, "offer {}", id);
                            settle().await;
                        }
                    }
                    Op::Answer(..) => {}
                    Op::AnswerUnknown(id) => prop_assert!(!broker.answer(id, Decision::Accept)),
                    Op::Withdraw(i) if !tasks.is_empty() => {
                        let n = tasks.len();
                        tasks[i % n].1.abort();
                        settle().await;
                    }
                    Op::Withdraw(_) => {}
                    Op::Advance(s) => {
                        tokio::time::advance(Duration::from_secs(s)).await;
                        settle().await;
                    }
                }
                prop_assert!(broker.pending() <= max_pending);
                let open = open_ids(&log.lock().unwrap());
                prop_assert_eq!(open.len(), broker.pending());
            }
            broker.shutdown();
            settle().await;
            prop_assert_eq!(broker.pending(), 0);
            let log = log.lock().unwrap().clone();
            // Every offer shown was closed exactly once, after it was shown.
            let mut pending: HashMap<u64, usize> = HashMap::new();
            let mut closed: HashMap<u64, Closed> = HashMap::new();
            for e in &log {
                match e {
                    ConsentEvent::Pending { id, .. } => {
                        prop_assert!(!closed.contains_key(id));
                        *pending.entry(*id).or_default() += 1;
                    }
                    ConsentEvent::Closed { id, reason } => {
                        prop_assert!(pending.contains_key(id));
                        prop_assert!(closed.insert(*id, *reason).is_none(), "closed twice: {}", id);
                    }
                }
            }
            prop_assert!(pending.values().all(|n| *n == 1));
            prop_assert_eq!(pending.len(), closed.len());
            // And each adapter was told what the UI was told.
            for (id, h) in tasks {
                let r = h.await;
                let Some(id) = id else {
                    // Never shown: refused outright, or aborted first.
                    prop_assert!(matches!(&r, Ok(Err(Refusal::Busy | Refusal::Shutdown))) || r.is_err());
                    continue;
                };
                match (r, closed[&id]) {
                    (Ok(Ok(got)), Closed::Accepted) => prop_assert_eq!(got, id),
                    (Ok(Err(Refusal::Declined)), Closed::Declined)
                    | (Ok(Err(Refusal::TimedOut)), Closed::TimedOut)
                    | (Ok(Err(Refusal::Shutdown)), Closed::Shutdown) => {}
                    (Err(e), Closed::Withdrawn) if e.is_cancelled() => {}
                    (r, c) => prop_assert!(false, "adapter got {:?}, UI got {:?}", r, c),
                }
            }
            Ok(())
        })?;
    }
}

fn open_ids(log: &[ConsentEvent]) -> HashSet<u64> {
    let mut open = HashSet::new();
    for e in log {
        match e {
            ConsentEvent::Pending { id, .. } => {
                open.insert(*id);
            }
            ConsentEvent::Closed { id, .. } => {
                open.remove(id);
            }
        }
    }
    open
}

fn is_open(log: &[ConsentEvent], id: u64) -> bool {
    open_ids(log).contains(&id)
}

// -------------------------------------------------------------- S7 reach

/// S7 restated over integers rather than through `std`'s helpers.
fn v4_ok(ip: Ipv4Addr, loopback: bool) -> bool {
    let u = u32::from(ip);
    (u >> 24) == 10
        || (u >> 20) == 0xAC1
        || (u >> 16) == 0xC0A8
        || (u >> 16) == 0xA9FE
        || (loopback && (u >> 24) == 127)
}

fn v6_ok(ip: Ipv6Addr, loopback: bool) -> bool {
    let u = u128::from(ip);
    if (u >> 32) == 0xFFFF {
        return v4_ok(Ipv4Addr::from((u & 0xFFFF_FFFF) as u32), loopback);
    }
    (u >> 121) == 0x7E || (u >> 118) == 0x3FA || (loopback && u == 1)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn reach_agrees_with_the_ranges(
        v4 in prop_oneof![
            any::<u32>(),
            // The edges of every range, and one either side.
            proptest::sample::select(vec![
                0x0A00_0000u32, 0x09FF_FFFF, 0x0AFF_FFFF, 0x0B00_0000, 0xAC0F_FFFF, 0xAC10_0000,
                0xAC1F_FFFF, 0xAC20_0000, 0xC0A7_FFFF, 0xC0A8_0000, 0xC0A8_FFFF, 0xC0A9_0000,
                0xA9FD_FFFF, 0xA9FE_0000, 0xA9FE_FFFF, 0xA9FF_0000, 0x7F00_0000, 0x7FFF_FFFF,
                0x8000_0000, 0x6440_0000, 0, u32::MAX,
            ]),
        ],
        v6 in any::<u128>(),
        prefix in proptest::option::of(proptest::sample::select(vec![
            0xFC00u16, 0xFBFF, 0xFDFF, 0xFE00, 0xFE7F, 0xFE80, 0xFEBF, 0xFEC0, 0xFF02, 0x2001, 0x0064, 0x0000,
        ])),
        loopback in any::<bool>(),
    ) {
        let p = ReachPolicy { allow_loopback: loopback };
        let a = Ipv4Addr::from(v4);
        prop_assert_eq!(p.permits(IpAddr::V4(a)), v4_ok(a, loopback), "{}", a);
        prop_assert_eq!(p.permits(IpAddr::V6(a.to_ipv6_mapped())), v4_ok(a, loopback));
        // Most random v6 addresses are public; put some in the prefixes that
        // matter.
        let v6 = match prefix {
            Some(pre) => (u128::from(pre) << 112) | (v6 & ((1u128 << 112) - 1)),
            None => v6,
        };
        let b = Ipv6Addr::from(v6);
        prop_assert_eq!(p.permits(IpAddr::V6(b)), v6_ok(b, loopback), "{}", b);
    }
}

#[derive(Clone, Debug)]
struct Hit {
    who: usize,
    dt_ms: u64,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_limiter_is_bounded_and_exact_below_capacity(
        hits in vec((0usize..RATE_LIMIT_ENTRIES - 1, 0u64..3_000).prop_map(|(who, dt_ms)| Hit { who, dt_ms }), 1..400),
        burst in 1u32..6,
        flood in 0usize..600,
    ) {
        let window = Duration::from_secs(10);
        let mut limiter = RateLimiter::new(burst, window);
        // The model: the same fixed-window rule with unbounded memory.
        let mut model: HashMap<usize, (u32, Instant)> = HashMap::new();
        let t0 = Instant::now();
        let mut now = t0;
        for h in &hits {
            now += Duration::from_millis(h.dt_ms);
            let ip = IpAddr::V4(Ipv4Addr::from(0x0A00_0000u32 + h.who as u32));
            let got = limiter.allow(ip, now);
            let entry = model.entry(h.who).or_insert((burst, now));
            if now.saturating_duration_since(entry.1) >= window {
                *entry = (burst, now);
            }
            let want = entry.0 > 0;
            if want {
                entry.0 -= 1;
            }
            prop_assert_eq!(got, want);
            prop_assert!(limiter.len() <= RATE_LIMIT_ENTRIES);
        }
        // Past capacity: a flood of fresh sources keeps memory bounded.
        for i in 0..flood {
            limiter.allow(IpAddr::V4(Ipv4Addr::from(0xC0A8_0000u32 + i as u32)), now);
            prop_assert!(limiter.len() <= RATE_LIMIT_ENTRIES);
        }
    }
}

// ------------------------------------------------------- hex and config

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn hex_round_trips(bytes in any::<[u8; 32]>(), upper in any::<bool>()) {
        let s = hex::encode(&bytes);
        prop_assert_eq!(s.len(), 64);
        prop_assert!(s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        let s = if upper { s.to_uppercase() } else { s };
        prop_assert_eq!(hex::decode_32(&s), Some(bytes));
    }

    #[test]
    fn hex_decodes_exactly_64_hex_digits(s in prop_oneof!["[0-9a-fA-F]{60,68}", nasty_string(70)]) {
        let ok = s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
        let got = hex::decode_32(&s);
        prop_assert_eq!(got.is_some(), ok);
        if let Some(b) = got {
            prop_assert_eq!(hex::encode(&b), s.to_ascii_lowercase());
        }
    }

    #[test]
    fn settings_validate_to_something_usable(
        device_name in nasty_string(120),
        pin in proptest::option::of(prop_oneof!["[ a-zA-Z0-9]{0,20}", nasty_string(20)]),
        mailbox in proptest::option::of(prop_oneof!["(ws|wss|WSS|http)://[a-z.]{0,30}", nasty_string(30)]),
        relay in proptest::option::of(prop_oneof!["tcp://[a-z.:0-9]{0,30}", nasty_string(30)]),
        logging in any::<bool>(),
    ) {
        let mut s = Settings::default();
        s.device_name = device_name;
        s.localsend.pin = pin;
        s.wormhole.mailbox_url = mailbox;
        s.wormhole.relay_url = relay;
        s.logging = logging;
        if let Ok(v) = s.validate() {
            prop_assert_eq!(oracle::display_violation(&v.device_name, MAX_ALIAS_CHARS), None);
            if let Some(p) = &v.localsend.pin {
                prop_assert!(!p.is_empty() && p.chars().count() <= MAX_PIN_CHARS);
                prop_assert!(p.bytes().all(|b| b.is_ascii_alphanumeric()));
            }
            for (url, schemes) in [
                (&v.wormhole.mailbox_url, &["ws://", "wss://"][..]),
                (&v.wormhole.relay_url, &["tcp://"][..]),
            ] {
                if let Some(u) = url {
                    prop_assert!(u.len() <= 256 && u.bytes().all(|b| b.is_ascii_graphic()));
                    let lower = u.to_ascii_lowercase();
                    prop_assert!(schemes.iter().any(|sc| lower.starts_with(sc) && lower.len() > sc.len()));
                }
            }
            prop_assert_eq!(v.clone().validate().unwrap(), v.clone(), "not idempotent");
            let json = serde_json::to_string(&v).unwrap();
            let back: Settings = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(back, v);
        }
    }
}
