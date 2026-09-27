//! A croc relay's banner, the first thing it says once the connection is
//! sealed: its data ports and our address as it saw it
//! (`sukkula_engine::croc::fuzzing::banner`).
//!
//! The ports are where every later connection of the transfer goes, so a
//! relay that could smuggle a port 0, a number past 65535 or a thousand
//! ports into the list would steer or stall us. Asserted, against the
//! banner read independently here: a banner is taken only if it is
//! `ports|||address`, or `ports|||address|||token` as croc 11's relays
//! write it to a client that asks for a token (which is not read), with
//! an address of at most 64 bytes, and `ports` is
//! either `ok` (no data ports: the room's own connection carries the data)
//! or 1 to 16 comma-separated decimal numbers of at most five digits, each
//! 1 to 65535 -- and then the ports are exactly those, in order. "bad
//! password" is the relay refusing ours.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_engine::croc::fuzzing::{self, BANNER_PORTS};

/// The ports a banner names, read here without the engine's code.
fn expected(plain: &[u8]) -> Option<Vec<u16>> {
    let text = std::str::from_utf8(plain).ok()?;
    let mut parts = text.split("|||");
    let ports = parts.next()?;
    let address = parts.next()?;
    if address.len() > 64 {
        return None;
    }
    if ports == "ok" {
        return Some(Vec::new());
    }
    let list: Vec<&str> = ports.split(',').collect();
    if list.len() > BANNER_PORTS {
        return None;
    }
    list.iter()
        .map(|p| {
            let digits = !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit());
            digits
                .then(|| p.bytes().fold(0u32, |n, d| n * 10 + u32::from(d - b'0')))
                .and_then(|n| u16::try_from(n).ok())
                .filter(|n| *n > 0)
        })
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let got = fuzzing::banner(data);
    if data == b"bad password" {
        assert!(got.is_err(), "the relay's refusal read as ports");
        return;
    }
    let want = expected(data);
    match (got, want) {
        (Ok(got), Some(want)) => {
            assert!(got.len() <= BANNER_PORTS, "{} ports", got.len());
            assert!(got.iter().all(|p| *p > 0), "port 0 taken");
            assert_eq!(got, want, "ports other than the banner's");
        }
        (Err(_), None) => {}
        (got, want) => panic!("banner {data:?}: engine {got:?}, restated {want:?}"),
    }
});
