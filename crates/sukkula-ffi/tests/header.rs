//! `include/sukkula.h`, the Rust side and `docs/FFI.md` agree: the same
//! return codes with the same values, the same four functions, and the
//! same limits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use sukkula_ffi::{
    MAX_IN_FLIGHT_COMMANDS, MAX_MESSAGE_BYTES, SUKKULA_ERR_BUSY, SUKKULA_ERR_NULL,
    SUKKULA_ERR_PANIC, SUKKULA_ERR_TOO_LONG, SUKKULA_ERR_UTF8, SUKKULA_OK,
};

const HEADER: &str = include_str!("../include/sukkula.h");
const DOC: &str = include_str!("../../../docs/FFI.md");

/// Every `#define SUKKULA_... <int>` in the header.
fn codes() -> BTreeMap<String, i32> {
    HEADER
        .lines()
        .filter_map(|l| l.strip_prefix("#define SUKKULA_"))
        .filter_map(|rest| {
            let mut words = rest.split_whitespace();
            let name = words.next()?;
            let value = words.next()?.parse().ok()?;
            Some((format!("SUKKULA_{name}"), value))
        })
        .collect()
}

#[test]
fn the_return_codes_match() {
    let rust = BTreeMap::from([
        ("SUKKULA_OK".to_owned(), SUKKULA_OK),
        ("SUKKULA_ERR_NULL".to_owned(), SUKKULA_ERR_NULL),
        ("SUKKULA_ERR_UTF8".to_owned(), SUKKULA_ERR_UTF8),
        ("SUKKULA_ERR_TOO_LONG".to_owned(), SUKKULA_ERR_TOO_LONG),
        ("SUKKULA_ERR_PANIC".to_owned(), SUKKULA_ERR_PANIC),
        ("SUKKULA_ERR_BUSY".to_owned(), SUKKULA_ERR_BUSY),
    ]);
    assert_eq!(codes(), rust, "the header and the Rust constants");
    for (name, value) in &rust {
        assert!(
            DOC.contains(&format!("| `{name}` | {value} |")),
            "{name} = {value} is not in docs/FFI.md's table"
        );
    }
}

#[test]
fn the_functions_match() {
    // Referencing them is what proves the symbols exist with these types.
    let _: unsafe extern "C" fn(
        *const std::ffi::c_char,
        sukkula_ffi::SukkulaEventCb,
        *mut std::ffi::c_void,
    ) -> *mut sukkula_ffi::SukkulaEngine = sukkula_ffi::sukkula_start;
    let _: unsafe extern "C" fn(*mut sukkula_ffi::SukkulaEngine, *const std::ffi::c_char) -> i32 =
        sukkula_ffi::sukkula_command;
    let _: extern "C" fn(*mut sukkula_ffi::SukkulaEngine) = sukkula_ffi::sukkula_stop;
    let _: extern "C" fn() -> *const std::ffi::c_char = sukkula_ffi::sukkula_version;

    let normalise = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let header = normalise(HEADER);
    let doc = normalise(DOC);
    for proto in [
        "SukkulaEngine *sukkula_start(const char *config_json, sukkula_event_cb callback, void *userdata);",
        "int32_t sukkula_command(SukkulaEngine *engine, const char *command_json);",
        "void sukkula_stop(SukkulaEngine *engine);",
        "const char *sukkula_version(void);",
        "typedef void (*sukkula_event_cb)(const char *event_json, void *userdata);",
    ] {
        assert!(header.contains(proto), "the header lacks {proto}");
        assert!(doc.contains(proto), "docs/FFI.md lacks {proto}");
    }
}

#[test]
fn the_limits_match() {
    assert_eq!(MAX_MESSAGE_BYTES, 64 * 1024);
    assert!(HEADER.contains("over 64 KiB"));
    assert!(HEADER.contains(&format!(
        "{MAX_IN_FLIGHT_COMMANDS} commands await their reply"
    )));
}
