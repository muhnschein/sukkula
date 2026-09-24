#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The C harness for sukkula-ffi under AddressSanitizer, UndefinedBehavior-
# Sanitizer and LeakSanitizer (docs/SPEC.md section 7, "FFI").
#
#   ci/ffi-harness/run.sh
#
# from any directory. It builds sukkula-ffi as a static library for the
# host, asks rustc which system libraries that library needs, compiles
# harness.c with -fsanitize=address,undefined against it, and runs it.
# Exit status 0 means every check passed and the sanitizers found nothing;
# anything else is a failure, with the reason on stdout/stderr.
#
# Needs: the pinned Rust toolchain (rust-toolchain.toml), and clang or gcc
# with the sanitizer runtimes (Ubuntu: clang, or gcc with libasan/libubsan).
# No network: cargo builds from the lock file and the local registry cache.
#
# Environment (all optional):
#   CC                      the C compiler (default: the first of clang, gcc,
#                           cc that can link a sanitized program)
#   CARGO_BUILD_JOBS        cargo parallelism (default: 2)
#   FFI_HARNESS_CARGO_ARGS  extra cargo arguments, e.g. "--no-default-features"
#                           to build the engine without any protocol
#   FFI_HARNESS_TIMEOUT     seconds the harness may run (default: 600)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cd "$root"

CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
export CARGO_BUILD_JOBS

fail() {
    echo "ffi-harness: $*" >&2
    exit 1
}

work=$(mktemp -d "${TMPDIR:-/tmp}/sukkula-ffi-harness.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

# A compiler that can link a sanitized program: an installed clang often
# lacks its runtime (Ubuntu splits it into libclang-rt-*-dev), gcc's comes
# with libasan/libubsan.
sanitizers_link() {
    printf 'int main(void) { return 0; }\n' >"$work/probe.c"
    "$1" -fsanitize=address,undefined "$work/probe.c" -o "$work/probe" \
        >/dev/null 2>&1 && "$work/probe"
}
if [ -n "${CC:-}" ]; then
    sanitizers_link "$CC" || fail "$CC cannot link -fsanitize=address,undefined"
else
    for candidate in clang gcc cc; do
        if command -v "$candidate" >/dev/null 2>&1 && sanitizers_link "$candidate"; then
            CC=$candidate
            break
        fi
    done
    [ -n "${CC:-}" ] ||
        fail "no C compiler links -fsanitize=address,undefined (install gcc, or clang with libclang-rt-dev)"
fi
echo "ffi-harness: CC=$CC ($("$CC" --version | head -n 1))"

# The static library, and rustc's own list of what it links against. The
# artifact path comes from cargo's JSON messages, so a configured target
# directory is found without guessing; the note is rendered on stderr.
# shellcheck disable=SC2086 # FFI_HARNESS_CARGO_ARGS is a list of words.
if ! cargo rustc --locked -p sukkula-ffi --lib --crate-type staticlib \
    ${FFI_HARNESS_CARGO_ARGS:-} --message-format=json-render-diagnostics \
    -- --print native-static-libs >"$work/cargo.json" 2>"$work/cargo.log"; then
    cat "$work/cargo.log" >&2
    fail "cargo could not build the static library"
fi
lib=$(grep -o '"filenames":\[[^]]*\]' "$work/cargo.json" |
    grep -o '[^"]*libsukkula_ffi\.a' | tail -n 1)
if [ -z "$lib" ] || [ ! -f "$lib" ]; then
    cat "$work/cargo.log" >&2
    fail "cargo reported no libsukkula_ffi.a"
fi
native=$(sed -n 's/^note: native-static-libs: //p' "$work/cargo.log" | tail -n 1)
[ -n "$native" ] || {
    cat "$work/cargo.log" >&2
    fail "rustc printed no native-static-libs line"
}
echo "ffi-harness: $lib"
echo "ffi-harness: links $native"

# -fno-sanitize-recover makes every UBSan finding fatal. -O1 keeps ASan's
# reports precise without hiding bugs behind -O0-only stack layouts.
# shellcheck disable=SC2086 # $native is a list of linker flags.
"$CC" -std=c11 -g -O1 -Wall -Wextra -Werror -pedantic -pthread \
    -fsanitize=address,undefined -fno-sanitize-recover=all \
    -fno-omit-frame-pointer \
    -I "$root/crates/sukkula-ffi/include" \
    "$here/harness.c" "$lib" $native \
    -o "$work/harness" ||
    fail "the harness did not compile or link"

# halt_on_error: the first finding ends the run with a non-zero status.
# detect_leaks: LeakSanitizer runs at exit over everything the Rust side
# allocated too, since ASan intercepts malloc for the whole process.
ASAN_OPTIONS="detect_leaks=1:halt_on_error=1:abort_on_error=0:detect_stack_use_after_return=1:strict_string_checks=1:check_initialization_order=1:strict_init_order=1:allocator_may_return_null=0"
UBSAN_OPTIONS="print_stacktrace=1:halt_on_error=1"
LSAN_OPTIONS="report_objects=1"
export ASAN_OPTIONS UBSAN_OPTIONS LSAN_OPTIONS

TMPDIR=$work timeout "${FFI_HARNESS_TIMEOUT:-600}" "$work/harness" ||
    fail "the harness failed (status $?)"
echo "ffi-harness: PASS"
