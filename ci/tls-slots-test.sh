#!/bin/bash
# The engine's thread-locals against the phone, under qemu-aarch64: the
# GL stack writing bionic's TLS slots, and the silica-qt5 booster
# dlopen()ing the binary (src/tls_reserve.c; docs/FFI.md, Linking).
#
#     ci/tls-slots-test.sh
#
# On the Jolla Phone 2026 the engine failed to start from the first RPM.
# EGL and GL (Android's, run under libhybris) write TLS_SLOT_OPENGL and
# TLS_SLOT_OPENGL_API, the words at tp+24 and tp+32, on the GUI thread
# before QML starts the engine. glibc had put the engine's thread-locals
# at tp+16, tokio's runtime context first, so entering the runtime
# panicked: "RefCell already borrowed". Launched from the app grid it
# would have failed another way: the booster dlopen()s the binary, which
# gets no fixed offset from the thread pointer for thread-locals of its
# own, so the engine's, linked into it, landed on the booster's.
#
# Each case below starts and stops the engine through the C ABI, after
# writing every slot from TLS_SLOT_APP to TLS_SLOT_ART_THREAD_SELF
# (tp+16 to tp+63) itself:
#
#   exec'd, as the package links it -- src/tls_reserve.c first, the engine
#       in libsukkula_ffi.so: it starts, the reserve is in place, and the
#       slots still hold what was written;
#   dlopen()ed the same way by a stand-in booster with thread-locals of
#       its own: the same, and the booster's are untouched.
#
# Each also has a negative control, which proves the test still reaches its
# hazard; were the engine ever laid out where the writes miss it, the test
# would pass whatever the fix did, and it says so instead:
#
#   exec'd without the reserve: the engine must fail as the phone did;
#   dlopen()ed with the engine's archive linked into the binary, as the
#       first RPM had it: the engine or the booster must come to harm.
#
# Ubuntu's glibc refuses to dlopen() a PIE, which Sailfish's does not
# (its glibc reverts that check: patch 0009 in sailfishos/glibc), so the
# booster is handed a copy with the PIE flag cleared.
#
# Needs target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.{a,so}
# (scripts/cross-build-rust.sh), aarch64-linux-gnu-gcc with its sysroot in
# /usr/aarch64-linux-gnu, qemu-aarch64 (Ubuntu: qemu-user) and python3.
# Run by ci.yml's `cross` job after the cross build.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
release="$root/target/aarch64-unknown-linux-gnu/release"
archive="$release/libsukkula_ffi.a"
so="$release/libsukkula_ffi.so"
cc=${CC_AARCH64:-aarch64-linux-gnu-gcc}
nm=${NM_AARCH64:-aarch64-linux-gnu-nm}
qemu=${QEMU_AARCH64:-qemu-aarch64}
sysroot=${AARCH64_SYSROOT:-/usr/aarch64-linux-gnu}

fail() { echo "tls-slots-test: FAIL $*" >&2; exit 1; }
for f in "$archive" "$so"; do
    [[ -f "$f" ]] || fail "no $f; run scripts/cross-build-rust.sh first"
done
for tool in "$cc" "$nm" "$qemu" python3; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool not found"
done
[[ -e "$sysroot/lib/ld-linux-aarch64.so.1" ]] || fail "no aarch64 sysroot at $sysroot"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/lib"
cp "$so" "$work/lib/"

# A libdbus-1 to run against: every dbus_* function the engine calls,
# returning 0 -- no bus, which the engine meets as a missing BlueZ. Under
# libdbus-1's soname and version node, so the links are the real ones'.
syms=$("$nm" -u "$archive" 2>/dev/null | awk '{print $NF}' | grep '^dbus_' | sort -u || true)
for s in $syms; do echo "long $s(void) { return 0; }"; done > "$work/dbus.c"
printf 'LIBDBUS_1_3 { global: *; };\n' > "$work/dbus.map"
"$cc" -shared -fPIC -Wl,-soname,libdbus-1.so.3 -Wl,--version-script="$work/dbus.map" \
    -o "$work/lib/libdbus-1.so.3" "$work/dbus.c"
ln -s libdbus-1.so.3 "$work/lib/libdbus-1.so"

# The app: what the GL stack does to the GUI thread, then the engine.
cat > "$work/app.c" <<'EOF'
#include <stdio.h>
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

__attribute__((visibility("default"))) int main(int argc, char **argv)
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

# The booster: thread-locals of its own behind a reserve (the app writes
# the slots in this process too), dlopen() of the app as mapplauncherd's
# Booster::loadMain() does it, then main() with the invoker's arguments.
cat > "$work/booster.c" <<'EOF'
#include <dlfcn.h>
#include <stdio.h>

#define N 256
static __thread unsigned long booster_state[N] = {1};

int main(int argc, char **argv)
{
    int i, rc, harmed = 0;

    if (argc != 3)
        return 2;
    for (i = 0; i < N; i++)
        booster_state[i] = 0xb005700000000000ul + (unsigned long)i;
    void *app = dlopen(argv[1], RTLD_LAZY | RTLD_GLOBAL);
    if (app == NULL) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 2;
    }
    int (*app_main)(int, char **) = (int (*)(int, char **))dlsym(app, "main");
    if (app_main == NULL) {
        fprintf(stderr, "dlsym: %s\n", dlerror());
        return 2;
    }
    char *app_argv[] = {argv[1], argv[2], NULL};
    rc = app_main(2, app_argv);
    for (i = 0; i < N; i++) {
        if (booster_state[i] != 0xb005700000000000ul + (unsigned long)i) {
            fprintf(stderr, "the booster's thread-local %d was overwritten\n", i);
            harmed = 1;
        }
    }
    return rc != 0 || harmed;
}
EOF

