//! The C ABI the Qt shell links (`include/sukkula.h`): four functions,
//! JSON in and out, and the only `unsafe` code in Sukkula (S10).
//!
//! # What is unsafe here, and why it is sound
//!
//! Exactly three things, each commented where it happens:
//!
//! 1. Exporting unmangled symbols (`#[unsafe(no_mangle)]`).
//! 2. Reading a C string the shell passed in ([`read_c_str`]): at most
//!    [`MAX_MESSAGE_BYTES`] + 1 bytes, one at a time, never past its NUL.
//! 3. Calling the shell's callback ([`Callback::deliver`]) with a string
//!    that lives until the call returns.
//!
//! Engine handles are never dereferenced. `sukkula_start` hands out an
//! opaque number dressed as a pointer, and every other function looks it
//! up in a registry. A handle that is NULL, stale (already stopped), or
//! garbage finds nothing and is refused like NULL: `sukkula_command` on a
//! stopped engine, `sukkula_stop` twice, and commands racing a stop from
//! other threads are all harmless rather than use-after-free.
//!
//! # Panics
//!
//! Every entry point runs inside `catch_unwind`, so no Rust panic crosses
//! into C (where unwinding is undefined behaviour and `extern "C"` would
//! abort the app). The engine catches panics in its own tasks as well; what
//! reaches this layer is reported as `SUKKULA_ERR_PANIC`, a `fatal` event,
//! or nothing, as the header says.
//!
//! # Threads
//!
//! The engine calls the sink on its delivery thread only, one event at a
//! time, never on a thread that called in, and never after
//! `sukkula_stop` returns (see `sukkula_engine`'s hub). The one callback
//! this layer makes itself, a failed start's `fatal` event, is made on a
//! short-lived thread for the same reason.

#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::{PhantomData, PhantomPinned};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use sukkula_engine::api::{ErrorCode, ErrorInfo, Event, parse_start_config};
use sukkula_engine::ctx::EventSink;
use sukkula_engine::{Engine, Refused};

/// The largest event the callback is handed, in bytes, not counting the
/// NUL.
pub use sukkula_core::limits::MAX_EVENT_BYTES;
/// The largest command or configuration accepted, in bytes, not counting
/// the NUL (S6).
pub use sukkula_core::limits::MAX_MESSAGE_BYTES;
/// Most commands waiting for their reply; one more is `SUKKULA_ERR_BUSY`.
pub use sukkula_engine::api::MAX_IN_FLIGHT_COMMANDS;

/// Taken; its `reply` event follows.
pub const SUKKULA_OK: i32 = 0;
/// The engine or the command was NULL, or the engine has been stopped.
pub const SUKKULA_ERR_NULL: i32 = -1;
/// The command is not UTF-8.
pub const SUKKULA_ERR_UTF8: i32 = -2;
/// The command is over [`MAX_MESSAGE_BYTES`]; nothing was parsed.
pub const SUKKULA_ERR_TOO_LONG: i32 = -3;
/// The engine failed internally.
pub const SUKKULA_ERR_PANIC: i32 = -4;
/// [`MAX_IN_FLIGHT_COMMANDS`] commands are waiting for their replies;
/// nothing was parsed.
pub const SUKKULA_ERR_BUSY: i32 = -5;

/// An opaque running engine. Never constructed: a handle is a number that
/// only this crate's registry understands.
#[repr(C)]
pub struct SukkulaEngine {
    _opaque: [u8; 0],
    _not_send_sync_unpin: PhantomData<(*mut u8, PhantomPinned)>,
}

/// The callback: one event, as NUL-terminated UTF-8 JSON valid only during
/// the call, and the `userdata` given to `sukkula_start`. `Option` because
/// C may pass NULL, which a bare Rust function pointer cannot be.
pub type SukkulaEventCb =
    Option<unsafe extern "C" fn(event_json: *const c_char, userdata: *mut c_void)>;

/// The shell's `userdata`, handed back verbatim and never dereferenced.
#[derive(Clone, Copy)]
struct UserData(*mut c_void);

// SAFETY: Rust never reads or writes through this pointer, it only passes
// the value back to the shell's callback. Sending that value to the
// delivery thread (Send) and sharing it (Sync, though only one thread ever
// uses it at a time) is therefore sound as far as Rust is concerned. What
// it points to is the shell's business: the header tells the shell that
// the callback runs on an engine thread, so by passing `userdata` the
// shell asserts that the callback may use it from there.
#[allow(unsafe_code)]
unsafe impl Send for UserData {}
// SAFETY: as for `Send` above.
#[allow(unsafe_code)]
unsafe impl Sync for UserData {}

