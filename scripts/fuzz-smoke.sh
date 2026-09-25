#!/usr/bin/env bash
# The short fuzz run spec §7 puts on every change: each cargo-fuzz target in
# fuzz/, for 60 seconds by default, from its committed seeds.
#
#     scripts/fuzz-smoke.sh [seconds-per-target] [target...]
#                                              every target, or the ones named
#     scripts/fuzz-smoke.sh --max-len <target>   print its -max_len, and exit
#     scripts/fuzz-smoke.sh --self-test          prove the lengths, the boundary
#                                              inputs and the verdicts, offline
#
# Targets are asked of `cargo fuzz list`, not listed here, so a target
# added to fuzz/Cargo.toml is fuzzed from the pull request that adds it.
# Zero targets is a failure: a smoke run that fuzzed nothing passed nothing.
# So is a target without committed seeds or a dictionary, and a dictionary
# libFuzzer cannot parse: ci/check-dicts.sh runs first, and says which.
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
#     (fuzz/seeds/<target>/) go in as a second, read-only corpus, with the
#     dictionary (fuzz/dicts/<target>.dict).
#   - libFuzzer's -max_len defaults to the larger of 4096 and the biggest
#     corpus file, and every seed here is far smaller, so a 64 KiB cap was
#     never reached and the assertions on it could not fail. Each target's
#     length is set below, and the capped ones get inputs at the cap and
#     one past it to start from (max_len_for, boundary_seeds).
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

fail() { echo "fuzz-smoke: FAIL $*" >&2; exit 1; }

# The caps the targets assert, read from the source that defines them, so
# a changed limit moves the lengths with it (and a moved definition fails
# here rather than silently fuzzing at the old length).
rust_const() { # file name
    local expr
    expr=$(sed -n "s/^pub const $2: usize = \([0-9 *]*\);\$/\1/p" "$1")
    [[ $expr =~ ^[0-9]+( \* [0-9]+)*$ ]] || fail "cannot read $2 from $1"
    echo $((expr))
}
MSG_CAP=$(rust_const crates/sukkula-core/src/limits.rs MAX_MESSAGE_BYTES)
SETTINGS_CAP=$(rust_const crates/sukkula-core/src/config.rs MAX_SETTINGS_BYTES)

# The longest input libFuzzer may make, per target -- clove's
# ci/fuzz.sh --max-len, for the reason above. fuzz/README.md has the table.
MAX_LEN_DEFAULT=4096
# "Over 64 KiB is refused" (MAX_MESSAGE_BYTES): a received text's cap, the
# FFI command's, sukkula_start's JSON's, and the prepare-upload body's.
# The length has to reach past the cap for those assertions to be able to
# fail at all. The JSON ones take whitespace-padded boundary inputs, the
# text a repeated one (boundary_seeds). --self-test fails if a name here
# is not a target, so a rename cannot drop one back to the default.
CAP64K_JSON=(command_json start_config localsend_prepare_upload)
CAP64K_TEXT=(text_message)
CAP64K=("${CAP64K_JSON[@]}" "${CAP64K_TEXT[@]}")
# Every size of settings file the store will read; the target returns
# early past it, as the store does.
CAP_SETTINGS=(settings_json)
in_list() { # word list...
    local w=$1 x
    shift
    for x in "$@"; do [[ "$x" = "$w" ]] && return 0; done
    return 1
}
max_len_for() {
    if in_list "$1" "${CAP64K[@]}"; then
        echo $((MSG_CAP + 4096))
    elif in_list "$1" "${CAP_SETTINGS[@]}"; then
        echo "$SETTINGS_CAP"
    else
        # Everything else asserts caps a 4 KiB input reaches, or that the
        # harness builds from a small input itself (offer_validate's 500
        # files and 64 KiB text, the Quick Share scripts' declared sizes).
        # The mailbox guard's 4 MiB per connection is beyond any practical
        # fuzz length; mailbox.rs's the_budget_is_per_connection holds it.
        echo "$MAX_LEN_DEFAULT"
    fi
}

# fuzz_args <target> <seconds>: libFuzzer's options for one target, one
# per line. The one place they are made, so the self-test reads what the
# run passes.
fuzz_args() {
    printf '%s\n' -max_total_time="$2" -max_len="$(max_len_for "$1")" -rss_limit_mb=2048 \
        -timeout=10 -print_final_stats=1 -dict="fuzz/dicts/$1.dict"
}

