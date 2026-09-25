#!/usr/bin/env bash
# Cross-build the Rust engine for the phone: libsukkula_ffi.a, aarch64.
#
#     scripts/cross-build-rust.sh --sdk <target-sysroot>
#     scripts/cross-build-rust.sh --host
#
# --sdk is the route every package takes (spec §3, rpm.yml). The SDK ships
# Rust 1.75 and the engine needs the pinned 1.97.1, but the SDK's *Rust* is
# the only part that is too old: its aarch64 GCC 10 and its Sailfish OS
# 5.2 target sysroot are exactly what has to match the phone. So the host's
# pinned cargo drives them -- every C file in the graph (ring's, above all)
# is compiled by the SDK's own gcc against the target's glibc headers, and
# the result links against the target's glibc. This is vuo's
# scripts/cross-build.sh route, which has shipped Harbour packages;
# docs/BUILDING.md has the recipe.
#
#   SFOS_CROSS       the SDK's cross toolchain prefix (default /opt/cross,
#                    where GCC resolves its own cc1 and specs -- it must be
#                    reachable at that absolute path)
#   SFOS_CROSS_LIBS  a directory of the SDK tooling's own libraries that
#                    the 32-bit cc1 loads (libmpc, libmpfr, libgmp); put on
#                    LD_LIBRARY_PATH for the compiler alone
#
# --host is the per-pull-request smoke (ci.yml's `cross`): Ubuntu's
# aarch64-linux-gnu-gcc and its sysroot. It proves the graph cross-compiles
# and links for aarch64 -- no x86-only code, no host-only C dependency --
# but Ubuntu's glibc is newer than the phone's, which is why no package is
# ever built this way. Where Ubuntu has no arm64 libdbus-1 installed it
# links the probe against a stand-in with libdbus-1's soname and symbol
# version, made here from the symbols the engine actually needs.
#
# Either way the output is where the qmake project and the spec expect it,
# target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.a, and it is then
# proved: its four C entry points exported, every native library it needs
# on Harbour's allowed list, and a probe executable linked against it the
# way the shell links it -- whose glibc symbol versions, NEEDED libraries
# and hardening ci/check-elf.sh reads back.
#
#   SUKKULA_GLIBC_CEILING  the newest glibc a symbol may need. Default: the
#                    target sysroot's own glibc with --sdk; with --host,
#                    2.34 -- the version Harbour's __libc_start_main rule
#                    guarantees every accepted device has.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TRIPLE=aarch64-unknown-linux-gnu
LIB="target/$TRIPLE/release/libsukkula_ffi.a"
HEADER="crates/sukkula-ffi/include/sukkula.h"

usage() {
    echo "usage: $0 --sdk <target-sysroot> | --host" >&2
    exit 2
}
fail() { echo "cross-build-rust: FAIL $*" >&2; exit 1; }

mode=""
SR=""
case "${1:-}" in
    --sdk) [[ $# -eq 2 ]] || usage; mode=sdk; SR=$(cd "$2" && pwd) ;;
    --host) [[ $# -eq 1 ]] || usage; mode=host ;;
    *) usage ;;
esac

# -- the toolchain ----------------------------------------------------------
#
# The pinned one, from rust-toolchain.toml, and never another: a cargo that
# is not rustup's (the SDK's, a distribution's) would ignore the pin and
# build with whatever it is.
pinned=$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' rust-toolchain.toml)
[[ -n "$pinned" ]] || fail "rust-toolchain.toml pins no channel"
actual=$(rustc --version)
case "$actual" in
    "rustc $pinned "*) echo "== $actual (pinned $pinned)" ;;
    *) fail "rustc is '$actual', not the pinned $pinned; run this under rustup" ;;
esac
rustup target list --installed 2>/dev/null | grep -qx "$TRIPLE" ||
    rustup target add "$TRIPLE"

BINDIR="$(mktemp -d)"
trap 'rm -rf "$BINDIR"' EXIT

# The hardening the distribution's own optflags apply to C, restated: this
# route does not go through rpm's build environment, and without them the C
# inside the engine would be the one part of the package built softer than
# an SDK build's C++. All are GCC 10 features. -fstack-clash-protection and
# -mbranch-protection=standard (pointer authentication and BTI, NOPs on
# cores without them) go beyond vuo's four.
HARDEN="-O2 -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fstack-clash-protection -mbranch-protection=standard -fPIC"

