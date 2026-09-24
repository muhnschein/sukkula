//! `docs/FFI.md` shows every message of the interface; this keeps it true.
//!
//! Every fenced block marked `json command` must parse as a command, every
//! `json event` must deserialise into `Event` and serialise back to exactly
//! the same JSON (so a misspelt or missing field fails, although `Event`
//! itself ignores unknown fields), and every `json config` must parse as a
//! start configuration. Then every command, event, error code and enum
//! value the API has must appear in the page. The lists come from serde
//! itself, so a new variant without an example fails here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use serde_json::Value;
use sukkula_core::Protocol;
use sukkula_core::config::Visibility;
use sukkula_core::consent::Closed;
use sukkula_core::limits::{MAX_EVENT_BYTES, MAX_MESSAGE_BYTES};
use sukkula_engine::api::{
    API_VERSION, Command, DeviceType, Direction, ErrorCode, Event, MAX_IN_FLIGHT_COMMANDS,
    MAX_LISTED_FILES, Outcome, ProtocolState, SendTarget, parse_command, parse_start_config,
};

const DOC: &str = include_str!("../../../docs/FFI.md");

struct Block {
    line: usize,
    kind: String,
    body: String,
}

/// Every fenced code block whose info string starts with `json`.
fn json_blocks() -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<Block> = None;
    for (n, line) in DOC.lines().enumerate() {
        match open.as_mut() {
            None => {
                if let Some(info) = line.strip_prefix("```") {
                    let info = info.trim();
                    open = Some(Block {
                        line: n + 1,
                        kind: info.to_owned(),
                        body: String::new(),
                    });
                }
            }
            Some(_) if line.trim_end() == "```" => {
                let b = open.take().unwrap();
                if b.kind.starts_with("json") {
                    blocks.push(b);
                }
            }
            Some(b) => {
                b.body.push_str(line);
                b.body.push('\n');
            }
        }
    }
    assert!(open.is_none(), "an unclosed code block");
    blocks
}

/// The names serde accepts for `T`, read from the error for a name it does
/// not: "unknown variant `~`, expected one of `a`, `b`, ...".
fn variants<T: DeserializeOwned>(probe: &str) -> Vec<String> {
    let err = serde_json::from_str::<T>(probe)
        .err()
        .expect("the probe must not parse")
        .to_string();
    let (_, expected) = err
        .split_once("expected")
        .unwrap_or_else(|| panic!("an unexpected serde error: {err}"));
    let names: Vec<String> = expected
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    assert!(!names.is_empty(), "no names in: {err}");
    names
}

#[test]
fn every_block_is_marked_and_valid() {
    let blocks = json_blocks();
    assert!(blocks.len() > 30, "only {} JSON blocks", blocks.len());
    for b in &blocks {
        let at = format!("docs/FFI.md:{}", b.line);
        match b.kind.as_str() {
            "json command" => {
                assert!(b.body.len() <= MAX_MESSAGE_BYTES, "{at}");
                let env = parse_command(b.body.trim())
                    .unwrap_or_else(|e| panic!("{at}: not a command: {e:?}"));
                assert_eq!(env.v, API_VERSION, "{at}");
            }
            "json event" => {
                let value: Value =
                    serde_json::from_str(&b.body).unwrap_or_else(|e| panic!("{at}: not JSON: {e}"));
                let event: Event = serde_json::from_value(value.clone())
                    .unwrap_or_else(|e| panic!("{at}: not an event: {e}"));
                assert_eq!(
                    serde_json::to_value(&event).unwrap(),
                    value,
                    "{at}: the engine would emit this differently"
                );
                assert!(serde_json::to_string(&event).unwrap().len() <= MAX_EVENT_BYTES);
                check_event_invariants(&at, &event);
            }
            "json config" => {
                let cfg = parse_start_config(b.body.trim())
                    .unwrap_or_else(|e| panic!("{at}: not a start configuration: {e:?}"));
                assert!(cfg.data_dir.starts_with('/') && cfg.download_dir.starts_with('/'));
            }
            other => panic!(
                "{at}: a JSON block marked {other:?}; mark it json command, json event or json config"
            ),
        }
    }
}

