# The engine interface

The Qt shell and the Rust engine talk through four C functions, with JSON in
and out. This document shows every message with an example; the
definitions are `crates/sukkula-engine/src/api.rs`, and the ABI is
`crates/sukkula-ffi/include/sukkula.h`.

The document cannot drift from the code: `crates/sukkula-engine/tests/hub_docs.rs`
parses every JSON block below (commands must parse, events must round-trip
through `Event` unchanged, the start configuration must parse) and checks
that every command, event and error code is shown, and
`crates/sukkula-ffi/tests/header.rs` checks every return code of the header
against this page and the Rust constants. A JSON block is marked with its
kind after the language: `json command`, `json event` or `json config`.

## Contents

1. [Functions](#functions)
2. [Threads and lifetimes](#threads-and-lifetimes)
3. [Limits](#limits)
4. [Versioning](#versioning)
5. [Start configuration](#start-configuration)
6. [Commands](#commands)
7. [Events](#events)
8. [Error codes](#error-codes)
9. [Values](#values)
10. [Logging](#logging)
11. [Linking](#linking)
12. [Testing](#testing)

## Functions

```c
SukkulaEngine *sukkula_start(const char *config_json, sukkula_event_cb callback, void *userdata);
int32_t        sukkula_command(SukkulaEngine *engine, const char *command_json);
void           sukkula_stop(SukkulaEngine *engine);
const char    *sukkula_version(void);

typedef void (*sukkula_event_cb)(const char *event_json, void *userdata);
```

**`sukkula_start`** takes a [start configuration](#start-configuration) and
the callback every event goes to. Before it returns, the callback has had
`started`, `settings` and `receiving`, in that order, and nothing else.
On failure it returns `NULL` after exactly one [`fatal`](#fatal) event. A
`NULL` callback returns `NULL` and nothing else happens (there is nobody to
tell).

**`sukkula_command`** hands over one [command](#commands). It never blocks,
and may be called from any thread, including from inside the callback.

| Return value | Value | Meaning |
| --- | --- | --- |
| `SUKKULA_OK` | 0 | Taken. Exactly one `reply` with the command's `id` follows. |
| `SUKKULA_ERR_NULL` | -1 | `engine` or `command_json` was `NULL`, or the engine was stopped. |
| `SUKKULA_ERR_UTF8` | -2 | `command_json` is not UTF-8. |
| `SUKKULA_ERR_TOO_LONG` | -3 | `command_json` is over 64 KiB; nothing was parsed. |
| `SUKKULA_ERR_PANIC` | -4 | The engine failed internally. |
| `SUKKULA_ERR_BUSY` | -5 | 64 commands are waiting for their replies; nothing was parsed. Try again after a reply. |

Any value but `SUKKULA_OK` means no reply will come, and later versions may
add values: treat every unknown one that way. A command that is taken but
malformed -- bad JSON, an unknown field, another version -- is answered
with a failed `reply`, with the `id` if one could be read and `0`
otherwise.

**`sukkula_stop`** stops the engine and frees it. It blocks until every
engine thread has finished: a few seconds at most, plus however long a
callback in progress takes to return. Events emitted before the stop are
delivered first. `NULL`, and a handle already stopped, are ignored.

**`sukkula_version`** returns the engine's version, e.g. `"0.1.0"`, as a
static string. Never free it.

## Threads and lifetimes

- **One delivery thread.** The engine calls the callback from one thread of
  its own, one event at a time, in the order the engine emitted them. Calls
  never overlap.
- **Never on a calling thread.** The callback never runs on a thread that
  is inside `sukkula_start`, `sukkula_command` or `sukkula_stop`, so it
  cannot re-enter the caller. Even the `fatal` event of a failed start is
  delivered from another thread while `sukkula_start` waits for it.
- **Copy and return.** The string is valid only during the call. Copy it
  and return quickly; post the copy to the UI thread with
  `Qt::QueuedConnection`.
- **Never wait for a caller.** The callback must not wait for any thread
  that calls into the engine: `sukkula_stop` waits for a callback in
  progress, so `Qt::BlockingQueuedConnection` to the thread that calls
  `sukkula_stop` deadlocks.
- **Never unwind.** A C++ exception must not leave the callback.
- **Commands from the callback** are allowed; they never block.
- **Stop from the callback** is forbidden, and safe anyway. When the
  callback stops its own engine, `sukkula_stop` returns once the engine is
  down without waiting for the callback (which is the caller), events not
  yet delivered are dropped, and the callback is not called again after the
  current call returns. Stopping *another* engine from a callback waits for
  that engine's callback at most 3 seconds, since the two could be waiting
  for each other.
- **After `sukkula_stop` returns** the callback is never called again, and
  `userdata` may be freed. The engine never dereferences `userdata`; it only
  hands it back to the callback, on the delivery thread. Whatever it points
  to must therefore be usable from that thread.
- **Handles are never dereferenced.** A handle is an opaque number the
  engine looks up. `sukkula_command` or `sukkula_stop` on a handle that was
  stopped, or never given out, is refused like `NULL` rather than used, so a
  command racing a stop on another thread is harmless.
- **Panics never cross.** Every function runs inside `catch_unwind`. A
  command whose handler panics is still answered, with `internal`.

## Limits

| What | Limit | When over it |
| --- | --- | --- |
| A command, or the start configuration | 64 KiB of UTF-8, not counting the NUL | `SUKKULA_ERR_TOO_LONG`, or a `fatal` event; nothing is parsed |
| Commands waiting for their reply | 64 | `SUKKULA_ERR_BUSY`; nothing is parsed |
| One command's work | 60 s; `receive_wormhole` 150 s, since it waits for the user | Answered with `internal` |
| `set_receiving` and `set_settings` waiting for the receive switch | 60 s; then they run to the end, each protocol bounded as below | Answered with `internal`; nothing was changed |
| One protocol's start or stop | 15 s | That protocol reports `failed`; the others go on |
| An event | 256 KiB of JSON | Never happens: events are bounded by construction. A `reply` or `transfer_finished` would be replaced by an `internal` failure, anything else dropped |
| Events waiting for the callback | 1024 events or 4 MiB, replies not counted | The engine waits for the callback to catch up |
| An error `message` | 256 characters, plain text | Cut with `…` |

## Versioning

- **`API_VERSION`** is `1`. Every command and the start configuration carry
  it as `"v"`, and `started` reports it as `"api"`. A command with another
  version is answered with `bad_version`; a start configuration with another
  version fails with `bad_version`.
- **What bumps it:** removing or renaming a command, event, field or value,
  or changing what one means.
- **What does not:** a new event type, a new field in an event, a new
  command, a new error code, a new value of an event's enum. The shell
  ignores events and fields it does not know, and treats an unknown error
  code as `internal`.
- **Commands are strict.** Unknown fields and unknown commands are refused
  (`bad_command`), so a typo never silently does nothing.
- The shell and the engine ship in one package, built from one commit, so
  the version is a tripwire for a mismatched build, not a negotiation: no
  engine speaks an older or newer version.
- **The C ABI** -- the four functions and their types -- does not change
  while `API_VERSION` is 1, except that return values may be added to
  `sukkula_command`. `sukkula_version` is the crate version, semver.

## Start configuration

```json config
{
  "v": 1,
  "data_dir": "/home/defaultuser/.local/share/sukkula/sukkula",
  "download_dir": "/home/defaultuser/Downloads/Sukkula",
  "device_model": "Jolla Phone",
  "allow_loopback": false
}
```

| Field | Meaning |
| --- | --- |
| `v` | `1`. |
| `data_dir` | The app's private data (settings, the LocalSend key). Created `0700`. |
| `download_dir` | Where received files go. Created `0700`; files are staged in its `.partial/` (S3). |
| `device_model` | Optional. The default device name (F-C7). When absent, `NAME=` (else `MER_HA_DEVICE=`) from `/etc/hw-release`. |
| `allow_loopback` | Optional, `false`. Answer peers on loopback: tests only, the shell never sets it. |

Both directories must be absolute, below `/`, at most 4096 bytes, and free
of `.` and `..` components and control characters. Neither may be inside
the other, even through a symlink: a received file must never land among
the settings and the key. The minimal configuration:

```json config
{"v":1,"data_dir":"/home/defaultuser/.local/share/sukkula/sukkula","download_dir":"/home/defaultuser/Downloads/Sukkula"}
```

## Commands

A command is an envelope with the version, an `id` of the shell's choosing
(echoed in the `reply`), and the command itself, tagged by `type`.

### set_receiving

Turns every enabled receiver on or off together (F-C1). Emits `receiving`
with `starting` states, then `receiving` with the outcome, then the reply.

It and `set_settings` hold the receive switch one at a time. Each waits at
most 60 s for the ones before it, and past that is answered with
`internal` having changed nothing. Once one has the switch it is not cut
off: it runs to the end, bounded by the 15 s each protocol has to start or
stop, and always ends with the final `receiving`.

```json command
{"v":1,"id":1,"cmd":{"type":"set_receiving","on":true}}
```

### set_settings

Replaces the settings, validates them (`bad_settings` on a bad PIN or URL),
saves them (`storage`), emits `settings`, and restarts running receivers
with them. Fields left out take their defaults.

Every protocol has `enabled`, on by default (F-C1). A protocol switched off
is not started by `set_receiving` and not used by `start_discovery`, and
its `send`, `receive_wormhole` or `list_bluetooth_devices` is
`unavailable`. A settings file from before `wormhole.enabled` existed
reads as on.

```json command
{"v":1,"id":2,"cmd":{"type":"set_settings","settings":{"device_name":"Pekka's Jolla","localsend":{"enabled":true,"pin":"4711"},"quickshare":{"enabled":true,"visibility":"hidden","ble_nudge":false},"wormhole":{"enabled":true,"mailbox_url":"wss://relay.example.org/v1","relay_url":"tcp://transit.example.org:4001"},"bluetooth":{"enabled":false},"logging":false}}}
```

```json command
{"v":1,"id":3,"cmd":{"type":"set_settings","settings":{"device_name":"Pekka"}}}
```

### get_settings

Emits `settings`.

```json command
{"v":1,"id":4,"cmd":{"type":"get_settings"}}
```

### start_discovery, stop_discovery

Starts or stops looking for LocalSend and Quick Share peers to send to;
found peers arrive as `peer_found` and `peer_lost`.

```json command
{"v":1,"id":5,"cmd":{"type":"start_discovery"}}
```

```json command
{"v":1,"id":6,"cmd":{"type":"stop_discovery"}}
```

### answer

Answers a pending offer (F-C2). `not_found` when it is no longer waiting.

```json command
{"v":1,"id":7,"cmd":{"type":"answer","offer":3,"accept":true}}
```

### send

Sends files and texts. Every file must be an absolute path to a regular
file (`bad_file`) within the size limits (`too_large`); a text is at most
64 KiB. The reply carries the `transfer` id; progress follows as
`transfer_*` events. A protocol disabled in the settings or not in the
build is `unavailable`.

```json command
{"v":1,"id":8,"cmd":{"type":"send","target":{"protocol":"local_send","peer":"ls-4f2a"},"items":[{"kind":"file","path":"/home/defaultuser/Downloads/20260924_101500.jpg"},{"kind":"text","text":"From the lake"}]}}
```

```json command
{"v":1,"id":9,"cmd":{"type":"send","target":{"protocol":"quick_share","peer":"qs-91c0"},"items":[{"kind":"file","path":"/home/defaultuser/Downloads/plan.pdf"}]}}
```

Magic Wormhole sends one file or one text, and reports the code to read out
as `wormhole_code` (F-MW1):

```json command
{"v":1,"id":10,"cmd":{"type":"send","target":{"protocol":"wormhole"},"items":[{"kind":"file","path":"/home/defaultuser/Downloads/plan.pdf"}]}}
```

```json command
{"v":1,"id":11,"cmd":{"type":"send","target":{"protocol":"bluetooth","address":"AA:BB:CC:DD:EE:FF"},"items":[{"kind":"file","path":"/home/defaultuser/Downloads/song.ogg"}]}}
```

### receive_wormhole

Receives with a code the sender's screen shows (F-MW2). The offer then goes
through consent like any other. `bad_code` for a malformed or wrong code.

The reply comes once the user has answered the offer, with the `transfer`
when it was accepted. So the command may take up to 150 s rather than 60:
the mailbox and the key exchange (20 s each), the offer (30 s), the
dialog's whole 60 s, and the goodbye. An offer nobody answers is declined
by its own timeout (`refused`), not cut off by the command's. An offer the
user asked for by typing its code does not count against the limit on
offers from the LAN, so a LAN flood cannot make it `busy`.

```json command
{"v":1,"id":12,"cmd":{"type":"receive_wormhole","code":"7-guitarist-revenge"}}
```

### cancel

Cancels a transfer in either direction (F-C5). `not_found` when it is not
running.

```json command
{"v":1,"id":13,"cmd":{"type":"cancel","transfer":12}}
```

### list_bluetooth_devices

Emits `bluetooth_devices`: the paired devices that accept Object Push.

```json command
{"v":1,"id":14,"cmd":{"type":"list_bluetooth_devices"}}
```

## Events

Every event is an object tagged by `type`. Strings a peer influenced --
names, models, aliases, texts, file names -- have been through S1 or S2;
the UI still shows them with `textFormat: Text.PlainText`.

### fatal

The engine could not start. The only event of a failed `sukkula_start`.

```json event
{"type":"fatal","error":{"code":"storage","message":"the data directory is unusable: /home/defaultuser/.local/share/sukkula/sukkula is not a plain directory"}}
```

### started

The first event after start: the version, the API version, and the
protocols in this build.

```json event
{"type":"started","version":"0.1.0","api":1,"protocols":["local_send","quick_share","wormhole","bluetooth"]}
```

### reply

The answer to one command: `ok`, and `error` when not, and `transfer` for a
`send` or `receive_wormhole`. For long work, `ok` means started.

```json event
{"type":"reply","id":1,"ok":true}
```

```json event
{"type":"reply","id":8,"ok":true,"transfer":12}
```

```json event
{"type":"reply","id":7,"ok":false,"error":{"code":"not_found","message":"no such offer"}}
```

### settings

The settings and the name peers see, after start, `get_settings` and
`set_settings`.

```json event
{"type":"settings","settings":{"device_name":"","localsend":{"enabled":true,"pin":null},"quickshare":{"enabled":true,"visibility":"everyone","ble_nudge":true},"wormhole":{"enabled":true,"mailbox_url":null,"relay_url":null},"bluetooth":{"enabled":true},"logging":false},"effective_device_name":"Jolla Phone"}
```

`recovered` is there, `true`, when the saved settings file could not be
used as it was: edited by hand, written by another version, or damaged.
What could not be read is never replaced by the defaults, which are the
most permissive settings there are. Each part that reads on its own is
kept; a protocol's section that does not is switched off, with no PIN,
`hidden`, no BLE nudge and the default servers; a field this version does
not know switches every protocol off; a file that cannot be read one way
only is all off. The file is left as it is, and `recovered` stays, until
the next `set_settings`. Here the wormhole section held a mailbox URL
this version refuses, and the PIN and `hidden` were kept:

```json event
{"type":"settings","settings":{"device_name":"Pekka","localsend":{"enabled":true,"pin":"4711"},"quickshare":{"enabled":true,"visibility":"hidden","ble_nudge":false},"wormhole":{"enabled":false,"mailbox_url":null,"relay_url":null},"bluetooth":{"enabled":true},"logging":false},"effective_device_name":"Pekka","recovered":true}
```

### receiving

The receive switch, and how each protocol in the build is doing (F-C1).

```json event
{"type":"receiving","on":true,"protocols":[{"protocol":"local_send","state":"ready"},{"protocol":"quick_share","state":"failed","error":{"code":"network","message":"the port is in use"}},{"protocol":"wormhole","state":"send_only"},{"protocol":"bluetooth","state":"send_only"}]}
```

```json event
{"type":"receiving","on":false,"protocols":[{"protocol":"local_send","state":"off"},{"protocol":"quick_share","state":"off"},{"protocol":"wormhole","state":"send_only"},{"protocol":"bluetooth","state":"send_only"}]}
```

### peer_found, peer_lost

A peer to send to appeared, changed, or went away. `id` is opaque and
stable while the peer is visible.

```json event
{"type":"peer_found","peer":{"id":"ls-4f2a","protocol":"local_send","name":"Aino's laptop","model":"ThinkPad X1","device_type":"computer"}}
```

```json event
{"type":"peer_found","peer":{"id":"qs-91c0","protocol":"quick_share","name":"Pixel 9","device_type":"phone"}}
```

```json event
{"type":"peer_lost","peer":"ls-4f2a"}
```

### offer_pending

Ask the user (F-C2). At most 50 files are listed; `more_files` counts the
rest. `pin` is the Quick Share PIN to compare with the sender's screen
(F-QS3). Unanswered, the offer is declined after `expires_in` seconds
(F-C3).

```json event
{"type":"offer_pending","offer":{"id":3,"protocol":"quick_share","sender":"Pixel 9","files":[{"name":"PXL_20260924_101500.jpg","size":3145728},{"name":"notes.txt","size":812}],"more_files":0,"file_count":2,"total_bytes":3146540,"has_text":false,"pin":"4821","expires_in":60}}
```

```json event
{"type":"offer_pending","offer":{"id":4,"protocol":"local_send","sender":"Aino's laptop","model":"ThinkPad X1","files":[],"more_files":0,"file_count":0,"total_bytes":0,"has_text":true,"expires_in":60}}
```

### offer_closed

Take the offer off the screen, and why.

```json event
{"type":"offer_closed","offer":3,"reason":"accepted"}
```

```json event
{"type":"offer_closed","offer":4,"reason":"timed_out"}
```

### transfer_started

A transfer began, in either direction (F-C5). Cancel it with its `id`.

```json event
{"type":"transfer_started","transfer":{"id":12,"direction":"incoming","protocol":"quick_share","peer":"Pixel 9","files":[{"name":"PXL_20260924_101500.jpg","size":3145728},{"name":"notes.txt","size":812}],"file_count":2,"total_bytes":3146540}}
```

### transfer_progress

At most four times a second per transfer.

```json event
{"type":"transfer_progress","transfer":12,"bytes":1048576,"total":3146540}
```

### transfer_finished

How a transfer ended. For a receive that succeeded, `saved` lists the names
the files were saved under in the download directory (numbered when a name
was taken); names, never paths.

```json event
{"type":"transfer_finished","transfer":12,"outcome":{"result":"done"},"saved":["PXL_20260924_101500.jpg","notes (1).txt"]}
```

```json event
{"type":"transfer_finished","transfer":13,"outcome":{"result":"cancelled"}}
```

```json event
{"type":"transfer_finished","transfer":14,"outcome":{"result":"failed","error":{"code":"peer_mismatch","message":"the certificate does not match the fingerprint the peer announced"}}}
```

### text_received

A text arrived (F-C4). Shown as plain text with a Copy button; a URL in it
is never opened.

```json event
{"type":"text_received","transfer":15,"from":"Aino's laptop","text":"https://example.org/menu\nSee you at seven"}
```

### wormhole_code

The code a wormhole send waits on (F-MW1), and the same code as a QR code:
`size` rows of `size` characters, `1` for a dark module.

```json event
{
  "type": "wormhole_code",
  "transfer": 16,
  "code": "7-guitarist-revenge",
  "qr": {
    "size": 21,
    "rows": [
      "111111100001101111111",
      "100000100100001000001",
      "101110101111101011101",
      "101110100110001011101",
      "101110101110001011101",
      "100000100101101000001",
      "111111101010101111111",
      "000000001001100000000",
      "100010110110101000100",
      "110011001111000010101",
      "011001110110111000000",
      "101100000010001010111",
      "001110101000001001100",
      "000000001101110101010",
      "111111101001010100110",
      "100000101010111011100",
      "101110100111011010010",
      "101110100101110000010",
      "101110101101000010101",
      "100000101100001100011",
      "111111100111100101001"
    ]
  }
}
```

### bluetooth_devices

Paired devices that accept Object Push, after `list_bluetooth_devices`.

```json event
{"type":"bluetooth_devices","devices":[{"address":"AA:BB:CC:DD:EE:FF","name":"Car kit"},{"address":"00:1A:7D:DA:71:13","name":"Aino's tablet"}]}
```

## Error codes

Every `error` is an object with a `code` for the UI to translate and a
`message` in English for logs. The UI translates the `code`, never the
`message`, and the `message` never carries peer-supplied text.

| Code | Meaning |
| --- | --- |
| `bad_command` | The command or configuration was malformed. |
| `bad_version` | It named an API version this engine does not speak. |
| `unavailable` | The protocol is not in this build, or disabled in the settings. |
| `not_found` | The peer, offer or transfer named does not exist (any more). |
| `bad_settings` | A settings value was refused. |
| `bad_file` | A file to send is missing, not a regular file, or unreadable. |
| `too_large` | Over a limit: size, count, text length, concurrent transfers, commands waiting. |
| `refused` | The peer refused, or the user declined. |
| `peer_mismatch` | The peer's certificate did not match what it announced (F-LS3). |
| `bad_code` | A wormhole code that is malformed, or wrong. |
| `network` | A network failure or timeout. |
| `storage` | The file system said no, or there is no space. |
| `internal` | Anything else, including a command that failed internally or ran out of time. |

## Values

| Value | Where | Values |
| --- | --- | --- |
| Protocol | `protocols`, `protocol` | `local_send`, `quick_share`, `wormhole`, `bluetooth` |
| Protocol state | `receiving` | `off`, `starting`, `ready`, `failed` (see `error`), `send_only` |
| Why an offer closed | `offer_closed` | `accepted`, `declined`, `timed_out`, `withdrawn` (the sender went away), `shutdown` (the engine is stopping) |
| Direction | `transfer_started` | `incoming`, `outgoing` |
| Outcome | `transfer_finished` | `done`, `cancelled`, `failed` (see `error`) |
| Device type | `peer_found` | `phone`, `tablet`, `computer`, `unknown` |
| Quick Share visibility | settings | `hidden`, `everyone` (F-QS4) |

## Logging

The engine writes its log to standard error, one line per event, which on
Sailfish reaches the journal (`journalctl --user`); it never writes a log
file. The lines start with `sukkula:` and the level:

```text
sukkula: WARN sukkula_engine::hub: settings file not usable as saved; what could not be read is off parts=["wormhole"]
```

With `logging` off, the default, only warnings and errors of the engine's
own crates appear. With `logging` on, the engine's debug lines appear too,
with the protocol libraries' lines at the levels
`crates/sukkula-engine/src/logging.rs` lists. The switch takes effect with
the `set_settings` that changes it. Neither ever logs a file name, a text,
an alias, a PIN, a wormhole code, a path in the download directory, a
peer's address or the TLS key at info level or above (S9), and the
engine's own lines never do at any level. Behind a `SUKKULA_ERR_PANIC`
or an `internal` error there is usually a panic, which Rust's own panic
message reports on standard error too, with where it happened.

The shell needs to do nothing: the log belongs to the engine, which sets
no process-wide logging state but one: the bridge from Rust's `log` crate,
installed once by the first engine.

## Linking

`sukkula-ffi` is a `staticlib` (`libsukkula_ffi.a`) and an `rlib` (for
tests). The package does not link the archive into the binary. Instead,
`scripts/cross-build-rust.sh` links it into a private shared library,
`libsukkula_ffi.so`, installed in `/usr/share/harbour-sukkula/lib`, where
Harbour lets an app keep its own libraries. The four functions above are
its only exports (`crates/sukkula-ffi/exports.map`). The binary finds the
library through the RPATH that the SDK's `sailfishapp` feature sets.
`src/hardening.pri` writes that RPATH as DT_RPATH, the only form Jolla's
validator reads.

Why a library: the engine's thread-locals. Launched from the app grid,
the binary is not executed but `dlopen()`ed by the `silica-qt5` booster,
and the linker resolves an executable's thread-locals to fixed offsets
from the thread pointer. A `dlopen()`ed object gets none, so the engine's
thread-locals, linked into the binary, would land on the booster's own. A
shared library reaches its thread-locals through TLS descriptors, which
find them wherever glibc allocates them. `ci/check-elf.sh --library` refuses
a library that uses the static models instead.

Link the library with `-Wl,--as-needed`. rustc's own list of the system
libraries a static library needs (`--print native-static-libs`) is
`-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc`, plus `-ldbus-1` once the
Bluetooth adapter uses D-Bus. Nothing in the library refers to `libutil`,
and `libutil.so.1` is not on Harbour's list of allowed libraries: without
`--as-needed` a glibc older than 2.34 would record it as a dependency and
the package would fail validation. Everything else on the list is allowed.

The release profile keeps `panic = "unwind"`: `catch_unwind` at the
boundary depends on it.

Link `src/tls_reserve.c` first into the binary. On the phone, the words
from `tp+16` to `tp+63` of the GUI thread also belong to Android: EGL and
GL run under libhybris and write bionic's TLS slots there. glibc puts the
first TLS block of the process at `tp+16`. That block is the executable's,
if it has thread-locals, and otherwise the first library's. The first RPM
put tokio's runtime context there, and the engine panicked entering its
runtime ("RefCell already borrowed").

The reserve is a 48-byte `.tdata` array, the executable's only
thread-local, so the executable's block is exactly those words and every
library's comes after it. The executable's TLS segment has to be aligned
to at most 16 bytes, so the block starts at exactly `tp+16`. Aligning it to
64, as bionic's own executables do, would leave a 48-byte gap, and glibc
would give that gap to some library's thread-locals.

The rules are checked in these places:

- `ci/check-elf.sh`, on the packaged binary: the array's marker must be the
  first bytes of its TLS image, and its RPATH must be exactly
  `/usr/share/harbour-sukkula/lib`. On the packaged library: exactly the
  four exports, and TLS descriptors only.
- `tests/run-cpp-tests.sh`: the same order for the host builds of
  `harbour-sukkula.pro`.
- `main()` refuses to start if the array is not at `tp+16`.
- `ci/tls-slots-test.sh` runs the aarch64 engine under qemu with the slots
  written, once executed and once `dlopen()`ed by a stand-in booster.
  Negative controls show both hazards are still real: without the
  reserve, and with the archive linked into the binary.

## Testing

| What | Where |
| --- | --- |
| Every function, return code and failure mode; callbacks checked on every event for thread, overlap and after-stop; stop from the callback; commands racing a stop | `crates/sukkula-ffi/tests/ffi.rs` |
| 200 start/stop cycles leak no memory (a counting allocator), threads or file descriptors | `crates/sukkula-ffi/tests/ffi_cycles.rs` |
| The header, the Rust constants and this page agree | `crates/sukkula-ffi/tests/header.rs` |
| Every example on this page | `crates/sukkula-engine/tests/hub_docs.rs` |
| The hub: delivery, bounds, replies after panics, races, hostile commands | `crates/sukkula-engine/src/hub.rs`, `crates/sukkula-engine/tests/hub.rs` |
| The same, from C, under AddressSanitizer, UBSan and LeakSanitizer | `ci/ffi-harness/run.sh` |
| The aarch64 engine, from its library, starts on a thread whose bionic TLS slots were written first and leaves them alone, both executed and `dlopen()`ed by a stand-in booster. Without `src/tls_reserve.c`, or with the archive linked into the binary, it fails. | `ci/tls-slots-test.sh` |

The C harness runs from any directory with

```sh
ci/ffi-harness/run.sh
```

It builds the static library for the host, links `ci/ffi-harness/harness.c`
against it with `-fsanitize=address,undefined -fno-sanitize-recover=all`,
runs it with leak checking on, and exits non-zero on any failed check or
sanitizer report. `CC` picks the compiler (by default the first of clang,
gcc and cc that can link a sanitized program), and
`FFI_HARNESS_CARGO_ARGS=--no-default-features` builds the engine without
protocols.
