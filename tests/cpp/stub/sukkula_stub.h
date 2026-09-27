/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Test hooks of the stub engine (sukkula_stub.c). The stub implements the
 * five functions of crates/sukkula-ffi/include/sukkula.h with the same
 * threading rules as the real engine -- the callback on a thread of its
 * own, never after sukkula_stop() returns -- so the Qt bridge can be
 * tested on the host without the Rust library.
 */
#ifndef SUKKULA_STUB_H
#define SUKKULA_STUB_H

#ifdef __cplusplus
extern "C" {
#endif

/* The StartConfig JSON the last sukkula_start() received, or "". */
const char *sukkula_stub_last_config(void);

/* Makes the next sukkula_start() fail (after one "fatal" event). */
void sukkula_stub_fail_next_start(int fail);

/*
 * Special commands, recognised by a "stub" member in the command JSON:
 *   "stub":"burst"     -- 5000 events as fast as the thread can go
 *   "stub":"oversize"  -- one event of 2 MiB
 *   "stub":"slow"      -- the reply after 200 ms
 *   "stub":"busy"      -- refused with SUKKULA_ERR_BUSY, no reply
 * Every other command is answered with {"type":"reply","id":N,"ok":true}.
 */

/*
 * sukkula_scan_qr() in the stub reads no QR code: it checks its arguments
 * as the engine does, and answers {"found":"croc","code":"gala-tulip-acorn"}
 * (SUKKULA_STUB_SCANNED) for a frame with any pixel darker than 128, and 0
 * for one without. These say what it was last given, and make it slow.
 */
#define SUKKULA_STUB_SCANNED "{\"found\":\"croc\",\"code\":\"gala-tulip-acorn\"}"
void sukkula_stub_last_scan(unsigned *width, unsigned *height, unsigned *stride, int *scans);
void sukkula_stub_scan_delay_ms(long ms);

#ifdef __cplusplus
}
#endif

#endif /* SUKKULA_STUB_H */
