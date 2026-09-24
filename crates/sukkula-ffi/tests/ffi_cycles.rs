//! 200 start/stop cycles through the C ABI must not leak memory, threads or
//! file descriptors.
//!
//! A binary of its own, with one test, so that nothing else runs while the
//! threads and descriptors of the process are counted; and with a counting
//! allocator, so that a leak of even a few bytes per cycle shows up as live
//! bytes that grow with the number of cycles.

#![allow(
    unsafe_code, // The C ABI, and a global allocator.
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_wrap
)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use common::{Recorder, command, start, stop};
use sukkula_ffi::SUKKULA_OK;

/// The system allocator, counting the bytes currently allocated.
struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

// SAFETY: every call is forwarded unchanged to the system allocator; the
// counter is only arithmetic on the side.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded with the caller's layout.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded; `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded; the caller upholds realloc's contract.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

fn fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

fn cycle(n: u64) {
    let dir = tempfile::tempdir().unwrap();
    let r = Recorder::new();
    let h = start(dir.path(), &r);
    let json = format!(r#"{{"v":1,"id":{n},"cmd":{{"type":"get_settings"}}}}"#);
    assert_eq!(command(h, &json), SUKKULA_OK);
    r.wait_reply(n);
    stop(h, &r);
    r.assert_clean();
}

/// Waits for counts that settle a moment after a stop (a thread's last
/// instructions after it has been joined, as the kernel sees them).
fn settled(what: &str, base: usize, f: fn() -> usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let now = f();
        if now <= base {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: {now} now, {base} before"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn two_hundred_start_stop_cycles_leak_nothing() {
    // Warm up: lazily initialised statics (the registry, tracing's callsite
    // cache, std's thread bookkeeping) are allocated once, not per cycle.
    for n in 0..5 {
        cycle(n);
    }
    let threads_before = threads();
    let fds_before = fds();
    let live_before = LIVE.load(Ordering::SeqCst);
    for n in 0..200 {
        cycle(1000 + n);
    }
    settled("threads", threads_before, threads);
    settled("fds", fds_before, fds);
    let grown = LIVE.load(Ordering::SeqCst) - live_before;
    // Measured: 0. A leak of 21 bytes a cycle would be over the line.
    assert!(
        grown < 4 * 1024,
        "{grown} bytes more are allocated after 200 cycles"
    );
}
