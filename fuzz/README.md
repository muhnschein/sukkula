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

The protocol parsers (LocalSend DTOs, Quick Share frames, wormhole offers)
belong to the adapter owners and get targets of their own there; every one
of them ends in `Offer::validate`, which `offer_validate` covers.

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

Two jobs. The first runs on every push on the pinned toolchain and keeps the
targets compiling:

```sh
cargo check --manifest-path fuzz/Cargo.toml --locked
```

The second is the fuzz smoke (spec §7). Exact commands, for a GitHub Actions
`ubuntu-latest` runner with `dtolnay/rust-toolchain@nightly` and cargo-fuzz
from `taiki-e/install-action`:

```sh
set -eu
# rust-toolchain.toml pins 1.97.1, and a toolchain file beats the default
# toolchain the nightly action installs; cargo-fuzz's -Zsanitizer then
# reaches a stable rustc and fails. RUSTUP_TOOLCHAIN beats the file.
export RUSTUP_TOOLCHAIN=nightly
# cargo-fuzz defaults --target to the triple it was built for, and the
# prebuilt binary is musl-static: ASan cannot link against a static libc
# ("sanitizer is incompatible with statically linked libc", then "can't find
# crate for `core`"). Pin the runner's own host triple.
HOST=$(rustc -vV | sed -n 's/^host: //p')
for t in name_sanitize text_display text_message offer_validate \
         settings_json hex command_json start_config; do
    maxlen=4096
    [ "$t" = text_message ] && maxlen=70000
    mkdir -p "fuzz/corpus/$t"
    cargo fuzz run --target "$HOST" "$t" "fuzz/corpus/$t" "fuzz/seeds/$t" -- \
        -max_total_time=60 -dict="fuzz/dicts/$t.dict" -timeout=10 \
        -rss_limit_mb=4096 -max_len="$maxlen"
done
```

Run from the repository root. Eight targets at 60 s each is eight minutes
plus one ASan build of about a minute; a matrix over the targets runs them
in parallel instead. On failure, upload `fuzz/artifacts/` as a job artifact:
that directory holds the input that failed.

Pitfalls, all met on vuo's `fuzz-smoke` job first:

- **`RUSTUP_TOOLCHAIN=nightly`** is not optional (see above); `cargo +nightly`
  works too, but not inside scripts that call plain `cargo`.
- **`--target` the host triple**, never cargo-fuzz's own.
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
