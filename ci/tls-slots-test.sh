#!/bin/bash
# The engine against the phone's GL stack, as far as thread-locals go:
# start it on a thread whose bionic TLS slots have already been written,
# under qemu-aarch64, and check that it runs and that it leaves the slots
# alone (src/tls_reserve.c).
#
#     ci/tls-slots-test.sh
#
# On the Jolla Phone 2026 the engine failed to start from the first RPM:
# EGL and GL, Android's, run under libhybris, write TLS_SLOT_OPENGL and
# TLS_SLOT_OPENGL_API -- the words at tp+24 and tp+32 -- on the GUI thread
# before QML starts the engine, and glibc had put the engine's
# thread-locals at tp+16, tokio's runtime context first. Entering the
# runtime then panicked: "RefCell already borrowed". Here the program
# writes every slot from TLS_SLOT_APP to TLS_SLOT_ART_THREAD_SELF (tp+16 to
# tp+63) itself, then starts and stops the engine through the C ABI:
#
#   with src/tls_reserve.c linked first, as harbour-sukkula.pro links it,
#       the engine starts, the reserve reports itself in place, and the
#       slots still hold what was written;
#   without it, the engine must fail one of those -- the negative control,
#       which proves the test still reaches the hazard: were the engine's
#       thread-locals ever laid out where the writes miss them, this test
#       would pass whatever the reserve did, and says so instead.
#
# Needs target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.a
# (scripts/cross-build-rust.sh), aarch64-linux-gnu-gcc with its sysroot in
# /usr/aarch64-linux-gnu, and qemu-aarch64 (Ubuntu: qemu-user). Run by
# ci.yml's `cross` job after the cross build.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
lib="$root/target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.a"
cc=${CC_AARCH64:-aarch64-linux-gnu-gcc}
nm=${NM_AARCH64:-aarch64-linux-gnu-nm}
qemu=${QEMU_AARCH64:-qemu-aarch64}
sysroot=${AARCH64_SYSROOT:-/usr/aarch64-linux-gnu}

fail() { echo "tls-slots-test: FAIL $*" >&2; exit 1; }
[[ -f "$lib" ]] || fail "no $lib; run scripts/cross-build-rust.sh first"
for tool in "$cc" "$nm" "$qemu"; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool not found"
done
[[ -e "$sysroot/lib/ld-linux-aarch64.so.1" ]] || fail "no aarch64 sysroot at $sysroot"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# A libdbus-1 to run against: every dbus_* function the archive calls,
# returning 0 -- no bus, which the engine meets as a missing BlueZ. Under
# libdbus-1's soname and version node, so the link is the real one's.
syms=$("$nm" -u "$lib" 2>/dev/null | awk '{print $NF}' | grep '^dbus_' | sort -u || true)
mkdir -p "$work/lib"
for s in $syms; do echo "long $s(void) { return 0; }"; done > "$work/dbus.c"
printf 'LIBDBUS_1_3 { global: *; };\n' > "$work/dbus.map"
"$cc" -shared -fPIC -Wl,-soname,libdbus-1.so.3 -Wl,--version-script="$work/dbus.map" \
    -o "$work/lib/libdbus-1.so.3" "$work/dbus.c"
ln -s libdbus-1.so.3 "$work/lib/libdbus-1.so"

cat > "$work/slots.c" <<'EOF'
/* What the GL stack does to the GUI thread before the engine starts. */
#include <stdio.h>
#include <string.h>
#include "sukkula.h"
#ifdef WITH_RESERVE
#include "tls_reserve.h"
#endif

/* Stand-ins for the hook tables EGL and GL keep in the slots. */
static char gl_state[8];

static void on_event(const char *event_json, void *userdata)
{
    (void)userdata;
    fprintf(stderr, "event: %.100s\n", event_json);
}

int main(int argc, char **argv)
{
    void **tp;
    int slot, failed = 0;

    if (argc != 2)
        return 2;
#ifdef WITH_RESERVE
    if (!sukkula_tls_reserved()) {
        fputs("the reserve is not at tp+16\n", stderr);
        failed = 1;
    }
#endif
    __asm__ volatile("mrs %0, tpidr_el0" : "=r"(tp));
    /* TLS_SLOT_APP (2) to TLS_SLOT_ART_THREAD_SELF (7). */
    for (slot = 2; slot < 8; slot++)
        tp[slot] = &gl_state[slot];

    SukkulaEngine *engine = sukkula_start(argv[1], on_event, NULL);
    if (engine == NULL) {
        fputs("the engine did not start\n", stderr);
        failed = 1;
    } else {
        sukkula_stop(engine);
    }
    for (slot = 2; slot < 8; slot++) {
        if (tp[slot] != &gl_state[slot]) {
            fprintf(stderr, "slot %d was overwritten\n", slot);
            failed = 1;
        }
    }
    return failed;
}
EOF

# Linked as scripts/cross-build-rust.sh links its probe, which is as
# harbour-sukkula.pro links the shell.
native="-ldbus-1 -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc"
link() {
    local out=$1
    shift
    # shellcheck disable=SC2086 # a word list on purpose
    "$cc" -O2 -fPIE -pie -I"$root/crates/sukkula-ffi/include" -I"$root/src" -o "$out" "$@" \
        -L"$work/lib" -Wl,-z,relro -Wl,-z,now -Wl,--as-needed "$lib" $native
}
link "$work/reserved" "$root/src/tls_reserve.c" -DWITH_RESERVE "$work/slots.c"
link "$work/unreserved" "$work/slots.c"

# run <binary>: the engine's two directories, siblings, as the shell's are.
run() {
    local home
    home=$(mktemp -d "$work/home.XXXXXX")
    mkdir -p "$home/share/sukkula" "$home/Downloads"
    local config
    config=$(printf '{"v":1,"data_dir":"%s/share/sukkula","download_dir":"%s/Downloads/Sukkula"}' \
        "$home" "$home")
    RUST_BACKTRACE=0 timeout 120 "$qemu" -L "$sysroot" -E LD_LIBRARY_PATH="$work/lib" \
        "$1" "$config"
}

echo "== with src/tls_reserve.c first"
if ! run "$work/reserved" > "$work/reserved.log" 2>&1; then
    cat "$work/reserved.log" >&2
    fail "the engine did not survive written TLS slots with the reserve in place"
fi
grep -q '"type":"started"' "$work/reserved.log" || {
    cat "$work/reserved.log" >&2
    fail "the engine never reported itself started"
}
echo "tls-slots-test:   ok   started, stopped, and the slots kept what was written"

echo "== without it (negative control)"
if run "$work/unreserved" > "$work/unreserved.log" 2>&1; then
    cat "$work/unreserved.log" >&2
    fail "the engine survived written slots with no reserve: the test no longer reaches the hazard it exists for"
fi
echo "tls-slots-test:   ok   failed, as the phone did: $(grep -m1 -E 'panicked|did not start|overwritten' "$work/unreserved.log" || echo "exit status")"

echo "tls-slots-test: ok"