# boundary_seeds <target> <dir>: inputs exactly at the target's cap and
# one byte past it, made from its first committed seed, written to <dir>.
# Mutation does not grow a 60-byte seed to 64 KiB in a minute; from these
# it starts on both sides of the edge. Made here rather than committed, so
# they follow the constant: the JSON targets' seed padded with whitespace
# (still the same JSON), the text's repeated.
boundary_seeds() {
    local t=$1 dir=$2 first size
    first=$(find "fuzz/seeds/$t" -type f | sort | head -1)
    mkdir -p "$dir"
    if in_list "$t" "${CAP64K_JSON[@]}"; then
        size=$(stat -c %s "$first")
        [[ "$size" -lt "$MSG_CAP" ]] || fail "$first is already past the cap"
        { cat "$first"; head -c $((MSG_CAP - size)) /dev/zero | tr '\0' ' '; } > "$dir/at-cap"
        { cat "$dir/at-cap"; printf ' '; } > "$dir/past-cap"
    elif in_list "$t" "${CAP64K_TEXT[@]}"; then
        { cat "$first"; printf ' '; } > "$dir/long"
        while [[ $(stat -c %s "$dir/long") -le "$MSG_CAP" ]]; do
            cat "$dir/long" "$dir/long" > "$dir/twice"
            mv "$dir/twice" "$dir/long"
        done
        head -c "$MSG_CAP" "$dir/long" > "$dir/at-cap"
        head -c $((MSG_CAP + 1)) "$dir/long" > "$dir/past-cap"
        rm "$dir/long"
    fi
    return 0
}

# A failed run is a finding only if libFuzzer wrote the input it failed
# on: it does so before it exits. A failure that left nothing new in
# fuzz/artifacts/<t>/ is the harness's, an option's or a dictionary's.
# new_artifacts <listing before> <dir>: what appeared in <dir> since.
new_artifacts() {
    comm -13 <(printf '%s\n' "$1") <(find "$2" -type f 2>/dev/null | sort || true) | grep -v '^$' || true
}
verdict() { # target rc new-artifacts
    if [[ -n "$3" ]]; then
        echo "fuzz-smoke: FAIL $1 found something; the reproducer is $(head -1 <<< "$3")"
    else
        echo "fuzz-smoke: FAIL $1: libFuzzer exited $2 without writing a reproducer --" \
             "not a finding but a broken run (an option, the build, the harness); see the log above"
    fi
}