inc=(-I"$root/crates/sukkula-ffi/include" -I"$root/src")
# The app linked as harbour-sukkula.pro links the shell: PIE, main()
# exported, the reserve first, the engine's library found by its RPATH
# (LD_LIBRARY_PATH here, where nothing is installed).
app() {
    local out=$1
    shift
    "$cc" -O2 -fPIE -pie "${inc[@]}" -o "$out" "$@" -rdynamic \
        -Wl,--dynamic-list="$root/src/dynamic.list" -Wl,--exclude-libs,ALL \
        -L"$work/lib" -Wl,-rpath-link,"$work/lib" -Wl,-z,relro -Wl,-z,now -Wl,--as-needed
}
app "$work/packaged" "$root/src/tls_reserve.c" -DWITH_RESERVE "$work/app.c" "$work/lib/libsukkula_ffi.so"
app "$work/unreserved" "$work/app.c" "$work/lib/libsukkula_ffi.so"
# The first RPM's link: the archive in the binary.
app "$work/archive-linked" "$root/src/tls_reserve.c" -DWITH_RESERVE "$work/app.c" "$archive" \
    -ldbus-1 -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
"$cc" -O2 "${inc[@]}" -o "$work/booster" "$root/src/tls_reserve.c" "$work/booster.c" -ldl

# pie_cleared <in> <out>: a copy without DF_1_PIE, so this glibc dlopen()s
# it as Sailfish's does.
pie_cleared() {
    python3 - "$1" "$2" <<'EOF'
import struct, sys
data = bytearray(open(sys.argv[1], "rb").read())
phoff, = struct.unpack_from("<Q", data, 0x20)
phnum, = struct.unpack_from("<H", data, 0x38)
cleared = False
for i in range(phnum):
    p_type, _, p_offset, _, _, p_filesz, _, _ = struct.unpack_from("<IIQQQQQQ", data, phoff + i * 56)
    if p_type != 2:  # PT_DYNAMIC
        continue
    for off in range(p_offset, p_offset + p_filesz, 16):
        tag, val = struct.unpack_from("<qQ", data, off)
        if tag == 0x6FFFFFFB and val & 0x08000000:  # DT_FLAGS_1, DF_1_PIE
            struct.pack_into("<qQ", data, off, tag, val & ~0x08000000)
            cleared = True
if not cleared:
    sys.exit("no DF_1_PIE to clear in " + sys.argv[1])
open(sys.argv[2], "wb").write(bytes(data))
EOF
}
pie_cleared "$work/packaged" "$work/packaged.dlopen"
pie_cleared "$work/archive-linked" "$work/archive-linked.dlopen"

# run <log> <program> <args...>: under qemu, with the engine's two
# directories, siblings, as the shell's are; the config is the last
# argument.
run() {
    local log=$1 home config
    shift
    home=$(mktemp -d "$work/home.XXXXXX")
    mkdir -p "$home/share/sukkula" "$home/Downloads"
    config=$(printf '{"v":1,"data_dir":"%s/share/sukkula","download_dir":"%s/Downloads/Sukkula"}' \
        "$home" "$home")
    RUST_BACKTRACE=0 timeout 120 "$qemu" -L "$sysroot" -E LD_LIBRARY_PATH="$work/lib" \
        "$@" "$config" > "$log" 2>&1
}
must_pass() { # what log program args...
    local what=$1 log=$2
    shift 2
    if ! run "$log" "$@" || ! grep -q '"type":"started"' "$log"; then
        cat "$log" >&2
        fail "$what"
    fi
}
must_fail() { # what log program args...
    local what=$1 log=$2
    shift 2
    if run "$log" "$@"; then
        cat "$log" >&2
        fail "$what: the test no longer reaches the hazard it exists for"
    fi
    echo "tls-slots-test:   ok   $(grep -m1 -E 'panicked|did not start|overwritten|dlopen' "$log" || echo "exit status")"
}

echo "== exec'd, as the package links it"
must_pass "the engine did not survive written TLS slots" "$work/packaged.log" "$work/packaged"
echo "tls-slots-test:   ok   started, stopped, and the slots kept what was written"

echo "== exec'd without src/tls_reserve.c (negative control)"
must_fail "the engine survived written slots with no reserve" "$work/unreserved.log" "$work/unreserved"

echo "== dlopen()ed by a booster, as the package links it"
must_pass "the engine did not survive being dlopen()ed" "$work/booster.log" \
    "$work/booster" "$work/packaged.dlopen"
echo "tls-slots-test:   ok   started and stopped, and the booster's thread-locals are untouched"

echo "== dlopen()ed with the engine linked into the binary (negative control)"
must_fail "the archive-linked engine survived being dlopen()ed" "$work/archive.log" \
    "$work/booster" "$work/archive-linked.dlopen"

echo "tls-slots-test: ok"
