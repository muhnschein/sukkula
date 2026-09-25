#!/usr/bin/env bash
# The short fuzz run spec §7 puts on every change: each cargo-fuzz target in
# fuzz/, for 60 seconds by default, from its committed seeds.
#
#     scripts/fuzz-smoke.sh [seconds-per-target]
#     scripts/fuzz-smoke.sh --max-len <target>   print its -max_len, and exit
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
max_len_for() {
    case "$1" in
        # "Over 64 KiB is refused" (MAX_MESSAGE_BYTES): a received text's
        # cap, the FFI command's, sukkula_start's JSON's, and the
        # prepare-upload body's. The length has to reach past the cap for
        # those assertions to be able to fail at all.
        text_message | command_json | start_config | localsend_prepare_upload)
            echo $((MSG_CAP + 4096)) ;;
        # Every size of settings file the store will read.
        settings_json) echo "$SETTINGS_CAP" ;;
        # Everything else asserts caps a 4 KiB input reaches, or that the
        # harness builds from a small input itself (offer_validate's 500
        # files and 64 KiB text, the Quick Share scripts' sizes). The
        # mailbox guard's 4 MiB per connection is beyond any practical
        # fuzz length; mailbox.rs's the_budget_is_per_connection holds it.
        *) echo "$MAX_LEN_DEFAULT" ;;
    esac
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
    case "$t" in
        command_json | start_config | localsend_prepare_upload)
            size=$(stat -c %s "$first")
            [[ "$size" -lt "$MSG_CAP" ]] || fail "$first is already past the cap"
            { cat "$first"; head -c $((MSG_CAP - size)) /dev/zero | tr '\0' ' '; } > "$dir/at-cap"
            { cat "$dir/at-cap"; printf ' '; } > "$dir/past-cap"
            ;;
        text_message)
            { cat "$first"; printf ' '; } > "$dir/long"
            while [[ $(stat -c %s "$dir/long") -le "$MSG_CAP" ]]; do
                cat "$dir/long" "$dir/long" > "$dir/twice"
                mv "$dir/twice" "$dir/long"
            done
            head -c "$MSG_CAP" "$dir/long" > "$dir/at-cap"
            head -c $((MSG_CAP + 1)) "$dir/long" > "$dir/past-cap"
            rm "$dir/long"
            ;;
        *) ;;
    esac
}

if [[ "${1:-}" = --max-len ]]; then
    [[ -n "${2:-}" ]] || fail "--max-len needs a target"
    max_len_for "$2"
    exit 0
fi

secs="${1:-60}"
[[ $secs =~ ^[0-9]+$ ]] || { echo "usage: $0 [seconds-per-target] | --max-len <target>" >&2; exit 2; }
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
    max_len=$(max_len_for "$t")
    args=(-max_total_time="$secs" -max_len="$max_len" -rss_limit_mb=2048 -timeout=10
          -print_final_stats=1 -dict="fuzz/dicts/$t.dict")
    echo
    echo "== $t: ${dirs[*]}, fuzz/dicts/$t.dict, -max_len=$max_len"
    before=$(find "fuzz/artifacts/$t" -type f 2>/dev/null | sort || true)
    rc=0
    cargo fuzz run --target "$host" "$t" "${dirs[@]}" -- "${args[@]}" || rc=$?
    [[ "$rc" -eq 0 ]] && continue
    status=1
    # A finding is an input; without one this was not a finding. libFuzzer
    # writes the input it failed on before it exits, so a failure that left
    # nothing new in fuzz/artifacts/ is the harness's or an option's, and
    # the log above says which.
    new=$(comm -13 <(printf '%s\n' "$before") \
                   <(find "fuzz/artifacts/$t" -type f 2>/dev/null | sort || true) | grep -v '^$' || true)
    if [[ -n "$new" ]]; then
        echo "fuzz-smoke: FAIL $t found something; the reproducer is $(head -1 <<< "$new")" >&2
    else
        echo "fuzz-smoke: FAIL $t: libFuzzer exited $rc without writing a reproducer --" \
             "not a finding but a broken run (an option, the build, the harness); see the log above" >&2
    fi
done

[[ "$status" -eq 0 ]] && echo "fuzz-smoke: ok (${#targets[@]} targets, ${secs}s each)"
exit "$status"
