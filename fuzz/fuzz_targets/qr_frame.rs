//! Camera frames, through rqrr and the reading of what a code says (spec
//! v0.6): `sukkula_engine::scan::scan`, what `sukkula_scan_qr` runs on
//! every frame the viewfinder gives.
//!
//! A frame is whatever the camera is pointed at, a QR code printed to be
//! hostile among them. The input's first byte is the frame's width, and
//! the rest its rows, as many whole ones as there are up to `MAX_SIDE`
//! (`sukkula_fuzz::qr_frame_of`). Asserted: the scan
//! ends -- libFuzzer's timeout is the bound, the grouping rqrr is vendored
//! with (`third_party/rqrr.patches/0002`) what keeps it -- and anything it
//! reads keeps the shape every code keeps (`sukkula_fuzz::assert_scanned`);
//! what it reads in a frame, it reads in the same frame again.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use sukkula_engine::scan;
use sukkula_fuzz::{assert_scanned, qr_frame_of};

fuzz_target!(|data: &[u8]| {
    let Some(frame) = qr_frame_of(data) else {
        return;
    };
    let found = scan::scan(&frame);
    if let Some(found) = &found {
        assert_scanned(found);
    }
    assert_eq!(
        scan::scan(&frame),
        found,
        "the same frame read twice differently"
    );
});
