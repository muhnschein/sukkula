//! The hub through its public API: the races it must not have, the bounds
//! it must keep, and `parse_command`'s cost on hostile input.
//!
//! Protocol-agnostic: every test passes with any set of protocol features,
//! including none.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation
)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use proptest::prelude::*;
use sukkula_engine::Engine;
use sukkula_engine::api::{
    API_VERSION, ErrorCode, Event, MAX_ERROR_MESSAGE_CHARS, MAX_IN_FLIGHT_COMMANDS, RequestId,
    StartConfig, parse_command,
};
use sukkula_engine::ctx::EventSink;

/// A sink that records every event, and can be made to wait.
#[derive(Default)]
struct Log {
    events: Mutex<Vec<Event>>,
    changed: Condvar,
    hold: Mutex<bool>,
    released: Condvar,
    after_stop: AtomicBool,
    late: AtomicUsize,
}

impl Log {
    fn sink(self: &Arc<Self>) -> EventSink {
        let log = self.clone();
        Arc::new(move |e| {
            if log.after_stop.load(Ordering::SeqCst) {
                log.late.fetch_add(1, Ordering::SeqCst);
            }
            let mut held = log.hold.lock().unwrap();
            while *held {
                held = log.released.wait(held).unwrap();
            }
            drop(held);
            log.events.lock().unwrap().push(e);
            log.changed.notify_all();
        })
    }

    fn set_hold(&self, on: bool) {
        *self.hold.lock().unwrap() = on;
        self.released.notify_all();
    }

    fn wait_for(&self, what: &str, f: impl Fn(&[Event]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut events = self.events.lock().unwrap();
        while !f(&events) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}: {events:?}");
            events = self.changed.wait_timeout(events, left).unwrap().0;
        }
    }

    fn replies(&self, id: RequestId) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::Reply { id: i, .. } if *i == id))
            .count()
    }

    fn wait_reply(&self, id: RequestId) {
        self.wait_for(&format!("reply {id}"), |events| {
            events
                .iter()
                .any(|e| matches!(e, Event::Reply { id: i, .. } if *i == id))
        });
    }

    fn last_receiving(&self) -> Option<bool> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::Receiving { on, .. } => Some(*on),
                _ => None,
            })
    }
}

fn config(dir: &Path) -> StartConfig {
    StartConfig {
        v: API_VERSION,
        data_dir: dir.join("data").to_string_lossy().into_owned(),
        download_dir: dir.join("dl").to_string_lossy().into_owned(),
        device_model: Some("Test Phone".into()),
        allow_loopback: true,
    }
}

fn start(dir: &Path) -> (Engine, Arc<Log>) {
    let log = Arc::new(Log::default());
    let engine = Engine::start(config(dir), log.sink()).unwrap();
    (engine, log)
}

