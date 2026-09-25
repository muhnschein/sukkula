#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The C++ shell's host tests, offline, on a desktop Qt 5.15:
#
#   1. tests/cpp/bridge_test under ASan and UBSan, against the stub engine
#      (tests/cpp/stub) and, with SUKKULA_ENGINE=rust, the real one too;
#   2. src/main.cpp and src/bridge.cpp built as the phone builds them (same
#      hardening flags) against a libsailfishapp stand-in, started
#      offscreen on the real qml/ with qml-stubs/: no Qt warning, and the
#      Finnish catalogue loads through main.cpp's own translator;
#   3. that binary's ELF checks: main() is the only export, PIE, full
#      RELRO, stripped, the one RPATH as DT_RPATH, and only libraries on
#      Harbour's list recorded;
#   4. harbour-sukkula.pro itself, built with a stand-in of the SDK's
#      sailfishapp feature against an engine library linked as
#      scripts/cross-build-rust.sh links the phone's, and installed with
#      INSTALL_ROOT, must install exactly the Harbour layout.
#
# Needs: qtbase5-dev, qtdeclarative5-dev, qml-module-qtquick2,
# qttools5-dev-tools (lrelease), g++ with libasan/libubsan, make,
# binutils (readelf); for SUKKULA_ENGINE=rust also libdbus-1-dev and
# `cargo build -p sukkula-ffi` beforehand.
#
# Env: BUILD_DIR (default build/host-tests), QMAKE (default qmake),
#      SUKKULA_ENGINE=stub|rust (default stub; rust runs the stub too),
#      SUKKULA_RUST_LIB (default target/debug/libsukkula_ffi.a).
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
build=${BUILD_DIR:-$root/build/host-tests}
qmake=${QMAKE:-qmake}
lrelease=${LRELEASE:-lrelease}
engine=${SUKKULA_ENGINE:-stub}
rust_lib=${SUKKULA_RUST_LIB:-$root/target/debug/libsukkula_ffi.a}
status=0

say() { printf '%s\n' "$*"; }
fail() { say "FAIL: $*"; status=1; }

for tool in "$qmake" make readelf "$lrelease"; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        say "run-cpp-tests: FAIL ($tool not found)" >&2
        exit 1
    fi
done
if [ "$engine" = rust ] && [ ! -f "$rust_lib" ]; then
    say "run-cpp-tests: FAIL (no $rust_lib; run cargo build -p sukkula-ffi)" >&2
    exit 1
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -m 700 "$scratch/runtime"
export XDG_RUNTIME_DIR="$scratch/runtime"
export QT_QPA_PLATFORM=offscreen
export QT_QUICK_BACKEND=software
unset QML_IMPORT_PATH

# qmake + make in a build directory of its own.
build_pro() { # dir pro [qmake args...]
    dir=$1
    pro=$2
    shift 2
    mkdir -p "$dir"
    (cd "$dir" && "$qmake" "$pro" "$@" >/dev/null && make -s >/dev/null)
}

engines=stub
[ "$engine" = rust ] && engines="stub rust"

