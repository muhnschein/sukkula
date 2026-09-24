# Fuzzing Sukkula's parsers and sanitisers

Spec §7 asks for every S-rule as a property test and for cargo-fuzz over the
parsers, with the corpus in the repository and a 60-second fuzz smoke in CI.
Three layers, each catching what the others cannot:

| Layer | Where | Toolchain | When |
| --- | --- | --- | --- |
| **Properties** | `crates/sukkula-core/tests/properties.rs` | pinned stable | every `cargo test` |
| **Sweep** | `crates/sukkula-core/tests/hostile.rs` | pinned stable | every `cargo test` |
| **Fuzz** | this directory | nightly + cargo-fuzz | 60 s per target in CI, longer by hand |

The sweep is a deterministic mutation pass, like clove's
`crates/clove-core/tests/hostile.rs`: no nightly, no flakes, seconds. The
fuzz targets are coverage-guided and go further, but need nightly, so they
run as their own CI job.

All three check the same **oracle**, `crates/sukkula-core/tests/common/oracle.rs`,
compiled into this crate through `#[path]` (`src/lib.rs`). It restates S1
and S2 from the Unicode properties a reader acts on -- controls,
whitespace, bidi controls, `Default_Ignorable_Code_Point`, format
characters, private use, noncharacters, blanks -- spelled out from the UCD
rather than taken from `text::classify`'s table. A range dropped from that
table then fails here instead of passing by agreeing with itself.

This crate is **not a workspace member** (it has its own `[workspace]`), so
`libfuzzer-sys` and `arbitrary` never enter the shipped build, `cargo deny`,
or the root `Cargo.lock`. Being outside it, `cargo test --workspace` does not
compile it, so CI must build it on every push (below): otherwise a changed
signature in `sukkula-core` rots a target silently.

It builds the engine with the three network protocols (`localsend`,
`quickshare`, `wormhole`; not `bluetooth`, whose D-Bus replies have sweeps
of their own), so building needs `libdbus-1-dev` and `pkg-config` like the
workspace does. `fuzz/Cargo.lock` was made from the root `Cargo.lock`, so
every protocol library is fuzzed at the version the phone runs; after a
dependency bump, `cp Cargo.lock fuzz/Cargo.lock` and `cargo metadata
--manifest-path fuzz/Cargo.toml` bring it back in line (cargo drops what the
fuzz crate does not use and adds only `libfuzzer-sys` and `arbitrary`).

## Targets

Each target asserts the rule, not only survival. A sanitiser that returns a
bidi override has not crashed, and is still the bug.

| Target | Surface | Properties asserted |
| --- | --- | --- |
| `name_sanitize` | every peer file name, `name::sanitize` | one normal path component, non-empty, ≤ 200 bytes, nothing the oracle forbids, no `/` `\` NUL, no leading dot, no trailing dot or space, no mark without a base or on a dot, mark stacks capped, idempotent; `numbered()` keeps all of it |
| `text_display` | aliases, models, `text::display` | nothing forbidden, character cap with the ellipsis counted, spaces collapsed and trimmed, marks capped, idempotent |
| `text_message` | received text, `text::message` | nothing forbidden but `\n`, ≤ 64 KiB, trimmed, no space at a line edge, ≤ 2 empty lines in a row, no mark without a base, idempotent |
| `offer_validate` | a whole `RawOffer` built with `arbitrary`, `Offer::validate` | no negative or oversized size accepted, exact total within 16 GiB, ≤ 500 files, every name/sender/model/text/MIME/PIN clean; every refusal names a real problem |
| `settings_json` | the settings file and `set_settings`, `serde_json` + `Settings::validate` | device name clean and capped, PIN 1–16 ASCII alphanumerics, URLs printable ASCII ≤ 256 bytes with the right scheme, validation idempotent, save/load round trip exact |
| `hex` | digests and fingerprints, `hex::decode_32` | exactly 64 hex digits decode and nothing else; round trip both ways |
| `command_json` | the FFI command channel, `sukkula_engine::api::parse_command` | > 64 KiB refused before parsing; a parsed command has the current version and survives its own serialiser; a recovered id was really in the input; `set_settings` payloads validate clean |
| `start_config` | `sukkula_start`'s JSON, `sukkula_engine::api::parse_start_config` | > 64 KiB refused; version checked; only known keys accepted; round trip exact |

### The protocol parsers

Every byte a peer, a mailbox server or the LAN sends is parsed by one of
these first (spec §7: "LocalSend DTOs, Quick Share frames, wormhole
offers"). Each goes through the adapter's real code -- upstream's DTOs,
rqs_lib's state machine, the wormhole guards -- via thin `#[doc(hidden)]`
entry points (`sukkula_engine::localsend::{offer_from_prepare_upload,
from_peer}`, `sukkula_engine::wormhole::fuzzing`,
`sukkula_engine::quickshare::fuzzing`), and every offer that comes out
validated is held to the same S-rules as `offer_validate`'s
(`sukkula_fuzz::assert_offer`), every listed peer to S2
(`sukkula_fuzz::assert_peer`). Beyond the S-rules, each target restates what
the input said -- through `serde_json::Value`, a decoder of its own, or the
library's own types -- and asserts the adapter kept it.