if [[ "$mode" = sdk ]]; then
    SFOS_CROSS="${SFOS_CROSS:-/opt/cross}"
    CROSS="$SFOS_CROSS/bin/aarch64-meego-linux-gnu"
    [[ -x "$CROSS-gcc" ]] || fail "no SDK cross gcc at $CROSS-gcc (docs/BUILDING.md: lift /opt/cross out of the SDK image)"
    [[ -d "$SR/usr/include" ]] || fail "no target sysroot at $SR"

    # GCC invokes plain `as` and `ld`; without -B it finds the host's x86
    # binutils on PATH and dies with "as: unrecognized option '-EL'".
    for t in as ld ar nm ranlib objcopy objdump strip readelf; do
        ln -sf "$CROSS-$t" "$BINDIR/$t"
    done
    # cc1 is a 32-bit program linking libraries that exist only inside the
    # SDK; they reach the compiler through a wrapper rather than through
    # LD_LIBRARY_PATH on everything cargo runs.
    for t in gcc g++; do
        {
            echo '#!/bin/sh'
            if [[ -n "${SFOS_CROSS_LIBS:-}" ]]; then
                echo "LD_LIBRARY_PATH='$SFOS_CROSS_LIBS'\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
                echo 'export LD_LIBRARY_PATH'
            fi
            echo "exec '$CROSS-$t' \"\$@\""
        } > "$BINDIR/sfos-$t"
        chmod +x "$BINDIR/sfos-$t"
    done
    CC="$BINDIR/sfos-gcc"
    CXX="$BINDIR/sfos-g++"
    AR="$CROSS-ar"
    READELF="$CROSS-readelf"
    NM="$CROSS-nm"
    SYSFLAGS="--sysroot=$SR -B$BINDIR/"
    LINKDIRS="-L$SR/usr/lib64 -L$SR/lib64 -Wl,-rpath-link,$SR/usr/lib64 -Wl,-rpath-link,$SR/lib64"

    # libdbus-sys finds the system libdbus-1 through pkg-config, and must
    # find the target's, never the build machine's.
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_SYSROOT_DIR="$SR"
    export PKG_CONFIG_LIBDIR="$SR/usr/lib64/pkgconfig:$SR/usr/share/pkgconfig"
    [[ -f "$SR/usr/lib64/pkgconfig/dbus-1.pc" ]] ||
        fail "the sysroot has no dbus-1.pc; the SDK image must carry the spec's BuildRequires (ci/build-sdk-image.sh)"

    # The device's glibc, read off the target's own headers.
    major=$(sed -n 's/^#define[[:space:]]\{1,\}__GLIBC__[[:space:]]\{1,\}\([0-9]\{1,\}\).*/\1/p' "$SR/usr/include/features.h")
    minor=$(sed -n 's/^#define[[:space:]]\{1,\}__GLIBC_MINOR__[[:space:]]\{1,\}\([0-9]\{1,\}\).*/\1/p' "$SR/usr/include/features.h")
    [[ -n "$major" && -n "$minor" ]] || fail "cannot read the glibc version from $SR/usr/include/features.h"
    CEILING="${SUKKULA_GLIBC_CEILING:-$major.$minor}"
    echo "== SDK route: $("$CC" --version | head -1), sysroot $SR (glibc $major.$minor)"
else
    command -v aarch64-linux-gnu-gcc >/dev/null 2>&1 ||
        fail "no aarch64-linux-gnu-gcc (install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu)"
    CC=aarch64-linux-gnu-gcc
    CXX=aarch64-linux-gnu-g++
    AR=aarch64-linux-gnu-ar
    READELF=readelf
    NM=aarch64-linux-gnu-nm
    command -v "$NM" >/dev/null 2>&1 || NM="nm"
    SYSFLAGS=""
    LINKDIRS=""
    CEILING="${SUKKULA_GLIBC_CEILING:-2.34}"

    export PKG_CONFIG_ALLOW_CROSS=1
    if [[ -f /usr/lib/aarch64-linux-gnu/pkgconfig/dbus-1.pc ]]; then
        export PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
        stub_dbus=0
    else
        # A pkg-config entry for the stand-in libdbus-1 built after the
        # engine, below. It carries no flags a compile could use; libdbus-sys
        # only needs the probe to succeed and names -ldbus-1 itself.
        mkdir -p "$BINDIR/pkgconfig" "$BINDIR/lib"
        cat > "$BINDIR/pkgconfig/dbus-1.pc" <<EOF