# What Harbour reads from a binary (1.6.1, 1.7): main() the only dynamic
# export, PIE, full RELRO, stripped, and only allowed libraries recorded;
# and the RPATH sailfishapp.prf sets, as DT_RPATH (src/hardening.pri).
elf_checks() { # binary label rpath [reserve]
    before=$status
    # The C runtime's and the linker's own symbols, which -rdynamic (the
    # SDK's sailfishapp feature passes it) exports from every binary, are
    # not ours; anything else besides main() -- a Bridge method, a
    # sukkula_* function, a Rust symbol -- is a leak.
    syms=$(readelf --dyn-syms -W "$1" | awk '$5 == "GLOBAL" && $7 != "UND" { print $8 }' | sed 's/@.*//' |
        grep -v -x -E '_IO_stdin_used|__bss_start|__data_start|data_start|_edata|_end|_start|_init|_fini|__dso_handle' |
        sort -u)
    [ "$syms" = main ] || fail "$2 exports more than main(): $(printf '%s' "$syms" | tr '\n' ' ')"
    readelf -d "$1" | grep -q 'BIND_NOW' || fail "$2: no BIND_NOW (-z now)"
    readelf -lW "$1" | grep -q 'GNU_RELRO' || fail "$2: no GNU_RELRO segment"
    readelf -hW "$1" | grep -q 'DYN' || fail "$2: not a position-independent executable"
    if readelf -d "$1" | grep -q '(RUNPATH)'; then
        fail "$2: carries a RUNPATH, which Jolla's validator does not read (-Wl,--disable-new-dtags)"
    fi
    got_rpath=$(readelf -d "$1" | sed -n 's/.*(RPATH).*\[\(.*\)\]/\1/p')
    [ "$got_rpath" = "$3" ] || fail "$2: its RPATH is '$got_rpath', not $3"
    if readelf -SW "$1" | grep -q ' \.symtab '; then
        fail "$2: not stripped"
    fi
    # With the engine a library of its own, the executable's only
    # thread-local is src/tls_reserve.c's array: 4096 zero bytes
    # (ci/check-elf.sh holds the phone's binary to the rest of the rule).
    # The host app links the engine's archive in, so only the project's own
    # build is asked (a fourth argument).
    if [ "${4:-}" = reserve ]; then
        tls=$(readelf -lW "$1" | awk '$1 == "TLS" { print $5, $6 }')
        [ "$tls" = "0x000000 0x001000" ] ||
            fail "$2: its thread-locals are not src/tls_reserve.c's array alone (FileSiz MemSiz: '$tls')"
    fi
    needed=$(readelf -d "$1" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | sort)
    for lib in $needed; do
        case $lib in
            libQt5Core.so.5|libQt5Gui.so.5|libQt5Qml.so.5|libQt5Quick.so.5|libQt5DBus.so.5) ;;
            libsailfishapp.so.1|libmdeclarativecache5.so.0|libdbus-1.so.3|libz.so.1) ;;
            # The engine's own library, found through the RPATH.
            libsukkula_ffi.so) ;;
            libc.so.6|libm.so.6|libdl.so.2|libpthread.so.0|librt.so.1|libgcc_s.so.1|libstdc++.so.6) ;;
            # The dynamic loader, for __tls_get_addr (the Rust library's
            # thread-locals): ld-linux-aarch64.so.1 is on Harbour's list,
            # and this is the host's twin of it.
            ld-linux-aarch64.so.1|ld-linux-x86-64.so.2) ;;
            *) fail "$2 links $lib, which is not on Harbour's list" ;;
        esac
    done
    if [ "$status" -eq "$before" ]; then
        say "binary checks ($2): ok (needs: $(printf '%s' "$needed" | tr '\n' ' '))"
    fi
}

# 1. The bridge.
for e in $engines; do
    dir=$build/bridge-$e
    if build_pro "$dir" "$root/tests/cpp/bridge_test/bridge_test.pro" CONFIG+=sukkula_sanitize \
        SUKKULA_ENGINE="$e" SUKKULA_RUST_LIB="$rust_lib"; then
        if (cd "$scratch" && HOME="$scratch/home-bridge-$e" "$dir/tst_bridge" -silent); then
            say "bridge tests ($e engine): ok"
        else
            fail "bridge tests ($e engine)"
        fi
    else
        fail "bridge tests ($e engine) did not build"
    fi
done