/// What the engine guarantees about an event, held of the examples too.
fn check_event_invariants(at: &str, event: &Event) {
    match event {
        Event::OfferPending { offer } => {
            assert!(offer.files.len() <= MAX_LISTED_FILES, "{at}");
            assert_eq!(
                offer.files.len() + offer.more_files,
                offer.file_count,
                "{at}"
            );
            if offer.more_files == 0 {
                let sum: u64 = offer.files.iter().map(|f| f.size).sum();
                assert_eq!(sum, offer.total_bytes, "{at}");
            }
            assert!(
                offer.has_text || offer.file_count > 0,
                "{at}: an empty offer"
            );
        }
        Event::TransferStarted { transfer } => {
            assert!(transfer.files.len() <= MAX_LISTED_FILES, "{at}");
            assert!(transfer.files.len() <= transfer.file_count, "{at}");
        }
        Event::TransferProgress { bytes, total, .. } => assert!(bytes <= total, "{at}"),
        Event::TransferFinished { outcome, saved, .. } => {
            assert!(saved.is_empty() || *outcome == Outcome::Done, "{at}");
            for name in saved {
                assert!(sukkula_core::name::is_safe(name), "{at}: {name:?}");
            }
        }
        Event::WormholeCode { qr, .. } => {
            let size = usize::try_from(qr.size).unwrap();
            assert_eq!(qr.rows.len(), size, "{at}");
            for row in &qr.rows {
                assert_eq!(row.len(), size, "{at}");
                assert!(row.bytes().all(|b| b == b'0' || b == b'1'), "{at}");
            }
        }
        Event::Started { api, .. } => assert_eq!(*api, API_VERSION, "{at}"),
        _ => {}
    }
}

#[test]
fn every_command_and_event_has_an_example() {
    let mut commands = BTreeSet::new();
    let mut events = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for b in json_blocks() {
        let v: Value = serde_json::from_str(&b.body).unwrap();
        match b.kind.as_str() {
            "json command" => {
                commands.insert(v["cmd"]["type"].as_str().unwrap().to_owned());
                if let Some(p) = v["cmd"]["target"]["protocol"].as_str() {
                    targets.insert(p.to_owned());
                }
            }
            "json event" => {
                events.insert(v["type"].as_str().unwrap().to_owned());
            }
            _ => {}
        }
    }
    for c in variants::<Command>(r#"{"type":"~"}"#) {
        assert!(commands.contains(&c), "command {c} has no example");
    }
    for e in variants::<Event>(r#"{"type":"~"}"#) {
        assert!(events.contains(&e), "event {e} has no example");
    }
    for t in variants::<SendTarget>(r#"{"protocol":"~"}"#) {
        assert!(targets.contains(&t), "send target {t} has no example");
    }
}

#[test]
fn every_code_and_value_is_documented() {
    for code in variants::<ErrorCode>(r#""~""#) {
        assert!(
            DOC.contains(&format!("| `{code}` |")),
            "error code {code} is not in the table"
        );
    }
    let values = [
        variants::<Protocol>(r#""~""#),
        variants::<ProtocolState>(r#""~""#),
        variants::<Closed>(r#""~""#),
        variants::<Direction>(r#""~""#),
        variants::<DeviceType>(r#""~""#),
        variants::<Visibility>(r#""~""#),
        variants::<Outcome>(r#"{"result":"~"}"#),
    ];
    for value in values.iter().flatten() {
        assert!(
            DOC.contains(&format!("`{value}`")),
            "value {value} is not documented"
        );
    }
}

#[test]
fn the_limits_on_the_page_are_the_limits_in_the_code() {
    assert_eq!(MAX_MESSAGE_BYTES, 64 * 1024);
    assert!(DOC.contains("| A command, or the start configuration | 64 KiB"));
    assert!(DOC.contains(&format!(
        "| Commands waiting for their reply | {MAX_IN_FLIGHT_COMMANDS} |"
    )));
    assert!(DOC.contains(&format!(
        "{MAX_IN_FLIGHT_COMMANDS} commands are waiting for their replies"
    )));
    assert_eq!(MAX_EVENT_BYTES, 256 * 1024);
    assert!(DOC.contains("| An event | 256 KiB of JSON |"));
    assert!(DOC.contains(&format!("**`API_VERSION`** is `{API_VERSION}`")));
    assert!(DOC.contains(&format!("At most {MAX_LISTED_FILES} files are listed")));
}