Name: dbus-1
Description: stand-in for the smoke build; see scripts/cross-build-rust.sh
Version: 1.14.10
Libs: -L$BINDIR/lib -ldbus-1
Cflags:
EOF
        export PKG_CONFIG_LIBDIR="$BINDIR/pkgconfig"
        stub_dbus=1
    fi
    echo "== smoke route: $("$CC" --version | head -1), glibc ceiling $CEILING"
fi

export "CC_${TRIPLE//-/_}=$CC"
export "CXX_${TRIPLE//-/_}=$CXX"
export "AR_${TRIPLE//-/_}=$AR"
export "CFLAGS_${TRIPLE//-/_}=$SYSFLAGS $HARDEN"
export "CXXFLAGS_${TRIPLE//-/_}=$SYSFLAGS $HARDEN"
# A staticlib is not linked by rustc, but anything aarch64 that is (a test
# binary, a future cdylib) links with the same compiler and sysroot.
TRIPLE_UPPER=$(tr 'a-z-' 'A-Z_' <<< "$TRIPLE")
export "CARGO_TARGET_${TRIPLE_UPPER}_LINKER=$CC"
# shellcheck disable=SC2086 # the flags are word lists on purpose
rustflags=""
for f in $SYSFLAGS $LINKDIRS -Wl,-z,relro -Wl,-z,now; do
    rustflags="$rustflags -C link-arg=$f"
done
export "CARGO_TARGET_${TRIPLE_UPPER}_RUSTFLAGS=${rustflags# }"

# -- the build --------------------------------------------------------------
#
# Only the final crate is cleaned, so rustc runs for it and prints the
# native libraries the archive needs; the rest of the graph stays cached.
# Uncoloured whatever CARGO_TERM_COLOR says (CI sets it to always): the
# escapes would land in the library list read out of the log below.
cargo clean --release --target "$TRIPLE" -p sukkula-ffi >/dev/null 2>&1 || true
log="$BINDIR/build.log"
echo "== cargo rustc --release --locked --target $TRIPLE -p sukkula-ffi --lib --crate-type staticlib"
if ! cargo rustc --release --locked --color never --target "$TRIPLE" -p sukkula-ffi --lib \
        --crate-type staticlib -- --print native-static-libs 2>&1 | tee "$log"; then
    fail "the engine did not build"
fi
[[ -f "$LIB" ]] || fail "cargo produced no $LIB"

native=$(sed -n 's/.*native-static-libs: //p' "$log" | tail -1)
[[ -n "$native" ]] || fail "rustc did not report the archive's native libraries"
echo
echo "== $LIB ($(du -h "$LIB" | cut -f1))"
echo "-- native libraries the archive needs: $native"

status=0
# Each is a library the shell's link will be handed. Not all of them end up
# NEEDED: Rust's libc crate names -lutil for openpty and friends, which
# nothing here calls -- and libutil.so.1 is not on Harbour's allowed list.
# Against glibc 2.34 and later -lutil resolves to an empty compatibility
# archive, and --as-needed drops whatever else is unused, so it must never
# reach NEEDED; the probe (below) and ci/check-elf.sh on the packaged
# binary are what prove it. Here each is only classified, and anything
# that is not a plain -l is refused.
for flag in $native; do
    case "$flag" in
        -l*) lib=${flag#-l} ;;
        *) echo "   unexpected link flag '$flag'" >&2; status=1; continue ;;
    esac
    if grep -qE "^lib${lib//+/\\+}\.so" ci/harbour/allowed_libraries.conf; then
        echo "   ok       -l$lib"
    else
        echo "   note     -l$lib is not on Harbour's allowed list; it must drop out of the link (--as-needed)"
    fi
done