# 2. and 3. The shell as the phone builds it.
mkdir -p "$build/qm"
for ts in "$root"/translations/*.ts; do
    "$lrelease" -silent -nounfinished "$ts" -qm "$build/qm/$(basename "$ts" .ts).qm"
done
for e in $engines; do
    dir=$build/host-app-$e
    if ! build_pro "$dir" "$root/tests/cpp/host_app/host_app.pro" SUKKULA_ENGINE="$e" SUKKULA_RUST_LIB="$rust_lib"; then
        fail "host app ($e engine) did not build"
        continue
    fi
    app=$dir/harbour-sukkula-host
    home=$scratch/home-app-$e
    mkdir -p "$home"
    out=$(HOME="$home" QML2_IMPORT_PATH="$root/qml-stubs" SUKKULA_HOST_QUIT_AFTER_MS=2000 \
        SUKKULA_HOST_TRANSLATIONS="$build/qm" SUKKULA_HOST_DUMP_TEXTS=1 LANG=fi_FI.UTF-8 LC_ALL=fi_FI.UTF-8 \
        "$app" 2>&1) || { fail "host app ($e engine) exited non-zero"; say "$out"; continue; }
    case $out in
        *"HOST-APP: warnings=0 "*) ;;
        *) fail "host app ($e engine) warned"; say "$out" ;;
    esac
    case $out in
        *"HOST-TEXT: Vastaanota"*) ;;
        *) fail "host app ($e engine): the Finnish catalogue did not load through main.cpp" ;;
    esac
    case $out in
        *"HOST-TEXT: Muille näkyy nimellä "*) ;;
        *) fail "host app ($e engine): the engine's settings never reached the main page" ;;
    esac
    if [ "$e" = rust ]; then
        [ -d "$home/.local/share/sukkula/sukkula" ] || fail "the real engine made no data dir under sukkula/sukkula"
        [ -d "$home/Downloads/Sukkula" ] || fail "the real engine made no ~/Downloads/Sukkula"
    fi
    [ "$status" -eq 0 ] && say "host app ($e engine): ok"


    # 3. What Harbour reads from the binary.
    elf_checks "$app" "host app, $e engine" /usr/share/harbour-sukkula-host/lib
done

# 4. The project file's install layout. Built fresh each time: which
# library it links changes between runs, and make would not relink.
dir=$build/pro
rm -rf "$dir"
mkdir -p "$dir"
mkdir -p "$dir/engine"
pro_lib=$dir/engine/libsukkula_ffi.so
abi="sukkula_command sukkula_start sukkula_stop sukkula_version"
if [ "$engine" = rust ]; then
    # The real archive, linked into the library the way
    # scripts/cross-build-rust.sh links the phone's: the four entry points
    # pulled in, the export map, every symbol resolved.
    cc -shared -o "$pro_lib" -Wl,-soname,libsukkula_ffi.so \
        -Wl,-u,sukkula_start -Wl,-u,sukkula_command -Wl,-u,sukkula_stop -Wl,-u,sukkula_version \
        -Wl,--version-script="$root/crates/sukkula-ffi/exports.map" \
        -Wl,-z,relro -Wl,-z,now -Wl,-z,defs -Wl,--as-needed \
        "$rust_lib" -ldbus-1 -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
    got_abi=$(readelf --dyn-syms -W "$pro_lib" | awk '$5 == "GLOBAL" && $7 != "UND" { print $8 }' |
        sed 's/@.*//' | sort -u | tr '\n' ' ')
    [ "$got_abi" = "$abi " ] ||
        fail "the engine's library exports '$got_abi', not the C ABI alone (crates/sukkula-ffi/exports.map)"
else
    cc -c -O2 -std=c11 -D_POSIX_C_SOURCE=200809L -fPIC -I"$root/crates/sukkula-ffi/include" \
        "$root/tests/cpp/stub/sukkula_stub.c" -o "$dir/sukkula_stub.o"
    cc -shared -o "$pro_lib" -Wl,-soname,libsukkula_ffi.so "$dir/sukkula_stub.o" -lpthread
fi
if (cd "$dir" && QMAKEFEATURES="$root/tests/cpp/sailfishapp/features" \
        "$qmake" "$root/harbour-sukkula.pro" SUKKULA_RUST_LIB="$pro_lib" >/dev/null \
        && make -s >/dev/null && rm -rf "$dir/root" && make -s install INSTALL_ROOT="$dir/root" >/dev/null); then
    got=$(cd "$dir/root" && find . -type f | sed 's|^\./|/|' | sort)
    want=$( (
        printf '%s\n' /usr/bin/harbour-sukkula /usr/share/applications/harbour-sukkula.desktop
        printf '%s\n' /usr/share/harbour-sukkula/lib/libsukkula_ffi.so
        for size in 86x86 108x108 128x128 172x172; do
            printf '/usr/share/icons/hicolor/%s/apps/harbour-sukkula.png\n' "$size"
        done
        for lang in "" -de -fi -sv; do
            printf '/usr/share/harbour-sukkula/translations/harbour-sukkula%s.qm\n' "$lang"
        done
        (cd "$root" && find qml -type f | sed 's|^|/usr/share/harbour-sukkula/|')
    ) | sort)
    if [ "$got" = "$want" ]; then
        say "install layout: ok ($(printf '%s\n' "$got" | wc -l) files)"
    else
        fail "install layout differs from Harbour's (- expected, + installed):"
        printf '%s\n' "$want" > "$scratch/want"
        printf '%s\n' "$got" > "$scratch/got"
        diff "$scratch/want" "$scratch/got" || true
    fi
    for f in $(cd "$dir/root" && find . -type f -perm /022); do
        fail "group- or world-writable: $f"
    done
    # Linked as the SDK links it, -rdynamic included: still main() alone.
    elf_checks "$dir/root/usr/bin/harbour-sukkula" "harbour-sukkula.pro build" \
        /usr/share/harbour-sukkula/lib reserve
else
    fail "harbour-sukkula.pro did not build or install on the host"
fi

if [ "$status" -eq 0 ]; then
    say "cpp tests: ok"
else
    say "cpp tests: FAIL"
fi
exit "$status"