/// The shell's callback and its `userdata`.
struct Callback {
    f: unsafe extern "C" fn(*const c_char, *mut c_void),
    userdata: UserData,
}

impl Callback {
    /// Serialises `event` and hands it to the shell.
    fn deliver(&self, event: &Event) {
        let json = match serde_json::to_string(event) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(error = %e, "an event did not serialise");
                return;
            }
        };
        // The engine holds events to this already; a second look costs a
        // comparison.
        if json.len() > MAX_EVENT_BYTES {
            tracing::error!(bytes = json.len(), "an event over the size cap");
            return;
        }
        // JSON cannot contain a raw NUL: serde_json writes U+0000 in a
        // string as `\u0000`. So this does not fail, and if it ever did the
        // event would be dropped whole rather than cut short at the NUL.
        let Ok(json) = CString::new(json) else {
            tracing::error!("an event contained a NUL byte");
            return;
        };
        // SAFETY: `f` is the non-NULL function pointer the shell passed to
        // `sukkula_start`, of the type the header declares. `json` is a
        // NUL-terminated UTF-8 string that outlives the call, as the header
        // promises ("valid only during the call"). `userdata` is the
        // shell's own value, passed back unchanged. The callback must not
        // unwind (the header says so); a C++ exception escaping it would be
        // undefined behaviour no Rust code can prevent.
        #[allow(unsafe_code)]
        unsafe {
            (self.f)(json.as_ptr(), self.userdata.0);
        }
    }

    /// Delivers a failed start's `fatal` event on a thread of its own, so
    /// that the callback never runs on the thread that called in. If no
    /// thread can be had, delivers it here: one event on the wrong thread
    /// beats the header's promise of one `fatal` event broken.
    fn deliver_fatal(self: &Arc<Self>, error: ErrorInfo) {
        let event = Event::Fatal { error };
        let cb = self.clone();
        let spare = event.clone();
        match std::thread::Builder::new()
            .name("sukkula-fatal".to_owned())
            .spawn(move || cb.deliver(&event))
        {
            Ok(thread) => {
                let _ = thread.join();
            }
            Err(_) => self.deliver(&spare),
        }
    }
}

/// A running engine. The lock is read-held for each command and
/// write-held only to take the engine out at stop, so a stop waits out the
/// commands already inside and the ones after it find nothing.
struct Live {
    engine: RwLock<Option<Engine>>,
}

struct Registry {
    next: u64,
    live: BTreeMap<u64, Arc<Live>>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    next: 1,
    live: BTreeMap::new(),
});

/// Handles are ids times this: never NULL, aligned like a real pointer so
/// they look like one in a debugger, and an odd garbage value is refused
/// without a lookup.
const HANDLE_STRIDE: usize = 16;

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

fn register(engine: Engine) -> *mut SukkulaEngine {
    let live = Arc::new(Live {
        engine: RwLock::new(Some(engine)),
    });
    let mut reg = registry();
    let id = reg.next;
    reg.next = reg.next.saturating_add(1);
    let handle = handle_for(id);
    if !handle.is_null() {
        reg.live.insert(id, live);
    }
    // A NULL handle (an id past what a pointer holds, which 2^60 starts
    // will not reach) drops `live` here, which stops the engine.
    handle
}

fn handle_for(id: u64) -> *mut SukkulaEngine {
    usize::try_from(id)
        .ok()
        .and_then(|n| n.checked_mul(HANDLE_STRIDE))
        .map_or(ptr::null_mut(), ptr::without_provenance_mut)
}

fn id_of(handle: *mut SukkulaEngine) -> Option<u64> {
    let addr = handle.addr();
    if addr == 0 || addr.checked_rem(HANDLE_STRIDE) != Some(0) {
        return None;
    }
    addr.checked_div(HANDLE_STRIDE)
        .and_then(|n| u64::try_from(n).ok())
}

fn lookup(handle: *mut SukkulaEngine) -> Option<Arc<Live>> {
    let id = id_of(handle)?;
    registry().live.get(&id).cloned()
}

/// Why a C string was refused.
#[derive(Debug, PartialEq, Eq)]
enum CStrError {
    Null,
    TooLong,
    Utf8,
}

