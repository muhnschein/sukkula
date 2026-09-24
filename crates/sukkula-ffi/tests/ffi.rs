//! The C ABI, driven the way the Qt shell drives it: every function, every
//! return code, every failure mode the header names, and its threading
//! promises checked inside the callback on every event (see `common`).

#![allow(
    unsafe_code, // The C ABI, called as C calls it.
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation
)]

mod common;

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::{Recorder, command, command_raw, config_for, start, start_raw, stop};
use serde_json::{Value, json};
use sukkula_ffi::{
    MAX_IN_FLIGHT_COMMANDS, MAX_MESSAGE_BYTES, SUKKULA_ERR_BUSY, SUKKULA_ERR_NULL,
    SUKKULA_ERR_TOO_LONG, SUKKULA_ERR_UTF8, SUKKULA_OK, SukkulaEngine, sukkula_start, sukkula_stop,
    sukkula_version,
};

fn get_settings(id: u64) -> String {
    format!(r#"{{"v":1,"id":{id},"cmd":{{"type":"get_settings"}}}}"#)
}

#[test]
fn the_version_is_a_static_semver_string() {
    let a = sukkula_version();
    let b = sukkula_version();
    assert_eq!(a, b, "the same static string every time");
    // SAFETY: a static C string.
    let v = unsafe { CStr::from_ptr(a) }.to_str().unwrap();
    assert_eq!(v, sukkula_engine::VERSION);
    assert_eq!(v.split('.').count(), 3);
    assert!(v.split('.').all(|p| p.parse::<u32>().is_ok()), "{v}");
}

#[test]
fn a_null_callback_is_refused_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let config = std::ffi::CString::new(config_for(dir.path())).unwrap();
    // SAFETY: a valid config; the NULL callback is what is tested.
    let h = unsafe { sukkula_start(config.as_ptr(), None, std::ptr::null_mut()) };
    assert!(h.is_null());
    assert!(!dir.path().join("data").exists(), "nothing was started");
}

