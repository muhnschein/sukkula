/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * The C ABI between the Qt shell and the Rust engine: four functions, JSON
 * in and out. The messages are defined in crates/sukkula-engine/src/api.rs
 * and shown with examples in docs/FFI.md.
 *
 * Threading and lifetime rules, which the shell must follow and the engine
 * guarantees:
 *
 *  - The callback runs on an engine thread, never on the thread that called
 *    into the engine. It must copy what it needs and return quickly: the
 *    string is valid only for the duration of the call.
 *  - The callback must not call sukkula_stop(). It may call
 *    sukkula_command().
 *  - Once sukkula_stop() returns, the callback is never called again and
 *    `userdata` may be freed.
 *  - Every function tolerates NULL and malformed input, and no Rust panic
 *    ever crosses this boundary.
 */
#ifndef SUKKULA_H
#define SUKKULA_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* An opaque running engine. */
typedef struct SukkulaEngine SukkulaEngine;

/* Receives one event: NUL-terminated UTF-8 JSON, valid only during the call. */
typedef void (*sukkula_event_cb)(const char *event_json, void *userdata);

/* Return values of sukkula_command(). */
#define SUKKULA_OK            0  /* taken; its "reply" event follows */
#define SUKKULA_ERR_NULL     -1  /* engine or command_json was NULL */
#define SUKKULA_ERR_UTF8     -2  /* command_json is not UTF-8 */
#define SUKKULA_ERR_TOO_LONG -3  /* command_json is over 64 KiB; nothing was parsed */
#define SUKKULA_ERR_PANIC    -4  /* the engine failed internally; see the log */

/*
 * Starts an engine with a StartConfig (api.rs), e.g.
 *
 *   {"v":1,"data_dir":"/home/defaultuser/.local/share/sukkula/sukkula",
 *    "download_dir":"/home/defaultuser/Downloads/Sukkula"}
 *
 * Emits "started", "settings" and "receiving" before returning. Returns NULL
 * on failure, after one "fatal" event saying why. `callback` must not be NULL.
 */
SukkulaEngine *sukkula_start(const char *config_json, sukkula_event_cb callback, void *userdata);

/*
 * Hands the engine one command, e.g.
 *
 *   {"v":1,"id":7,"cmd":{"type":"set_receiving","on":true}}
 *
 * Returns SUKKULA_OK when the command was taken; exactly one "reply" event
 * with the same id follows, from an engine thread. Any other return value
 * means no reply will come.
 */
int32_t sukkula_command(SukkulaEngine *engine, const char *command_json);

/*
 * Stops the engine and frees it. Blocks until every engine thread has
 * finished (a few seconds at most). NULL is ignored. The handle is invalid
 * afterwards.
 */
void sukkula_stop(SukkulaEngine *engine);

/* The engine version, e.g. "0.1.0". A static string; never free it. */
const char *sukkula_version(void);

#ifdef __cplusplus
}
#endif

#endif /* SUKKULA_H */
