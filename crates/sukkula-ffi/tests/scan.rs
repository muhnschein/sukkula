//! `sukkula_scan_qr`, called as the shell calls it: frames with a code,
//! without one, and of every wrong shape; buffers that fit the answer
//! exactly, and one byte short; and from several threads at once.

#![allow(
    unsafe_code, // The C ABI, called as C calls it.
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::ffi::{CStr, c_char};
use std::ptr;

use serde_json::{Value, json};
use sukkula_ffi::{SUKKULA_ERR_NULL, SUKKULA_ERR_RANGE, SUKKULA_SCAN_BYTES, sukkula_scan_qr};

/// A `w` by `h` white frame, `stride` bytes a row and no byte more, with
/// `text` as a QR code at 4 pixels a module near its top left.
fn frame(text: &str, w: usize, h: usize, stride: usize) -> Vec<u8> {
    let mut f = vec![u8::MAX; stride * (h - 1) + w];
    let q = qrcode::QrCode::new(text.as_bytes()).unwrap();
    let size = q.width();
    for (i, c) in q.to_colors().iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (x, y) = (16 + (i % size) * 4, 16 + (i / size) * 4);
            for d in 0..16 {
                f[(y + d / 4) * stride + x + d % 4] = 0;
            }
        }
    }
    f
}

/// Calls the function on `luma` with an `out_size` buffer: its return
/// value, and the buffer's string when it wrote one.
fn call(luma: &[u8], w: u32, h: u32, stride: u32, out_size: u32) -> (i32, Option<String>) {
    let mut out = vec![0x55 as c_char; out_size as usize + 1];
    // SAFETY: `luma` spans the frame, `out` has `out_size` bytes (and one
    // more, left alone, to see that nothing is written past them).
    let n = unsafe { sukkula_scan_qr(luma.as_ptr(), w, h, stride, out.as_mut_ptr(), out_size) };
    assert_eq!(
        out[out_size as usize], 0x55,
        "a byte past out_size was written"
    );
    let text = (n > 0).then(|| {
        // SAFETY: a positive return is the length of a NUL-terminated string.
        let s = unsafe { CStr::from_ptr(out.as_ptr()) };
        assert_eq!(s.to_bytes().len(), n as usize);
        s.to_str().unwrap().to_owned()
    });
    (n, text)
}

#[test]
fn a_frame_with_a_code_gives_its_json_and_one_without_gives_zero() {
    let f = frame("gala-tulip-acorn", 200, 160, 204);
    let (n, text) = call(&f, 200, 160, 204, SUKKULA_SCAN_BYTES);
    assert!(n > 0, "{n}");
    let v: Value = serde_json::from_str(&text.unwrap()).unwrap();
    assert_eq!(v, json!({"found": "croc", "code": "gala-tulip-acorn"}));

    let f = frame("wormhole-transfer:7-guitarist-revenge", 240, 240, 240);
    let (_, text) = call(&f, 240, 240, 240, SUKKULA_SCAN_BYTES);
    let v: Value = serde_json::from_str(&text.unwrap()).unwrap();
    assert_eq!(
        v,
        json!({"found": "wormhole", "code": "7-guitarist-revenge"})
    );

    let f = frame("https://example.org/", 200, 200, 200);
    let (_, text) = call(&f, 200, 200, 200, SUKKULA_SCAN_BYTES);
    assert_eq!(text.as_deref(), Some(r#"{"found":"other"}"#));

    let white = vec![u8::MAX; 100 * 100];
    assert_eq!(call(&white, 100, 100, 100, SUKKULA_SCAN_BYTES), (0, None));
}

#[test]
fn the_answer_is_written_only_when_it_fits() {
    let f = frame("gala-tulip-acorn", 200, 160, 200);
    let (n, _) = call(&f, 200, 160, 200, SUKKULA_SCAN_BYTES);
    let n = u32::try_from(n).unwrap();
    assert_eq!(call(&f, 200, 160, 200, n + 1).0, i32::try_from(n).unwrap());
    assert_eq!(call(&f, 200, 160, 200, n).0, SUKKULA_ERR_RANGE);
    assert_eq!(call(&f, 200, 160, 200, 1).0, SUKKULA_ERR_RANGE);
    // Nothing to say fits in one byte.
    let white = vec![u8::MAX; 64 * 64];
    assert_eq!(call(&white, 64, 64, 64, 1).0, 0);
}

#[test]
fn nulls_and_shapes_out_of_range_are_refused_unread() {
    let f = vec![u8::MAX; 4096 * 1024];
    let mut out = [0 as c_char; 64];
    // SAFETY: NULLs are refused before anything is read or written.
    unsafe {
        assert_eq!(
            sukkula_scan_qr(ptr::null(), 10, 10, 10, out.as_mut_ptr(), 64),
            SUKKULA_ERR_NULL
        );
        assert_eq!(
            sukkula_scan_qr(f.as_ptr(), 10, 10, 10, ptr::null_mut(), 64),
            SUKKULA_ERR_NULL
        );
    }
    for (w, h, stride, out_size) in [
        (0, 10, 10, 64),
        (10, 0, 10, 64),
        (1025, 10, 1025, 64),
        (10, 1025, 10, 64),
        (10, 10, 9, 64),
        (10, 10, 4097, 64),
        (10, 10, 10, 0),
        (u32::MAX, u32::MAX, u32::MAX, 64),
    ] {
        // Every shape here is refused before `f` is read, so its size does
        // not matter; it is big enough for any shape accepted.
        assert_eq!(
            call(&f, w, h, stride, out_size).0,
            SUKKULA_ERR_RANGE,
            "{w}x{h}/{stride}"
        );
    }
    // The largest frame, read to its last byte.
    assert_eq!(call(&f, 1024, 1024, 4096, 64).0, 0);
}

#[test]
fn scans_run_side_by_side() {
    let f = frame("gala-tulip-acorn", 200, 160, 200);
    std::thread::scope(|s| {
        let runs: Vec<_> = (0..4)
            .map(|_| s.spawn(|| call(&f, 200, 160, 200, SUKKULA_SCAN_BYTES).1))
            .collect();
        for r in runs {
            assert_eq!(
                r.join().unwrap().as_deref(),
                Some(r#"{"found":"croc","code":"gala-tulip-acorn"}"#)
            );
        }
    });
}