#[test]
fn every_failed_start_emits_one_fatal_event() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_string_lossy().into_owned();
    let long = format!(
        r#"{{"v":1,"data_dir":"{d}/data","download_dir":"{d}/dl","device_model":"{}"}}"#,
        "x".repeat(MAX_MESSAGE_BYTES)
    );
    let cases: Vec<(Option<Vec<u8>>, &str)> = vec![
        (None, "bad_command"),
        (Some(b"{\"v\":1,\xff}".to_vec()), "bad_command"),
        (Some(long.into_bytes()), "bad_command"),
        (Some(b"not json".to_vec()), "bad_command"),
        (Some(b"{}".to_vec()), "bad_command"),
        (
            Some(
                format!(r#"{{"v":1,"data_dir":"{d}/a","download_dir":"{d}/b","x":1}}"#)
                    .into_bytes(),
            ),
            "bad_command",
        ),
        (
            Some(format!(r#"{{"v":2,"data_dir":"{d}/a","download_dir":"{d}/b"}}"#).into_bytes()),
            "bad_version",
        ),
        (
            Some(br#"{"v":1,"data_dir":"rel/a","download_dir":"/tmp/b"}"#.to_vec()),
            "bad_command",
        ),
        (
            Some(format!(r#"{{"v":1,"data_dir":"{d}/a","download_dir":"{d}/a"}}"#).into_bytes()),
            "bad_command",
        ),
        (
            Some(format!(r#"{{"v":1,"data_dir":"{d}/a","download_dir":"{d}/a/dl"}}"#).into_bytes()),
            "bad_command",
        ),
    ];
    for (config, code) in cases {
        let r = Recorder::new();
        let h = start_raw(config.as_deref(), &r);
        assert!(h.is_null(), "{:?} started", config.map(String::from_utf8));
        // The event came before sukkula_start returned.
        assert_eq!(r.types(), vec!["fatal"], "{config:?}");
        let events = r.events.lock().unwrap();
        assert_eq!(events[0].0["error"]["code"], code, "{:?}", events[0].0);
        assert_ne!(events[0].1, std::thread::current().id());
        drop(events);
        r.assert_clean();
    }
}

#[test]
fn start_emits_started_settings_receiving_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);
    assert_eq!(r.types(), vec!["started", "settings", "receiving"]);
    {
        let events = r.events.lock().unwrap();
        assert_eq!(events[0].0["api"], 1);
        assert_eq!(events[0].0["version"], sukkula_engine::VERSION);
        assert_eq!(events[1].0["effective_device_name"], "Test Phone");
        assert_eq!(events[2].0["on"], false);
        let delivery = events[0].1;
        assert_ne!(delivery, std::thread::current().id());
        assert!(events.iter().all(|(_, t)| *t == delivery), "one thread");
    }
    stop(h, &r);
    r.assert_clean();
}

#[test]
fn command_return_codes() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);

    assert_eq!(
        command(std::ptr::null_mut(), &get_settings(1)),
        SUKKULA_ERR_NULL
    );
    assert_eq!(command_raw(h, None), SUKKULA_ERR_NULL);
    assert_eq!(
        command_raw(h, Some(b"{\"v\":1,\"id\":2,\xc3\x28}")),
        SUKKULA_ERR_UTF8
    );
    let over = "x".repeat(MAX_MESSAGE_BYTES + 1);
    assert_eq!(command(h, &over), SUKKULA_ERR_TOO_LONG);
    // Not a handle this library gave out.
    let garbage = std::ptr::without_provenance_mut::<SukkulaEngine>(0xdead_bee0);
    assert_eq!(command(garbage, &get_settings(3)), SUKKULA_ERR_NULL);
    let odd = std::ptr::without_provenance_mut::<SukkulaEngine>(h.addr() + 1);
    assert_eq!(command(odd, &get_settings(3)), SUKKULA_ERR_NULL);

    // Exactly 64 KiB is taken, and answered even though it is nonsense.
    let head = r#"{"v":1,"id":4,"pad":""#;
    let tail = r#""}"#;
    let exact = format!(
        "{head}{}{tail}",
        "a".repeat(MAX_MESSAGE_BYTES - head.len() - tail.len())
    );
    assert_eq!(exact.len(), MAX_MESSAGE_BYTES);
    assert_eq!(command(h, &exact), SUKKULA_OK);
    let reply = r.wait_reply(4);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["code"], "bad_command");
    assert!(reply["error"]["message"].as_str().unwrap().len() < 1100);

    // Malformed, with and without an id; another version.
    assert_eq!(command(h, "nonsense"), SUKKULA_OK);
    assert_eq!(r.wait_reply(0)["error"]["code"], "bad_command");
    assert_eq!(
        command(h, r#"{"v":1,"id":5,"cmd":{"type":"self_destruct"}}"#),
        SUKKULA_OK
    );
    assert_eq!(r.wait_reply(5)["error"]["code"], "bad_command");
    assert_eq!(
        command(h, r#"{"v":9,"id":6,"cmd":{"type":"get_settings"}}"#),
        SUKKULA_OK
    );
    assert_eq!(r.wait_reply(6)["error"]["code"], "bad_version");

    // A good one: its settings event comes first, then the reply.
    assert_eq!(command(h, &get_settings(7)), SUKKULA_OK);
    assert_eq!(r.wait_reply(7)["ok"], true);
    let types = r.types();
    let at = types.len() - 1;
    assert_eq!(&types[at - 1..], ["settings", "reply"]);

    std::thread::sleep(Duration::from_millis(50));
    for id in [0, 4, 5, 6, 7] {
        assert_eq!(r.replies(id).len(), 1, "exactly one reply to {id}");
    }
    assert!(r.replies(1).is_empty() && r.replies(2).is_empty() && r.replies(3).is_empty());
    stop(h, &r);
    r.assert_clean();
}

#[test]
fn stopped_and_stale_handles_are_harmless() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);
    stop(h, &r);
    let n = r.len();
    assert_eq!(command(h, &get_settings(1)), SUKKULA_ERR_NULL);
    sukkula_stop(h);
    sukkula_stop(std::ptr::null_mut());
    sukkula_stop(std::ptr::without_provenance_mut(0x10));
    sukkula_stop(std::ptr::without_provenance_mut(0x13));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(r.len(), n);
    r.assert_clean();
}

#[test]
fn commands_from_many_threads_get_exactly_one_reply_each() {
    const THREADS: u64 = 8;
    const EACH: u64 = 150;
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);
    let handle = h.addr();
    let barrier = Arc::new(Barrier::new(THREADS as usize));
    let workers: Vec<_> = (0..THREADS)
        .map(|t| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let h = std::ptr::without_provenance_mut::<SukkulaEngine>(handle);
                barrier.wait();
                let mut taken = Vec::new();
                let mut busy = 0u32;
                for n in 0..EACH {
                    let id = 1000 + t * EACH + n;
                    let json = match n % 4 {
                        0 => get_settings(id),
                        1 => format!(r#"{{"v":1,"id":{id},"cmd":{{"type":"nope"}}}}"#),
                        2 => format!(
                            r#"{{"v":1,"id":{id},"cmd":{{"type":"cancel","transfer":{n}}}}}"#
                        ),
                        _ => format!(
                            r#"{{"v":1,"id":{id},"cmd":{{"type":"answer","offer":{n},"accept":true}}}}"#
                        ),
                    };
                    match command(h, &json) {
                        SUKKULA_OK => taken.push(id),
                        SUKKULA_ERR_BUSY => busy += 1,
                        other => panic!("{other}"),
                    }
                }
                (taken, busy)
            })
        })
        .collect();
    let mut taken = Vec::new();
    for w in workers {
        let (t, _busy) = w.join().unwrap();
        taken.extend(t);
    }
    assert!(!taken.is_empty());
    let want = taken.len();
    r.wait_for("every reply", |events| {
        events.iter().filter(|(e, _)| e["type"] == "reply").count() >= want
    });
    std::thread::sleep(Duration::from_millis(50));
    let mut per_id: HashMap<u64, usize> = HashMap::new();
    for (e, _) in r.events.lock().unwrap().iter() {
        if e["type"] == "reply" {
            *per_id.entry(e["id"].as_u64().unwrap()).or_default() += 1;
        }
    }
    assert_eq!(
        per_id.len(),
        want,
        "a reply for each taken command, no more"
    );
    for id in &taken {
        assert_eq!(per_id.get(id), Some(&1), "reply to {id}");
    }
    stop(h, &r);
    r.assert_clean();
}

