//! A shell stand-in for the FFI tests: calls the C ABI the way C does, and
//! checks the header's promises inside the callback, on every event.
//!
//! The callback records a violation when it
//! - runs on a thread that is inside a `sukkula_*` call made through the
//!   wrappers here (the header: "never on the thread that called into the
//!   engine");
//! - runs while another call of it is still running (one at a time);
//! - runs after `sukkula_stop` returned for its engine;
//! - is handed anything but NUL-terminated UTF-8 JSON.

#![allow(
    unsafe_code, // The C ABI, called as C calls it.
    dead_code,   // Each test binary uses part of this.
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use serde_json::Value;
use sukkula_ffi::{SukkulaEngine, sukkula_command, sukkula_start, sukkula_stop};

thread_local! {
    static IN_CALL: Cell<bool> = const { Cell::new(false) };
}

/// What the callback does besides recording, e.g. send a command.
pub type Hook = Box<dyn Fn(&Recorder, &Value) + Send + Sync>;

/// One shell: the `userdata` of one engine.
pub struct Recorder {
    pub events: Mutex<Vec<(Value, ThreadId)>>,
    changed: Condvar,
    pub violations: Mutex<Vec<String>>,
    busy: AtomicUsize,
    /// Set by the test once `sukkula_stop` has returned.
    pub stopped: AtomicBool,
    /// The engine, for hooks that call back in.
    pub engine: AtomicPtr<SukkulaEngine>,
    pub hook: Mutex<Option<Hook>>,
    /// While set, the callback waits until it is cleared.
    pub hold: Mutex<bool>,
    hold_changed: Condvar,
}

impl Recorder {
    pub fn new() -> Arc<Recorder> {
        Arc::new(Recorder {
            events: Mutex::new(Vec::new()),
            changed: Condvar::new(),
            violations: Mutex::new(Vec::new()),
            busy: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            engine: AtomicPtr::new(std::ptr::null_mut()),
            hook: Mutex::new(None),
            hold: Mutex::new(false),
            hold_changed: Condvar::new(),
        })
    }

    pub fn userdata(self: &Arc<Self>) -> *mut c_void {
        Arc::as_ptr(self).cast_mut().cast()
    }

    pub fn violation(&self, what: String) {
        self.violations.lock().unwrap().push(what);
    }

    pub fn assert_clean(&self) {
        let v = self.violations.lock().unwrap();
        assert!(v.is_empty(), "violations: {v:?}");
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn types(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(e, _)| e["type"].as_str().unwrap_or("?").to_owned())
            .collect()
    }

    /// Waits until `f` holds for the events.
    pub fn wait_for(&self, what: &str, f: impl Fn(&[(Value, ThreadId)]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut events = self.events.lock().unwrap();
        while !f(&events) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}: {events:?}");
            events = self.changed.wait_timeout(events, left).unwrap().0;
        }
    }

    pub fn replies(&self, id: u64) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, _)| e["type"] == "reply" && e["id"] == id)
            .map(|(e, _)| e.clone())
            .collect()
    }

    pub fn wait_reply(&self, id: u64) -> Value {
        self.wait_for(&format!("reply {id}"), |events| {
            events
                .iter()
                .any(|(e, _)| e["type"] == "reply" && e["id"] == id)
        });
        self.replies(id).remove(0)
    }

    pub fn set_hold(&self, on: bool) {
        *self.hold.lock().unwrap() = on;
        self.hold_changed.notify_all();
    }

    fn wait_hold(&self) {
        let mut held = self.hold.lock().unwrap();
        while *held {
            held = self.hold_changed.wait(held).unwrap();
        }
    }
}

/// The callback every test engine gets.
pub unsafe extern "C" fn on_event(json: *const c_char, userdata: *mut c_void) {
    // SAFETY: `userdata` is the `Arc<Recorder>` the test keeps alive until
    // after `sukkula_stop`.
    let r = unsafe { &*userdata.cast_const().cast::<Recorder>() };
    if r.busy.fetch_add(1, Ordering::SeqCst) != 0 {
        r.violation("two callbacks at once".into());
    }
    if r.stopped.load(Ordering::SeqCst) {
        r.violation("called after sukkula_stop returned".into());
    }
    if IN_CALL.with(Cell::get) {
        r.violation("called on a thread inside a sukkula_* call".into());
    }
    if json.is_null() {
        r.violation("NULL event".into());
    } else {
        // SAFETY: the engine promises a NUL-terminated string, valid now.
        let bytes = unsafe { CStr::from_ptr(json) }.to_bytes();
        // Copy it, as a shell must: the pointer dies when we return.
        match std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        {
            Some(v) => {
                r.wait_hold();
                if let Some(hook) = r.hook.lock().unwrap().as_ref() {
                    hook(r, &v);
                }
                r.events
                    .lock()
                    .unwrap()
                    .push((v, std::thread::current().id()));
                r.changed.notify_all();
            }
            None => r.violation(format!("not UTF-8 JSON: {bytes:?}")),
        }
    }
    r.busy.fetch_sub(1, Ordering::SeqCst);
}

fn in_call<T>(f: impl FnOnce() -> T) -> T {
    let was = IN_CALL.with(|c| c.replace(true));
    let out = f();
    IN_CALL.with(|c| c.set(was));
    out
}

pub fn start_raw(config: Option<&[u8]>, r: &Arc<Recorder>) -> *mut SukkulaEngine {
    let c = config.map(|b| CString::new(b).unwrap());
    let p = c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    // SAFETY: `p` is NULL or a C string alive for the call; `on_event` has
    // the declared type; the recorder outlives the engine.
    let h = in_call(|| unsafe { sukkula_start(p, Some(on_event), r.userdata()) });
    r.engine.store(h, Ordering::SeqCst);
    h
}

pub fn config_for(dir: &Path) -> String {
    serde_json::json!({
        "v": 1,
        "data_dir": dir.join("data"),
        "download_dir": dir.join("dl"),
        "device_model": "Test Phone",
    })
    .to_string()
}

pub fn start(dir: &Path, r: &Arc<Recorder>) -> *mut SukkulaEngine {
    let h = start_raw(Some(config_for(dir).as_bytes()), r);
    assert!(!h.is_null(), "start failed: {:?}", r.types());
    h
}

pub fn command_raw(h: *mut SukkulaEngine, json: Option<&[u8]>) -> i32 {
    let c = json.map(|b| CString::new(b).unwrap());
    let p = c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    // SAFETY: `p` is NULL or a C string alive for the call.
    in_call(|| unsafe { sukkula_command(h, p) })
}

pub fn command(h: *mut SukkulaEngine, json: &str) -> i32 {
    command_raw(h, Some(json.as_bytes()))
}

pub fn stop(h: *mut SukkulaEngine, r: &Recorder) {
    in_call(|| sukkula_stop(h));
    r.stopped.store(true, Ordering::SeqCst);
}
