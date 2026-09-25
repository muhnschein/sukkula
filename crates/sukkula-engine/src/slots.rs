//! A counted slot: take one if fewer than a maximum are taken, give it back
//! later. Used for the hub's in-flight commands and wormhole's connecting
//! receives.
//!
//! A compare-and-swap loop rather than `AtomicUsize::fetch_update`, which
//! newer toolchains deprecate under another name: the fuzz build runs on
//! nightly, and `warnings = "deny"` would turn the rename into a build
//! failure there while the pinned toolchain has no replacement yet.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Takes a slot if fewer than `max` are taken. False when none is free.
pub(crate) fn take(count: &AtomicUsize, max: usize) -> bool {
    let mut n = count.load(Ordering::Acquire);
    loop {
        if n >= max {
            return false;
        }
        match count.compare_exchange_weak(
            n,
            n.saturating_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(actual) => n = actual,
        }
    }
}

/// Gives a slot back. Never goes below zero.
pub(crate) fn give_back(count: &AtomicUsize) {
    let mut n = count.load(Ordering::Acquire);
    while n > 0 {
        match count.compare_exchange_weak(
            n,
            n.saturating_sub(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(actual) => n = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_bounded_and_never_negative() {
        let c = AtomicUsize::new(0);
        assert!(take(&c, 2));
        assert!(take(&c, 2));
        assert!(!take(&c, 2));
        give_back(&c);
        assert!(take(&c, 2));
        give_back(&c);
        give_back(&c);
        give_back(&c);
        assert_eq!(c.load(Ordering::Acquire), 0);
    }

    #[test]
    fn concurrent_takers_never_exceed_the_maximum() {
        let c = std::sync::Arc::new(AtomicUsize::new(0));
        let taken: usize = (0..8)
            .map(|_| {
                let c = c.clone();
                std::thread::spawn(move || (0..1000).filter(|_| take(&c, 100)).count())
            })
            .map(|h| h.join().unwrap())
            .sum();
        assert_eq!(taken, 100);
    }
}