/// Reads a NUL-terminated string of at most [`MAX_MESSAGE_BYTES`] bytes.
///
/// Scans one byte at a time and stops at the NUL, so it never reads a byte
/// beyond the string, however short; and it gives up after
/// [`MAX_MESSAGE_BYTES`] + 1 bytes, so a huge or unterminated buffer costs
/// no more than that.
///
/// # Safety
///
/// `p` is NULL, or points to a NUL-terminated string that stays readable
/// and unchanged until the returned `&str` is dropped.
#[allow(unsafe_code)]
unsafe fn read_c_str<'a>(p: *const c_char) -> Result<&'a str, CStrError> {
    if p.is_null() {
        return Err(CStrError::Null);
    }
    let mut len: usize = 0;
    loop {
        if len > MAX_MESSAGE_BYTES {
            return Err(CStrError::TooLong);
        }
        // SAFETY: the `len` bytes before this one were all non-NUL, so this
        // byte is still part of the string, at worst its terminator, which
        // the caller promises is readable. `len` is at most 64 KiB, so the
        // offset cannot overflow.
        let byte = unsafe { p.add(len).read() };
        if byte == 0 {
            break;
        }
        len = len.saturating_add(1);
    }
    // SAFETY: the `len` bytes at `p` were just read one by one, so they are
    // readable, and the caller promises they stay unchanged for 'a. `len`
    // is far below `isize::MAX`.
    let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) };
    std::str::from_utf8(bytes).map_err(|_| CStrError::Utf8)
}

/// Starts an engine. See `include/sukkula.h`.
///
/// # Safety
///
/// `config_json` is NULL or a NUL-terminated string, readable and
/// unchanged for the duration of the call. `callback` is NULL or a
/// function of the declared type that does not unwind. `userdata` is
/// anything; it is passed back verbatim.
#[unsafe(no_mangle)]
#[allow(unsafe_code)] // The exported symbol.
pub unsafe extern "C" fn sukkula_start(
    config_json: *const c_char,
    callback: SukkulaEventCb,
    userdata: *mut c_void,
) -> *mut SukkulaEngine {
    let Some(f) = callback else {
        // Nobody to tell why.
        return ptr::null_mut();
    };
    let cb = Arc::new(Callback {
        f,
        userdata: UserData(userdata),
    });
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller's promise about `config_json`, passed on.
        let json = unsafe { read_c_str(config_json) }.map_err(|e| {
            let message = match e {
                CStrError::Null => "config_json is NULL",
                CStrError::TooLong => "config_json is over 64 KiB",
                CStrError::Utf8 => "config_json is not UTF-8",
            };
            ErrorInfo::new(ErrorCode::BadCommand, message)
        })?;
        let config = parse_start_config(json).map_err(ErrorInfo::from)?;
        let events = cb.clone();
        let sink: EventSink = Arc::new(move |event| events.deliver(&event));
        let engine = Engine::start(config, sink)?;
        Ok::<_, ErrorInfo>(register(engine))
    }));
    let failure = match outcome {
        Ok(Ok(handle)) if !handle.is_null() => return handle,
        Ok(Ok(_)) => ErrorInfo::new(ErrorCode::Internal, "no handle left"),
        Ok(Err(e)) => e,
        Err(_) => ErrorInfo::new(ErrorCode::Internal, "the engine failed internally"),
    };
    // Even the report must not unwind into C.
    let _ = catch_unwind(AssertUnwindSafe(|| cb.deliver_fatal(failure)));
    ptr::null_mut()
}

/// Hands the engine one command. See `include/sukkula.h`.
///
/// # Safety
///
/// `command_json` is NULL or a NUL-terminated string, readable and
/// unchanged for the duration of the call. `engine` may be anything:
/// handles are looked up, never dereferenced.
#[unsafe(no_mangle)]
#[allow(unsafe_code)] // The exported symbol.
pub unsafe extern "C" fn sukkula_command(
    engine: *mut SukkulaEngine,
    command_json: *const c_char,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if engine.is_null() {
            return SUKKULA_ERR_NULL;
        }
        // SAFETY: the caller's promise about `command_json`, passed on.
        let json = match unsafe { read_c_str(command_json) } {
            Ok(json) => json,
            Err(CStrError::Null) => return SUKKULA_ERR_NULL,
            Err(CStrError::TooLong) => return SUKKULA_ERR_TOO_LONG,
            Err(CStrError::Utf8) => return SUKKULA_ERR_UTF8,
        };
        let Some(live) = lookup(engine) else {
            return SUKKULA_ERR_NULL;
        };
        let held = live.engine.read().unwrap_or_else(PoisonError::into_inner);
        let Some(engine) = held.as_ref() else {
            return SUKKULA_ERR_NULL;
        };
        match engine.try_command_json(json) {
            Ok(()) => SUKKULA_OK,
            Err(Refused::Busy) => SUKKULA_ERR_BUSY,
            Err(Refused::Stopped) => SUKKULA_ERR_NULL,
        }
    }))
    .unwrap_or(SUKKULA_ERR_PANIC)
}

