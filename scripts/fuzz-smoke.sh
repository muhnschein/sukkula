#!/usr/bin/env bash
# The short fuzz run spec §7 puts on every change: each cargo-fuzz target in
# fuzz/, for 60 seconds by default, from its committed seeds.
#
#     scripts/fuzz-smoke.sh [seconds-per-target]
#
# Targets are asked of `cargo fuzz list`, not listed here, so a target
# added to fuzz/Cargo.toml is fuzzed from the pull request that adds it.
# Zero targets is a failure: a smoke run that fuzzed nothing passed nothing.
#
# The pitfalls, each of which has cost a sibling project a red CI run for a
# reason unrelated to the code (vuo's ci.yml):
#
#   - rust-toolchain.toml pins stable for everyone and overrides the
#     toolchain a job installed, so the sanitizer flags reached a stable
#     rustc. RUSTUP_TOOLCHAIN outranks the file.
#   - a prebuilt cargo-fuzz is a static musl binary and defaults --target to
#     its own triple; a sanitizer cannot link against a static libc
#     ("sanitizer is incompatible with statically linked libc", then
#     "can't find crate for core"). --target is the real host's.
#   - fuzz/corpus/ is gitignored, so from an empty corpus 60 seconds never
#     reaches the code a target exists for. The committed seeds
#     (fuzz/seeds/<target>/) go in as a second, read-only corpus, and the
#     dictionary (fuzz/dicts/<target>.dict, or fuzz/<target>.dict) when
#     there is one.
#
# Every input is bounded the way the engine's own limits are (spec S4, S6):
# a 2 GiB RSS cap, so an unbounded allocation is a finding rather than a
# slow machine, and a 10-second per-input timeout, so a hang is too.
#
#   FUZZ_TOOLCHAIN  the nightly to fuzz with, pinned (default below) so a
#                   nightly regression is not a red pull request
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

secs="${1:-60}"
[[ $secs =~ ^[0-9]+$ ]] || { echo "usage: $0 [seconds-per-target]" >&2; exit 2; }
export RUSTUP_TOOLCHAIN="${FUZZ_TOOLCHAIN:-nightly-2026-09-15}"

fail() { echo "fuzz-smoke: FAIL $*" >&2; exit 1; }
[[ -f fuzz/Cargo.toml ]] || fail "no fuzz/Cargo.toml; the fuzz crate is missing"
command -v cargo-fuzz >/dev/null 2>&1 || fail "cargo-fuzz is not installed (cargo install cargo-fuzz --locked)"
rustc --version >/dev/null 2>&1 || fail "$RUSTUP_TOOLCHAIN is not installed (rustup toolchain install $RUSTUP_TOOLCHAIN)"

host=$(rustc -vV | sed -n 's/^host: //p')
echo "== $(rustc --version), target $host, ${secs}s per target"

mapfile -t targets < <(cargo fuzz list 2>/dev/null | grep -v '^[[:space:]]*$')
[[ ${#targets[@]} -gt 0 ]] || fail "cargo fuzz list found no targets in fuzz/Cargo.toml"
echo "== targets: ${targets[*]}"

# Built together first, so a target that does not compile is reported as
# that rather than as a fuzz failure.
cargo fuzz build --target "$host" || fail "the fuzz targets do not build"

status=0
for t in "${targets[@]}"; do
    corpus="fuzz/corpus/$t"
    mkdir -p "$corpus"
    dirs=("$corpus")
    [[ -d "fuzz/seeds/$t" ]] && dirs+=("fuzz/seeds/$t")
    args=(-max_total_time="$secs" -rss_limit_mb=2048 -timeout=10 -print_final_stats=1)
    used="no dictionary"
    for dict in "fuzz/dicts/$t.dict" "fuzz/$t.dict"; do
        if [[ -f "$dict" ]]; then
            args+=(-dict="$dict")
            used=$dict
            break
        fi
    done
    echo
    echo "== $t: ${dirs[*]}, $used"
    if ! cargo fuzz run --target "$host" "$t" "${dirs[@]}" -- "${args[@]}"; then
        echo "fuzz-smoke: FAIL $t found something; the reproducer is in fuzz/artifacts/$t/" >&2
        status=1
    fi
done

[[ "$status" -eq 0 ]] && echo "fuzz-smoke: ok (${#targets[@]} targets, ${secs}s each)"
exit "$status"