#[test]
fn a_backed_up_ui_gets_busy_instead_of_a_growing_queue() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);
    r.set_hold(true);
    let mut taken = 0;
    for id in 0..MAX_IN_FLIGHT_COMMANDS as u64 {
        assert_eq!(command(h, &get_settings(id)), SUKKULA_OK, "{id}");
        taken += 1;
    }
    // Every reply is stuck behind the held callback, so every slot is
    // taken; the next command is refused, and parsed not at all.
    assert_eq!(command(h, &get_settings(9999)), SUKKULA_ERR_BUSY);
    assert_eq!(command(h, "not even json"), SUKKULA_ERR_BUSY);
    r.set_hold(false);
    r.wait_for("every reply", |events| {
        events.iter().filter(|(e, _)| e["type"] == "reply").count() == taken
    });
    // Slots come back as replies are delivered.
    let mut ok = false;
    for _ in 0..200 {
        if command(h, &get_settings(10_000)) == SUKKULA_OK {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(ok);
    r.wait_reply(10_000);
    assert!(r.replies(9999).is_empty());
    stop(h, &r);
    r.assert_clean();
}

#[test]
fn the_callback_may_send_commands() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    *r.hook.lock().unwrap() = Some(Box::new(|r: &Recorder, e: &Value| {
        if e["type"] == "reply" && e["id"] == 1 {
            let h = r.engine.load(Ordering::SeqCst);
            let rc = command(h, &get_settings(2));
            if rc != SUKKULA_OK {
                r.violation(format!("command from the callback: {rc}"));
            }
        }
    }));
    let h = start(dir.path(), &r);
    assert_eq!(command(h, &get_settings(1)), SUKKULA_OK);
    assert_eq!(r.wait_reply(2)["ok"], true);
    stop(h, &r);
    r.assert_clean();
}