| Target | Surface | Properties asserted |
| --- | --- | --- |
| `localsend_prepare_upload` | `POST /prepare-upload`, the offer, from anyone on the LAN | the S-rules; ≤ 64 KiB parsed; HTTPS only (F-LS2); a lone `text/plain` file with a preview is the message and nothing else (F-C4); otherwise every file, in id order, with its declared size exact and its declared digest |
| `localsend_discovery` | the multicast announcement, `POST /register`, the `register` and `info` answers, the `prepare-upload` answer | an announcement is answered only if HTTPS, a port, and a well-formed fingerprint that is not ours, pinned to exactly that fingerprint (F-LS2, F-LS3); a peer is listed only on the fingerprint its certificate proved, never one it claimed, and S2-clean; an upload path has exactly the session, file and token parameters, short and plain, with the receiver's values: nothing smuggled into the request |
| `wormhole_wire` | the v1 peer messages: offers, transit hints, answers, the transit ack | an offer is exactly one text or one file with the declared name and signed size, re-encodes to itself, and validates S-clean; hints bounded (16 direct, 2 relays × 3), deduplicated, well-formed hosts, no port 0; the connection plan is ≤ 4 targets, ours first, and never loopback, unspecified, multicast or broadcast, v4-mapped included; the ack matches only its digest |
| `wormhole_code` | the code the user types (F-MW2), through the library's parser and entropy check | an accepted code is the input trimmed and lowercased, ≤ 128 bytes, in the strict grammar restated here; acceptance is idempotent and case-blind; every refusal is `BadCode` |
| `wormhole_mailbox` | the mailbox guard's filter on everything the server sends, one connection per input | each message let through is re-read with magic-wormhole 0.8.1's own server-message types: hashcash ≤ 20 bits (W1), phases the library cannot `todo!()` on (W2), bodies it cannot `split_at` past (W3), PAKE bodies ≤ 512 bytes; ≤ 128 messages and 4 MiB per connection, nothing read after a refusal (W5) |
| `quickshare_handshake` | the plaintext handshake from the first byte: frame reader, connection request and endpoint info, UKEY2 client init and finish, the peer's P-256 key; then the channel it keys | no event of any kind before the key exchange; an honest connection request's name read exactly and shown S2-clean; a finished exchange gives a four-digit PIN; then everything `quickshare_frame` asserts, under the derived keys |
| `quickshare_frame` | everything inside the encrypted channel: offline frames, byte payloads and their reassembly, sharing frames, the introduction, file chunks, texts, spoiled seals | no file byte or text before the user accepts (S5); ≤ 1000 files, no negative size, no repeated payload id, text ≤ 64 KiB; chunks in order, never past the declared size, ending exactly there; ≤ 2 byte payloads buffered; ≤ 16 KiB of reply per frame; the raw offer is the introduction as sent and validates S-clean; a received text is S2-clean |
| `quickshare_mdns` | an mDNS service's instance name and `n` record, from anyone on the link | a listed peer's id is `qs:` and the endpoint id (letters and digits, or hex), its name exactly S2 of what the record carries, decoded here independently |

The two Quick Share targets play the sender (`src/quickshare.rs`): rqs_lib's
`InboundRequest` reads real frames from a socket that never waits, and the
harness seals what the input asks for under keys it shares with the
receiver -- a fuzzer cannot forge the channel's HMAC, and without this
nothing past the handshake would be reached. `quickshare_handshake` goes
further: it commits to the client finish it will really send, so the
commitment check passes and the key derivation runs on the input's key. The
receiver side does with each event what `quickshare/receive.rs` does.

## Running

```sh
cargo install cargo-fuzz --locked          # once
rustup toolchain install nightly --profile minimal   # once

# One target, one minute, from the repository root:
t=name_sanitize
mkdir -p fuzz/corpus/$t
RUSTUP_TOOLCHAIN=nightly cargo fuzz run --target x86_64-unknown-linux-gnu $t \
    fuzz/corpus/$t fuzz/seeds/$t -- \
    -max_total_time=60 -dict=fuzz/dicts/$t.dict -timeout=10 -rss_limit_mb=4096 -max_len=4096
```

