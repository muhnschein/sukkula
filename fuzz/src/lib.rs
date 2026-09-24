//! What every fuzz target shares: the independent statement of S1 and S2
//! that `crates/sukkula-core/tests/` holds the real one to, compiled from the
//! same file so the three layers (property tests, hostile sweep, fuzzing)
//! can never disagree about what "safe" means.

#[path = "../../crates/sukkula-core/tests/common/oracle.rs"]
pub mod oracle;

/// Asserts `violation` is `None`, naming the input when it is not. A
/// violation is a finding as real as a crash: libFuzzer saves the input.
pub fn assert_clean(what: &str, input: &str, violation: Option<String>) {
    if let Some(v) = violation {
        panic!("{what}: {v} for input {input:?}");
    }
}