#[test]
fn stop_from_inside_the_callback_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    *r.hook.lock().unwrap() = Some(Box::new(move |r: &Recorder, e: &Value| {
        if e["type"] == "reply" && e["id"] == 1 {
            let h = r.engine.load(Ordering::SeqCst);
            // The header says not to; it must still neither deadlock nor
            // call back once it has returned.
            stop(h, r);
            if command(h, &get_settings(3)) != SUKKULA_ERR_NULL {
                r.violation("the handle outlived its stop".into());
            }
            d.store(true, Ordering::SeqCst);
        }
    }));
    let h = start(dir.path(), &r);
    // Queue a few so there is usually something the stop has to drop. The
    // reply to 1 may already have stopped the engine by the time the later
    // ones go in, and then they are refused: both are right.
    assert_eq!(command(h, &get_settings(1)), SUKKULA_OK);
    for id in 2..=5 {
        let rc = command(h, &get_settings(id));
        assert!(rc == SUKKULA_OK || rc == SUKKULA_ERR_NULL, "{rc}");
    }
    for _ in 0..2000 {
        if done.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        done.load(Ordering::SeqCst),
        "stop from the callback returned"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(command(h, &get_settings(9)), SUKKULA_ERR_NULL);
    sukkula_stop(h);
    r.assert_clean();
}

#[test]
fn hammering_commands_while_stopping_is_safe() {
    for round in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::new();
        let h = start(dir.path(), &r);
        let handle = h.addr();
        let stopped = Arc::new(AtomicBool::new(false));
        let quit = Arc::new(AtomicBool::new(false));
        let late = Arc::new(AtomicU64::new(0));
        let next = Arc::new(AtomicU64::new(1));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let (stopped, quit, late, next) =
                    (stopped.clone(), quit.clone(), late.clone(), next.clone());
                std::thread::spawn(move || {
                    let h = std::ptr::without_provenance_mut::<SukkulaEngine>(handle);
                    while !quit.load(Ordering::SeqCst) {
                        let was_stopped = stopped.load(Ordering::SeqCst);
                        let id = next.fetch_add(1, Ordering::SeqCst);
                        let rc = command(h, &get_settings(id));
                        if was_stopped && rc != SUKKULA_ERR_NULL {
                            late.fetch_add(1, Ordering::SeqCst);
                        }
                        assert!(
                            [SUKKULA_OK, SUKKULA_ERR_BUSY, SUKKULA_ERR_NULL].contains(&rc),
                            "{rc}"
                        );
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(20 + round * 10));
        stop(h, &r);
        stopped.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        quit.store(true, Ordering::SeqCst);
        for w in workers {
            w.join().unwrap();
        }
        let n = r.len();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(r.len(), n);
        assert_eq!(late.load(Ordering::SeqCst), 0, "a stale handle was taken");
        assert!(next.load(Ordering::SeqCst) > 10, "the workers did run");
        r.assert_clean();
    }
}

#[test]
fn stop_waits_for_a_callback_in_progress() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);
    let inside = Arc::new(AtomicBool::new(false));
    let left = Arc::new(AtomicBool::new(false));
    let (i, l) = (inside.clone(), left.clone());
    *r.hook.lock().unwrap() = Some(Box::new(move |_: &Recorder, e: &Value| {
        if e["type"] == "reply" {
            i.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            l.store(true, Ordering::SeqCst);
        }
    }));
    assert_eq!(command(h, &get_settings(1)), SUKKULA_OK);
    while !inside.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    stop(h, &r);
    // Only now may the shell free `userdata`.
    assert!(left.load(Ordering::SeqCst), "stop returned mid-callback");
    r.assert_clean();
}

#[test]
fn events_are_json_objects_that_round_trip_into_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);
    let set = json!({"v":1,"id":1,"cmd":{"type":"set_settings","settings":{"device_name":"Pekka\u{202e}\u{0}"}}});
    assert_eq!(command(h, &set.to_string()), SUKKULA_OK);
    assert_eq!(r.wait_reply(1)["ok"], true);
    stop(h, &r);
    for (e, _) in r.events.lock().unwrap().iter() {
        let typed: sukkula_engine::api::Event = serde_json::from_value(e.clone()).unwrap();
        assert_eq!(&serde_json::to_value(&typed).unwrap(), e);
    }
    let names: Vec<String> = r
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|(e, _)| e["type"] == "settings")
        .map(|(e, _)| e["effective_device_name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.last().unwrap(), "Pekka", "S2 applied to the name");
    r.assert_clean();
}
