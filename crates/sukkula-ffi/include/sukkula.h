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
 *    into the engine. Calls never overlap: events arrive one at a time, in
 *    the order the engine emitted them. The callback must copy what it
 *    needs and return quickly: the string is valid only for the duration of
 *    the call.
 *  - The callback must not wait for a thread that calls into the engine:
 *    sukkula_stop() waits for a callback in progress to return. In Qt,
 *    post with Qt::QueuedConnection, never Qt::BlockingQueuedConnection.
 *  - The callback must not throw or otherwise unwind into the engine.
 *  - The callback must not call sukkula_stop(). It may call
 *    sukkula_command(). (If it does call sukkula_stop() on its own engine,
 *    the engine is stopped without waiting for the callback, events not yet
 *    delivered are dropped, and the callback is not called again once the
 *    current call returns. Nothing deadlocks.)
 *  - Once sukkula_stop() returns, the callback is never called again and
 *    `userdata` may be freed. The engine never dereferences `userdata`; it
 *    only passes it back to the callback, on the engine thread.
 *  - Every function tolerates NULL and malformed input, and no Rust panic
 *    ever crosses this boundary. A handle that has been stopped is refused
 *    like NULL, never used.
 */
#ifndef SUKKULA_H
#define SUKKULA_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* An opaque running engine. Never dereference it. */
typedef struct SukkulaEngine SukkulaEngine;

/* Receives one event: NUL-terminated UTF-8 JSON, valid only during the call. */
typedef void (*sukkula_event_cb)(const char *event_json, void *userdata);

/* Return values of sukkula_command(). Treat any value but SUKKULA_OK as
 * "no reply will come"; later versions may add codes. Enumerators rather
 * than macros: constants of type int in C and in C++ alike, which a
 * debugger can name and the preprocessor cannot redefine. */
enum {
    SUKKULA_OK = 0,            /* taken; its "reply" event follows */
    SUKKULA_ERR_NULL = -1,     /* engine or command_json was NULL, or the engine was stopped */
    SUKKULA_ERR_UTF8 = -2,     /* command_json is not UTF-8 */
    SUKKULA_ERR_TOO_LONG = -3, /* command_json is over 64 KiB; nothing was parsed */
    SUKKULA_ERR_PANIC = -4,    /* the engine failed internally; see the log (stderr) */
    SUKKULA_ERR_BUSY = -5      /* 64 commands await their reply; nothing was parsed, try again after one */
};
/* CONTRACT: SUKKULA_ERR_BUSY is new (additive; covered by the rule above).
 * The codes were #defines before, with the same names and values; nothing
 * that compared or returned them changes. */

/*
 * Starts an engine with a StartConfig (api.rs), e.g.
 *
 *   {"v":1,"data_dir":"/home/defaultuser/.local/share/sukkula/sukkula",
 *    "download_dir":"/home/defaultuser/Downloads/Sukkula"}
 *
 * Emits "started", "settings" and "receiving" before returning. Returns NULL
 * on failure, after one "fatal" event saying why. `callback` must not be NULL
 * (if it is, NULL is returned and nothing else happens).
 */
SukkulaEngine *sukkula_start(const char *config_json, sukkula_event_cb callback, void *userdata);

/*
 * Hands the engine one command, e.g.
 *
 *   {"v":1,"id":7,"cmd":{"type":"set_receiving","on":true}}
 *
 * Returns SUKKULA_OK when the command was taken; exactly one "reply" event
 * with the same id follows, from an engine thread. Any other return value
 * means no reply will come. Never blocks; may be called from any thread,
 * including from inside the callback.
 */
int32_t sukkula_command(SukkulaEngine *engine, const char *command_json);

/*
 * Stops the engine and frees it. Blocks until every engine thread has
 * finished (a few seconds at most, plus however long a callback in progress
 * takes to return). NULL, and a handle already stopped, are ignored. The
 * handle is invalid afterwards.
 */
void sukkula_stop(SukkulaEngine *engine);

/* The engine version, e.g. "0.1.0". A static string; never free it. */
const char *sukkula_version(void);

#ifdef __cplusplus
}
#endif

#endif /* SUKKULA_H */