/// Stops an engine and frees it. See `include/sukkula.h`.
///
/// Safe for any value: NULL, a stale handle and garbage are ignored.
#[unsafe(no_mangle)]
#[allow(unsafe_code)] // The exported symbol.
pub extern "C" fn sukkula_stop(engine: *mut SukkulaEngine) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(id) = id_of(engine) else {
            return;
        };
        // Out of the registry first: from here on no new command finds it.
        let Some(live) = registry().live.remove(&id) else {
            return;
        };
        // Waits out the commands already inside; they never block.
        let taken = live
            .engine
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(engine) = taken {
            // From inside the callback too: the engine sees that it is on
            // its own delivery thread and does not wait for itself.
            engine.stop();
        }
    }));
}

/// The version, e.g. `"0.1.0"`. A static string; never free it.
const VERSION: &CStr =
    match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(v) => v,
        Err(_) => c"unknown",
    };

/// The engine version. See `include/sukkula.h`.
#[unsafe(no_mangle)]
#[allow(unsafe_code)] // The exported symbol.
pub extern "C" fn sukkula_version() -> *const c_char {
    catch_unwind(|| VERSION.as_ptr()).unwrap_or(ptr::null())
}

#[cfg(test)]
#[allow(unsafe_code)] // Tests call the C ABI the way C does.
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn handles_round_trip_and_garbage_is_refused() {
        for id in [1u64, 2, 12345, u64::from(u32::MAX)] {
            let h = handle_for(id);
            assert!(!h.is_null());
            assert_eq!(id_of(h), Some(id));
        }
        assert_eq!(id_of(ptr::null_mut()), None);
        assert_eq!(id_of(ptr::without_provenance_mut(17)), None);
        assert_eq!(id_of(ptr::without_provenance_mut(3)), None);
        assert!(lookup(handle_for(u64::MAX >> 8)).is_none());
    }

    #[test]
    fn c_strings_are_read_within_bounds() {
        let ok = c"{\"a\":1}";
        // SAFETY: a valid C string.
        assert_eq!(unsafe { read_c_str(ok.as_ptr()) }, Ok("{\"a\":1}"));
        // SAFETY: NULL is allowed.
        assert_eq!(unsafe { read_c_str(ptr::null()) }, Err(CStrError::Null));
        let bad = CString::new(vec![b'a', 0xff, b'b']).unwrap();
        // SAFETY: a valid C string.
        assert_eq!(unsafe { read_c_str(bad.as_ptr()) }, Err(CStrError::Utf8));
        let exact = CString::new(vec![b'x'; MAX_MESSAGE_BYTES]).unwrap();
        // SAFETY: a valid C string.
        assert_eq!(
            unsafe { read_c_str(exact.as_ptr()) }.map(str::len),
            Ok(MAX_MESSAGE_BYTES)
        );
        let over = CString::new(vec![b'x'; MAX_MESSAGE_BYTES + 1]).unwrap();
        // SAFETY: a valid C string.
        assert_eq!(
            unsafe { read_c_str(over.as_ptr()) },
            Err(CStrError::TooLong)
        );
        // A buffer with no NUL at all, one byte longer than the cap: the
        // scan stops before running off its end.
        let unterminated = vec![b'y'; MAX_MESSAGE_BYTES + 1];
        // SAFETY: every byte read is inside the buffer: the scan reads at
        // most MAX_MESSAGE_BYTES + 1 bytes.
        assert_eq!(
            unsafe { read_c_str(unterminated.as_ptr().cast()) },
            Err(CStrError::TooLong)
        );
    }

    static SEEN: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

    unsafe extern "C" fn record(json: *const c_char, _userdata: *mut c_void) {
        // SAFETY: the engine passes a valid C string.
        let bytes = unsafe { CStr::from_ptr(json) }.to_bytes().to_vec();
        SEEN.lock().unwrap().push(bytes);
    }

    #[test]
    fn a_nul_in_an_event_is_escaped_not_cut() {
        let cb = Callback {
            f: record,
            userdata: UserData(ptr::null_mut()),
        };
        cb.deliver(&Event::Fatal {
            error: ErrorInfo {
                code: ErrorCode::Internal,
                message: "before\0after".to_owned(),
            },
        });
        let seen = SEEN.lock().unwrap().pop().unwrap();
        let json = String::from_utf8(seen).unwrap();
        assert!(json.contains(r"before\u0000after"), "{json}");
        match serde_json::from_str::<Event>(&json).unwrap() {
            Event::Fatal { error } => assert_eq!(error.message, "before\0after"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_version_is_the_engine_version() {
        // SAFETY: a static C string.
        let v = unsafe { CStr::from_ptr(sukkula_version()) };
        assert_eq!(v.to_str().unwrap(), sukkula_engine::VERSION);
    }
}