# The fuzz crate's targets as fuzz/Cargo.toml lists them ([[bin]] names,
# what `cargo fuzz list` reads), for the self-test, which needs no cargo.
cargo_targets() {
    awk '/^\[\[bin\]\]/ { bin = 1; next } /^\[/ { bin = 0 }
         bin && /^name[[:space:]]*=/ { v = $0; sub(/^[^"]*"/, "", v); sub(/".*/, "", v); print v }' fuzz/Cargo.toml
}

# --- the self-test ----------------------------------------------------------
# Both are called through the self-test's `check`, which shellcheck
# cannot follow.
# shellcheck disable=SC2317
contains() { [[ "$1" == *"$2"* ]]; }
# padded_with_spaces <seed> <file>: <file> is <seed>'s bytes, then spaces.
# shellcheck disable=SC2317
padded_with_spaces() {
    local n
    n=$(stat -c %s "$1")
    cmp -s -n "$n" "$1" "$2" && [[ -z "$(tail -c +$((n + 1)) "$2" | tr -d ' ')" ]]
}
# What finding "fuzz-smoke passes no -max_len" was, held in place without
# nightly or cargo-fuzz: every capped target is a real target, its
# -max_len reaches past its cap, the run passes that length, its boundary
# inputs are exactly the cap and one byte more and fit in the length (so
# libFuzzer does not cut them), and a failure is called a finding only
# with a reproducer.
self_test() {
    local status=0 cases=0 t len got work first before
    work=$(mktemp -d)
    check() { # what, then a command that must succeed
        local what=$1
        shift
        cases=$((cases + 1))
        if "$@"; then echo "fuzz-smoke: self-test ok   $what"
        else echo "fuzz-smoke: self-test FAIL $what" >&2; status=1; fi
    }
    local -a targets
    mapfile -t targets < <(cargo_targets)
    check "fuzz/Cargo.toml lists targets" test "${#targets[@]}" -gt 0
    for t in "${CAP64K[@]}" "${CAP_SETTINGS[@]}"; do
        check "$t, capped here, is a target" in_list "$t" "${targets[@]}"
    done
    for t in "${targets[@]}"; do
        len=$(max_len_for "$t")
        got=$(fuzz_args "$t" 60 | grep -c -x -e "-max_len=$len" -e "-dict=fuzz/dicts/$t.dict" || true)
        check "$t runs with -max_len=$len and its dictionary" test "$got" -eq 2
    done
    for t in "${CAP64K[@]}"; do
        len=$(max_len_for "$t")
        check "$t's -max_len passes the 64 KiB cap" test "$len" -gt "$MSG_CAP"
        boundary_seeds "$t" "$work/$t"
        check "$t starts from an input of exactly the cap" test "$(stat -c %s "$work/$t/at-cap")" -eq "$MSG_CAP"
        check "$t starts from an input one byte past the cap" test "$(stat -c %s "$work/$t/past-cap")" -eq $((MSG_CAP + 1))
        check "$t's longest input fits its -max_len" test $((MSG_CAP + 1)) -le "$len"
    done
    for t in "${CAP64K_JSON[@]}"; do
        first=$(find "fuzz/seeds/$t" -type f | sort | head -1)
        check "$t's boundary input is its seed, padded with whitespace" \
            padded_with_spaces "$first" "$work/$t/at-cap"
    done
    check "settings_json runs at the store's read limit" test "$(max_len_for settings_json)" -eq "$SETTINGS_CAP"
    check "an uncapped target runs at $MAX_LEN_DEFAULT" test "$(max_len_for hex)" -eq "$MAX_LEN_DEFAULT"
    mkdir -p "$work/artifacts"
    : > "$work/artifacts/old"
    before=$(find "$work/artifacts" -type f | sort)
    check "a failure with no new artifact is a broken run" \
        contains "$(verdict x 1 "$(new_artifacts "$before" "$work/artifacts")")" "broken run"
    : > "$work/artifacts/crash-1"
    check "a failure with a new artifact is a finding, and names it" \
        contains "$(verdict x 1 "$(new_artifacts "$before" "$work/artifacts")")" \
        "found something; the reproducer is $work/artifacts/crash-1"
    rm -rf "$work"
    if [[ "$status" -eq 0 ]]; then echo "fuzz-smoke: self-test ok ($cases cases)"
    else echo "fuzz-smoke: self-test FAILED" >&2; fi
    return "$status"
}

case "${1:-}" in
    --max-len)
        [[ -n "${2:-}" ]] || fail "--max-len needs a target"
        max_len_for "$2"
        exit 0
        ;;
    --self-test) self_test; exit ;;
esac

secs="${1:-60}"
[[ $secs =~ ^[0-9]+$ ]] || { echo "usage: $0 [seconds-per-target] [target...] | --max-len <target> | --self-test" >&2; exit 2; }
[[ $# -gt 0 ]] && shift
only=("$@")
export RUSTUP_TOOLCHAIN="${FUZZ_TOOLCHAIN:-nightly-2026-09-15}"

[[ -f fuzz/Cargo.toml ]] || fail "no fuzz/Cargo.toml; the fuzz crate is missing"
command -v cargo-fuzz >/dev/null 2>&1 || fail "cargo-fuzz is not installed (cargo install cargo-fuzz --locked)"
rustc --version >/dev/null 2>&1 || fail "$RUSTUP_TOOLCHAIN is not installed (rustup toolchain install $RUSTUP_TOOLCHAIN)"

# Seeds and a parseable dictionary for every target, before anything is
# built: libFuzzer exits before fuzzing on a bad dictionary line, and that
# is a broken gate, not a finding.
"$ROOT/ci/check-dicts.sh" || fail "the fuzz inputs are incomplete; see above (ci/check-dicts.sh)"

host=$(rustc -vV | sed -n 's/^host: //p')
echo "== $(rustc --version), target $host, ${secs}s per target"

mapfile -t targets < <(cargo fuzz list 2>/dev/null | grep -v '^[[:space:]]*$')
[[ ${#targets[@]} -gt 0 ]] || fail "cargo fuzz list found no targets in fuzz/Cargo.toml"
# Named targets narrow the run (to reproduce one); CI names none.
if [[ ${#only[@]} -gt 0 ]]; then
    for t in "${only[@]}"; do
        in_list "$t" "${targets[@]}" || fail "'$t' is not a target (cargo fuzz list: ${targets[*]})"
    done
    targets=("${only[@]}")
fi
echo "== targets: ${targets[*]}"
for t in "${targets[@]}"; do
    # cargo fuzz's list and check-dicts' read of Cargo.toml should agree;
    # this holds even where they do not.
    [[ -f "fuzz/dicts/$t.dict" ]] || fail "$t has no fuzz/dicts/$t.dict"
    [[ -n "$(find "fuzz/seeds/$t" -type f -print -quit 2>/dev/null)" ]] ||
        fail "$t has no committed seeds in fuzz/seeds/$t/"
done

# Built together first, so a target that does not compile is reported as
# that rather than as a fuzz failure.
cargo fuzz build --target "$host" || fail "the fuzz targets do not build"

gen=$(mktemp -d)
trap 'rm -rf "$gen"' EXIT
status=0
for t in "${targets[@]}"; do
    corpus="fuzz/corpus/$t"
    mkdir -p "$corpus"
    boundary_seeds "$t" "$gen/$t"
    dirs=("$corpus" "fuzz/seeds/$t")
    [[ -n "$(ls -A "$gen/$t")" ]] && dirs+=("$gen/$t")
    mapfile -t args < <(fuzz_args "$t" "$secs")
    echo
    echo "== $t: ${dirs[*]}, ${args[*]}"
    before=$(find "fuzz/artifacts/$t" -type f 2>/dev/null | sort || true)
    rc=0
    cargo fuzz run --target "$host" "$t" "${dirs[@]}" -- "${args[@]}" || rc=$?
    [[ "$rc" -eq 0 ]] && continue
    status=1
    verdict "$t" "$rc" "$(new_artifacts "$before" "fuzz/artifacts/$t")" >&2
done

[[ "$status" -eq 0 ]] && echo "fuzz-smoke: ok (${#targets[@]} targets, ${secs}s each)"
exit "$status"