fn cmd(id: RequestId, body: &str) -> String {
    format!(r#"{{"v":1,"id":{id},"cmd":{body}}}"#)
}

/// `set_settings` restarts running receivers. It used to read the switch,
/// let go of it, and then turn the receivers off and on again -- so a
/// `set_receiving` off in between was undone. Now both hold the switch for
/// their whole run, and whichever order they run in, off stays off.
#[test]
fn set_settings_never_undoes_a_concurrent_switch_off() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, log) = start(dir.path());
    let mut id = 0;
    for round in 0..30 {
        id += 1;
        engine.command_json(&cmd(id, r#"{"type":"set_receiving","on":true}"#));
        log.wait_reply(id);
        assert_eq!(log.last_receiving(), Some(true));
        let settings = id + 1;
        let off = id + 2;
        id += 2;
        let name = format!(r#"{{"type":"set_settings","settings":{{"device_name":"n{round}"}}}}"#);
        if round % 2 == 0 {
            engine.command_json(&cmd(settings, &name));
            engine.command_json(&cmd(off, r#"{"type":"set_receiving","on":false}"#));
        } else {
            engine.command_json(&cmd(off, r#"{"type":"set_receiving","on":false}"#));
            engine.command_json(&cmd(settings, &name));
        }
        log.wait_reply(settings);
        log.wait_reply(off);
        assert_eq!(log.last_receiving(), Some(false), "round {round}");
    }
    engine.stop();
}

/// Two `set_settings` at once leave the file and the engine agreeing.
#[test]
fn concurrent_set_settings_agree_with_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, log) = start(dir.path());
    std::thread::scope(|s| {
        for t in 0..4 {
            let engine = &engine;
            s.spawn(move || {
                for n in 0..10 {
                    let id = 100 + t * 10 + n;
                    engine.command_json(&cmd(
                        id,
                        &format!(
                            r#"{{"type":"set_settings","settings":{{"device_name":"t{t}n{n}"}}}}"#
                        ),
                    ));
                }
            });
        }
    });
    for id in 100..140 {
        log.wait_reply(id);
    }
    engine.command_json(&cmd(1, r#"{"type":"get_settings"}"#));
    log.wait_reply(1);
    let live = log
        .events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Settings {
                effective_device_name,
                ..
            } => Some(effective_device_name.clone()),
            _ => None,
        })
        .unwrap();
    engine.stop();
    // What a restart reads back is what the running engine had.
    let (engine, log) = start(dir.path());
    let saved = match &log.events.lock().unwrap()[1] {
        Event::Settings {
            effective_device_name,
            ..
        } => effective_device_name.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(saved, live);
    engine.stop();
}

#[test]
fn command_json_answers_even_when_busy() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, log) = start(dir.path());
    log.set_hold(true);
    let total = MAX_IN_FLIGHT_COMMANDS as u64 + 10;
    for id in 1..=total {
        engine.command_json(&cmd(id, r#"{"type":"get_settings"}"#));
    }
    log.set_hold(false);
    for id in 1..=total {
        log.wait_reply(id);
    }
    std::thread::sleep(Duration::from_millis(30));
    let busy = log
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(e, Event::Reply { ok: false, error: Some(err), .. } if err.code == ErrorCode::TooLarge)
        })
        .count();
    assert!(busy >= 10, "{busy} refused as busy");
    for id in 1..=total {
        assert_eq!(log.replies(id), 1, "reply to {id}");
    }
    engine.stop();
}

#[test]
fn many_threads_then_stop_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, log) = start(dir.path());
    let next = AtomicUsize::new(1);
    std::thread::scope(|s| {
        for _ in 0..6 {
            s.spawn(|| {
                for _ in 0..200 {
                    let id = next.fetch_add(1, Ordering::SeqCst) as u64;
                    let _ = engine.try_command_json(&cmd(id, r#"{"type":"get_settings"}"#));
                }
            });
        }
    });
    engine.stop();
    log.after_stop.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(log.late.load(Ordering::SeqCst), 0);
    // Each command was answered at most once.
    let mut seen = std::collections::HashSet::new();
    for e in log.events.lock().unwrap().iter() {
        if let Event::Reply { id, .. } = e {
            assert!(seen.insert(*id), "two replies to {id}");
        }
    }
}

/// Engine A's sink stops engine B: no deadlock, and B's sink is quiet
/// afterwards.
#[test]
fn one_engine_may_stop_another_from_its_sink() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let (engine_b, log_b) = start(dir_b.path());
    let slot = Arc::new(Mutex::new(Some(engine_b)));
    let done = Arc::new(AtomicBool::new(false));
    let (s, d, lb) = (slot.clone(), done.clone(), log_b.clone());
    let log_a = Arc::new(Log::default());
    let record = log_a.sink();
    let sink: EventSink = Arc::new(move |e: Event| {
        if matches!(e, Event::Reply { id: 1, .. })
            && let Some(b) = s.lock().unwrap().take()
        {
            b.stop();
            lb.after_stop.store(true, Ordering::SeqCst);
            d.store(true, Ordering::SeqCst);
        }
        record(e);
    });
    let engine_a = Engine::start(config(dir_a.path()), sink).unwrap();
    engine_a.command_json(&cmd(1, r#"{"type":"get_settings"}"#));
    log_a.wait_reply(1);
    assert!(done.load(Ordering::SeqCst));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(log_b.late.load(Ordering::SeqCst), 0);
    engine_a.stop();
}

/// The fuzz target's function, on the worst shapes that fit in 64 KiB: it
/// returns, quickly, with a short message, and never panics.
#[test]
fn parse_command_is_cheap_on_hostile_input() {
    let cap = 64 * 1024;
    let fill = |unit: &str, open: &str, close: &str| {
        let mut s = String::from(open);
        while s.len() + unit.len() + close.len() <= cap {
            s.push_str(unit);
        }
        s.push_str(close);
        s
    };
    let inputs = [
        "[".repeat(cap),
        "{\"a\":".repeat(cap / 5),
        fill(
            "1,",
            r#"{"v":1,"id":1,"cmd":{"type":"send","items":["#,
            "1]}}",
        ),
        fill(r#""k":0,"#, r#"{"v":1,"id":2,"#, r#""x":0}"#),
        fill(r"\u0000", r#"{"v":1,"id":3,"cmd":{"type":""#, r#""}}"#),
        fill("a", r#"{"v":1,"id":4,"cmd":{"type":""#, r#""}}"#),
        fill(
            r#"{"kind":"text","text":""},"#,
            r#"{"v":1,"id":5,"cmd":{"type":"send","target":{"protocol":"wormhole"},"items":["#,
            r#"{"kind":"text","text":""}]}}"#,
        ),
        fill(r#""id":1,"#, "{", r#""v":1}"#),
        fill(
            "\u{202E}",
            r#"{"v":1,"id":6,"cmd":{"type":"receive_wormhole","code":""#,
            r#""}}"#,
        ),
    ];
    let started = Instant::now();
    for input in &inputs {
        assert!(input.len() <= cap, "{}", input.len());
        let t = Instant::now();
        let result = parse_command(input);
        assert!(t.elapsed() < Duration::from_secs(2), "{:.60}", input);
        if let Err((_, e)) = result {
            let info: sukkula_engine::api::ErrorInfo = e.into();
            assert!(info.message.chars().count() <= MAX_ERROR_MESSAGE_CHARS);
        }
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any string: no panic, and a failure's message is short and plain.
    #[test]
    fn parse_command_never_panics(s in any::<String>()) {
        if let Err((_, e)) = parse_command(&s) {
            let info: sukkula_engine::api::ErrorInfo = e.into();
            prop_assert!(info.message.chars().count() <= MAX_ERROR_MESSAGE_CHARS);
            prop_assert!(!info.message.chars().any(char::is_control));
        }
    }

    /// Commands built from the grammar, with hostile strings where strings
    /// go: they parse or fail, and an id is recovered when there is one.
    #[test]
    fn near_commands_recover_their_id(
        id in any::<u64>(),
        kind in prop::sample::select(vec![
            "get_settings", "set_receiving", "answer", "cancel", "send",
            "receive_wormhole", "list_bluetooth_devices", "self_destruct",
        ]),
        junk in ".{0,64}",
    ) {
        let junk = serde_json::to_string(&junk).unwrap();
        let body = format!(r#"{{"v":1,"id":{id},"cmd":{{"type":"{kind}","x":{junk}}}}}"#);
        match parse_command(&body) {
            Ok(env) => prop_assert_eq!(env.id, id),
            Err((got, _)) => prop_assert_eq!(got, Some(id)),
        }
    }
}