Two corpus directories: libFuzzer writes what it finds into the first
(`fuzz/corpus/`, gitignored) and only reads the second (`fuzz/seeds/`,
committed). So no copy step is needed, and a run never rewrites a committed
seed. `text_message` takes `-max_len=70000`, so that the 64 KiB cap is
reachable; every other target uses 4096.

## CI: the 60-second smoke per target

The `fuzz-smoke` job of `.github/workflows/ci.yml` (and `make fuzz-smoke`,
which runs the same) does two things on every pull request. First it lints
the crate on the pinned toolchain, which also keeps every target compiling:

```sh
cargo clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings
```

Then `scripts/fuzz-smoke.sh 60` runs the smoke (spec §7): it asks `cargo
fuzz list` for the targets -- so a target added to `Cargo.toml` is fuzzed
from the pull request that adds it, and zero targets is a failure -- builds
them all once, and runs each for 60 s from `fuzz/corpus/<t>` and
`fuzz/seeds/<t>` with `fuzz/dicts/<t>.dict`, a 10 s per-input timeout and a
2 GiB RSS cap. It sets `RUSTUP_TOOLCHAIN` and `--target` itself, for the
pitfalls below.

Sixteen targets at 60 s each is sixteen minutes, plus one ASan build of the
protocol libraries of about ten; a matrix over the targets would run them in
parallel instead. On failure the job uploads `fuzz/artifacts/`: that
directory holds the input that failed.

Pitfalls, all met on vuo's `fuzz-smoke` job first:

- **`RUSTUP_TOOLCHAIN=nightly`** is not optional: rust-toolchain.toml pins
  1.97.1, a toolchain file beats the default toolchain a job installs, and
  cargo-fuzz's `-Zsanitizer` then reaches a stable rustc and fails.
  `RUSTUP_TOOLCHAIN` beats the file; `cargo +nightly` works too, but not
  inside scripts that call plain `cargo`.
