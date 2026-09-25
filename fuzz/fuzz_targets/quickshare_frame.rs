//! Quick Share after the key exchange: everything a sender can put in the
//! encrypted channel, through rqs_lib's frame reader, D2D channel, byte
//! payload reassembly and introduction, and the adapter's conversion of the
//! introduction into the offer the user is asked about (spec §7 "Quick
//! Share frames").
//!
//! The receiver starts where a sender that finished UKEY2 leaves it --
//! before the paired-key frames, between them, or waiting for the
//! introduction -- under keys the harness shares, so every frame passes
//! the channel's HMAC unless a step spoils it on purpose. Asserted, by
//! `sukkula_fuzz::quickshare::Receiver`: no file byte or text before the
//! user accepts (S5); the library's size, count and order guarantees the
//! adapter relies on (S4, S6); every offer that validates keeps every
//! S-rule, and a received text is S2-clean.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use sukkula_fuzz::quickshare::{FrameInput, run_frames};

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = FrameInput::arbitrary(&mut Unstructured::new(data)) {
        run_frames(&input);
    }
});