# The C ABI of include/sukkula.h, exported from the archive.
echo "-- exported entry points --"
exports=$("$NM" -g --defined-only "$LIB" 2>/dev/null | awk '$2 == "T" {print $3}' | sort -u)
for sym in sukkula_start sukkula_command sukkula_stop sukkula_version; do
    if grep -qx "$sym" <<< "$exports"; then
        echo "   ok       $sym"
    else
        echo "   MISSING  $sym (include/sukkula.h promises it)" >&2
        status=1
    fi
done
[[ "$status" -eq 0 ]] || fail "the archive does not satisfy the shell's link; see above"

# -- the probe ----------------------------------------------------------------
#
# Linked the way harbour-sukkula.pro links the shell: PIE, RELRO, BIND_NOW,
# as-needed, the archive and the libraries rustc named, and the export
# flags -- the -rdynamic the SDK's sailfishapp feature adds, and
# src/hardening.pri's dynamic list and --exclude-libs,ALL that hold the
# exports to main() anyway. A symbol the device glibc lacks fails here, or
# shows up as a version above the ceiling; a symbol of the engine's that
# reaches .dynsym fails --only-main, on every pull request, against the
# real archive rather than a stand-in.
if [[ "$mode" = host && "$stub_dbus" = 1 ]]; then
    # The stand-in: every dbus_* symbol the archive needs, under libdbus-1's
    # soname and its LIBDBUS_1_3 version node, so the probe's version needs
    # read as the real link's would.
    syms=$("$NM" -u "$LIB" 2>/dev/null | awk '{print $NF}' | grep '^dbus_' | sort -u || true)
    {
        for s in $syms; do echo "void $s(void) {}"; done
        echo "void dbus_stand_in_marker(void) {}"
    } > "$BINDIR/dbus-stub.c"
    printf 'LIBDBUS_1_3 { global: *; };\n' > "$BINDIR/dbus-stub.map"
    "$CC" -shared -fPIC -Wl,-soname,libdbus-1.so.3 -Wl,--version-script="$BINDIR/dbus-stub.map" \
        -o "$BINDIR/lib/libdbus-1.so.3" "$BINDIR/dbus-stub.c"
    ln -sf libdbus-1.so.3 "$BINDIR/lib/libdbus-1.so"
    LINKDIRS="-L$BINDIR/lib"
    echo "-- libdbus-1: a stand-in with $(wc -w <<< "$syms") symbol(s); Ubuntu has no arm64 libdbus-1 here"
fi

cat > "$BINDIR/probe.c" <<'EOF'
/* Every entry point of include/sukkula.h, so the whole engine is linked. */
#include <stddef.h>
#include "sukkula.h"
static void on_event(const char *event_json, void *userdata) { (void)event_json; (void)userdata; }
int main(void) {
    SukkulaEngine *e = sukkula_start("{}", on_event, NULL);
    (void)sukkula_command(e, "{}");
    sukkula_stop(e);
    return sukkula_version() == NULL;
}
EOF
probe="target/$TRIPLE/release/sukkula-link-probe"
# src/tls_reserve.c first, as harbour-sukkula.pro links it: the engine's
# thread-locals go in after the 48 bytes of bionic's TLS slots, which
# check-elf.sh reads back.
# shellcheck disable=SC2086 # word lists on purpose
"$CC" $SYSFLAGS $HARDEN -fPIE -pie -I"$(dirname "$HEADER")" -I"$ROOT/src" -o "$probe" \
    "$ROOT/src/tls_reserve.c" "$BINDIR/probe.c" \
    $LINKDIRS -Wl,-z,relro -Wl,-z,now -Wl,-z,noexecstack -Wl,--as-needed \
    -rdynamic -Wl,--dynamic-list="$ROOT/src/dynamic.list" -Wl,--exclude-libs,ALL \
    "$LIB" $native || fail "the probe does not link against the engine"

echo
echo "== the probe, linked as the shell will be =="
"$ROOT/ci/check-elf.sh" --readelf "$READELF" --glibc-ceiling "$CEILING" \
    --libc-start-main 2.34 --main-export --only-main "$probe" ||
    fail "the engine needs more than the phone provides, or leaks exports; see above"

echo
echo "cross-build-rust: ok -- $LIB"