- **`--target` the host triple**, never cargo-fuzz's own: the prebuilt
  cargo-fuzz is musl-static and defaults to its own triple, and ASan cannot
  link against a static libc ("sanitizer is incompatible with statically
  linked libc", then "can't find crate for `core`").
- **Pass `fuzz/seeds/<t>`**. From an empty corpus, 60 seconds rarely reaches
  the assertions these targets exist for: `settings_json` has to invent
  `"localsend":{"pin":` before it tests a PIN at all.
- **Pass the dictionary**, and keep its grammar: `"token"` per line, and
  inside the quotes only `\\`, `\"` and `\xHH`. `\n`, `\r`, `\t` are not
  escapes to libFuzzer; one bad line and it exits before fuzzing anything.
  The dictionaries here were generated with `\xHH` for every byte outside
  printable ASCII, so they are safe to extend in the same spelling.
- The build is `--release` with debug assertions on (cargo-fuzz's default),
  so overflow checks are live in `sukkula-core` and the engine, as they are
  on the phone (S10).

## A finding

A crash, or a violated property, is written to `fuzz/artifacts/<target>/`:

```sh
RUSTUP_TOOLCHAIN=nightly cargo fuzz run --target x86_64-unknown-linux-gnu \
    <target> fuzz/artifacts/<target>/crash-<hash>
RUSTUP_TOOLCHAIN=nightly cargo fuzz tmin --target x86_64-unknown-linux-gnu \
    <target> fuzz/artifacts/<target>/crash-<hash>
```

Then add the minimised input to the module's own unit tests: the fuzzer
finds a bug once, a unit test keeps it dead. Worthwhile corpus growth can be
folded into `fuzz/seeds/<target>/` after `cargo fuzz cmin`, keeping the
committed seed small and reviewable.

## Seeds

The JSON, code and mDNS targets' seeds are what a real peer sends plus the
hostile variants each target exists for, written out as they are. The two
Quick Share targets decode their input with `arbitrary`, so their seeds are
generated from readable scripts in `src/seeds.rs` -- a sender taken to a
received file, to a text, through the whole handshake with a real key, to
a refusal, a spoiled seal -- encoded into exactly the bytes `arbitrary`
decodes back into each script. Two tests hold them to that, on the pinned
toolchain: each seed decodes to its script, and each reaches the state it
is for (the harness records milestones as it goes). After changing a
seed or an input type, from `fuzz/`:

```sh
SUKKULA_FUZZ_WRITE_SEEDS=seeds cargo test --lib seeds::write   # rewrite them
cargo test --lib                                              # and check them
```

To see what a corpus reaches, run a Quick Share target once over it with
the trace on and count the milestones:

```sh
SUKKULA_FUZZ_TRACE=1 fuzz/target/x86_64-unknown-linux-gnu/release/quickshare_frame \
    -runs=0 fuzz/corpus/quickshare_frame fuzz/seeds/quickshare_frame 2>&1 |
    grep '^MILESTONE' | sort | uniq -c
```

## First runs (2026-09-24)

Each target 60 s from the committed seeds with its dictionary, one core,
ASan, on the development container:

| Target | Executions (per second) | Coverage, start -> end | Findings |
| --- | ---: | ---: | --- |
| `name_sanitize` | 311,568 (5,107) | 329 -> 571 | none |
| `text_display` | 650,359 (10,661) | 222 -> 349 | none |
| `text_message` | 656,088 (10,755) | 213 -> 370 | none |
| `offer_validate` | 109,497 (1,795) | 1111 -> 1274 | none (second run, after making the 500-file and 64 KiB paddings one case in sixteen; the first did 32,750 at 536/s) |
| `settings_json` | 4,750,348 (77,874) | 837 -> 2317 | none |
| `hex` | 7,536,554 (123,550) | 97 -> 97 | none (a dozen lines; saturated at once) |
| `command_json` | 1,091,964 + 3,040,238 | 1348 -> 3480 | one, in the target's own oracle: see below |
| `start_config` | 4,895,638 (80,256) | 478 -> 1558 | none |

The one finding: `{"v":1,"id":9,"cmd":{"type":4e+6666666666666}}`. The
command is refused, correctly, and `parse_command` recovers id 9 for the
reply, correctly; the target then re-checked the id through
`serde_json::Value`, which refuses a number that overflows an `f64` although
JSON's grammar allows it. The check now skips inputs `Value` cannot hold,
and the input is kept as `seeds/command_json/regress-huge-number`.

The property tests found, before any fuzzing, that `sanitize(".\u{301}")`
returned `"\u{301}"` -- a name that is nothing but a combining mark, and
that sanitised to `received-file` the second time. That and the other
findings are in the core's commit history and fixed at the source.

## The protocol targets' first runs (2026-09-24)

Each 60 s with its dictionary, one core, ASan, on the development
container, after shorter runs had grown a corpus (the Quick Share targets:
from their generated seeds alone). Coverage is libFuzzer's, at the start of
the run and at its end.

| Target | Executions (per second) | Coverage, start -> end | Findings |
| --- | ---: | ---: | --- |
| `localsend_prepare_upload` | 1,596,243 (26,167) | 3493 -> 3573 | none |
| `localsend_discovery` | 3,396,058 (55,673) | 3624 -> 3711 | none |
| `wormhole_wire` | 552,242 (9,053) | 3200 -> 3282 | none |
| `wormhole_code` | 2,133 (34) | 9398 -> 9416 | none; see below |
| `wormhole_mailbox` | 874,294 (14,332) | 2437 -> 2496 | none |
| `quickshare_handshake` | 51,290 (840) | 3317 -> 4736 | none |
| `quickshare_frame` | 128,962 (2,114) | 2734 -> 3704 | none |
| `quickshare_mdns` | 3,452,832 (56,603) | 794 -> 800 | none |

Before that, every target ran 10 s, 60 s, and the two Quick Share targets
180 s more, without a finding in engine or library code. Over those runs the
Quick Share corpora reached every milestone the harness records: an
introduction accepted and refused, files and texts received to the end, and
the whole handshake with a real key exchange to a received file.

What the runs changed was the harness. Protobuf enum fields were first
fuzzed as plain `i32`s, so a text's kind or a frame's type took a defined
value about once in a billion inputs, and three minutes never produced a
received text; they are now mostly small numbers (`quickshare::Enum`), and
the seeds are generated so that every state is reached from the first
second. One seed's expectation was wrong in an instructive way: rqs_lib
takes an introduction of up to 1000 files, and `Offer::validate` refuses it
past 500 (S6), so the 996-file seed ends refused -- the adapter's cap, not
the library's, is the one that holds.

`wormhole_code` is slow on purpose: every code that passes the adapter's
grammar goes to the library's entropy estimate (zxcvbn), which under ASan
and coverage instrumentation takes tens of milliseconds. The code is typed
by the user, never sent by a peer, so this is a cost of fuzzing, not an
exposure; the deterministic sweep in `wormhole/sweep.rs` covers the grammar
at full speed on every `cargo test`.
