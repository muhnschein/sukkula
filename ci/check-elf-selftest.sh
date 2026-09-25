#!/bin/bash
# Prove that ci/check-elf.sh still refuses the binaries it claims to
# refuse, on real aarch64 ELF files linked here with Ubuntu's cross GCC:
# one per rule, each built wrong in exactly one way, plus the one built
# right. Run by ci.yml's `cross` job, which has the compiler.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
check="$root/ci/check-elf.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cc=${CC_AARCH64:-aarch64-linux-gnu-gcc}
strip=${STRIP_AARCH64:-aarch64-linux-gnu-strip}
ar=${AR_AARCH64:-aarch64-linux-gnu-ar}
for tool in "$cc" "$strip" "$ar"; do
    command -v "$tool" >/dev/null 2>&1 || { echo "selftest: FAIL $tool not found" >&2; exit 1; }
done

cat > "$work/main.c" <<'EOF'
#include <stdio.h>
__attribute__((visibility("default"))) int main(void) { puts("x"); return 0; }
EOF
# A library Harbour does not allow, by soname. Since glibc 2.34 a real
# -lutil resolves to an empty static archive and never reaches NEEDED, so
# the stand-in carries libutil's soname itself.
cat > "$work/util.c" <<'EOF'
int util_stand_in(void) { return 0; }
EOF
cat > "$work/uses-util.c" <<'EOF'
int util_stand_in(void);
__attribute__((visibility("default"))) int main(void) { return util_stand_in(); }
EOF
# The leaks --only-main exists for: one of the C ABI's entry points, and a
# Rust symbol (a mangled name, spelt with an asm label), exported beside
# main() the way -rdynamic exports them when the archive's symbols are not
# kept out of .dynsym.
cat > "$work/exports-ffi.c" <<'EOF'
__attribute__((visibility("default"))) int sukkula_version(void) { return 1; }
__attribute__((visibility("default"))) int main(void) { return sukkula_version() - 1; }
EOF
cat > "$work/exports-rust.c" <<'EOF'
__attribute__((visibility("default"))) int rust_fn(void)
    __asm__("_ZN12sukkula_core4name8sanitize17h0123456789abcdefE");
int rust_fn(void) { return 0; }
__attribute__((visibility("default"))) int main(void) { return rust_fn(); }
EOF
# The same, but the way the package links: the "Rust" code in a static
# archive, kept out of .dynsym by --exclude-libs,ALL and a dynamic list of
# main alone (src/hardening.pri, src/dynamic.list).
cat > "$work/archived.c" <<'EOF'
__attribute__((visibility("default"))) int sukkula_version(void) { return 1; }
EOF
cat > "$work/uses-archive.c" <<'EOF'
int sukkula_version(void);
__attribute__((visibility("default"))) int main(void) { return sukkula_version() - 1; }
EOF
printf '{\n    main;\n};\n' > "$work/dynamic.list"
# Thread-locals of the executable's own (src/tls_reserve.c): the shell,
# whose only one is the reserve; the reserve beside a zeroed thread-local
# and beside an initialised one, as an engine linked into the binary has;
# thread-locals and no reserve; and the reserve beside a thread-local
# aligned to 64, which moves the whole block off tp+16.
cp "$root/src/tls_reserve.c" "$root/src/tls_reserve.h" "$work/"
cat > "$work/shell-tls.c" <<'EOF'
#include "tls_reserve.h"
__attribute__((visibility("default"))) int main(void) {
    return !sukkula_tls_reserved() || sukkula_tls_reserve_used() != 0;
}
EOF
cat > "$work/zeroed-tls.c" <<'EOF'
__thread long engine_scratch[4];
long read_scratch(void) { return engine_scratch[0]; }
EOF
cat > "$work/initialised-tls.c" <<'EOF'
__thread long engine_state = 1;
long read_state(void) { return engine_state; }
EOF
cat > "$work/engine-tls-only.c" <<'EOF'
__thread long engine_state = 1;
__attribute__((visibility("default"))) int main(void) { return (int)engine_state - 1; }
EOF
cat > "$work/aligned-tls.c" <<'EOF'
__thread long wide __attribute__((aligned(64))) = 1;
long read_wide(void) { return wide; }
EOF
# The engine's library, in miniature: a thread-local, one exported entry
# point and one that the export map keeps in; and a shell linked against
# it the way harbour-sukkula.pro links the real one.
cat > "$work/engine.c" <<'EOF'
#include <unistd.h>
static __thread long engine_state = 1;
long sukkula_version(void) { return getpid() < 0 ? -1 : engine_state++; }
long engine_internal(void) { return engine_state; }
EOF
printf '{\n    global: sukkula_version;\n    local: *;\n};\n' > "$work/engine.map"
cat > "$work/uses-engine.c" <<'EOF'
#include "tls_reserve.h"
long sukkula_version(void);
__attribute__((visibility("default"))) int main(void) {
    return !sukkula_tls_reserved() || sukkula_version() != 1 || sukkula_tls_reserve_used() != 0;
}
EOF

status=0
cases=0
# shellcheck disable=SC2054 # -Wl, flags carry their commas by design
good=(-fvisibility=hidden -fPIE -pie -rdynamic -Wl,-z,relro -Wl,-z,now -Wl,-z,noexecstack)
# build <name> <source> <flags...>: link, then strip, like the package.
build() {
    local name=$1 src=$2
    shift 2
    "$cc" -O2 -o "$work/$name" "$work/$src" "$@" 2>/dev/null &&
        "$strip" --strip-all "$work/$name"
}
full="--main-export --only-main --stripped --libc-start-main 2.34 --glibc-ceiling 2.39"
# The same for the engine's library (--library), set as $full for its cases.
lib_full="--library --stripped --glibc-ceiling 2.39"
# expect <pass|fail> <what> <binary> [extra check-elf options]
expect() {
    local want=$1 what=$2 bin=$3 got
    shift 3
    cases=$((cases + 1))
    # shellcheck disable=SC2086 # $full is a list of options
    if "$check" $full "$@" "$work/$bin" >/dev/null 2>&1; then got=pass; else got=fail; fi
    if [[ "$got" = "$want" ]]; then
        echo "selftest: ok   $what -> $want"
    else
        echo "selftest: FAIL $what should $want, got $got" >&2
        # shellcheck disable=SC2086
        "$check" $full "$@" "$work/$bin" >&2
        status=1
    fi
    return 0
}

build good main.c "${good[@]}"
# Linked like the rest, and left unstripped.
"$cc" -O2 -o "$work/unstripped" "$work/main.c" "${good[@]}"
build no-export main.c -fvisibility=hidden -fPIE -pie -Wl,-z,relro -Wl,-z,now
build no-relro main.c -fvisibility=hidden -fPIE -pie -rdynamic -Wl,-z,norelro -Wl,-z,now
build lazy main.c -fvisibility=hidden -fPIE -pie -rdynamic -Wl,-z,relro -Wl,-z,lazy
build no-pie main.c -fvisibility=hidden -no-pie -rdynamic -Wl,-z,relro -Wl,-z,now
build execstack main.c "${good[@]}" -Wl,-z,execstack
build rpath main.c "${good[@]}" -Wl,-rpath,/opt/lib
"$cc" -shared -fPIC -Wl,-soname,libutil.so.1 -o "$work/libutil-stand-in.so" "$work/util.c"
build libutil uses-util.c "${good[@]}" "$work/libutil-stand-in.so"
build libutil-asneeded main.c "${good[@]}" -Wl,--as-needed -lutil
build exports-ffi exports-ffi.c "${good[@]}"
build exports-rust exports-rust.c "${good[@]}"
"$cc" -O2 -c -fPIC -o "$work/archived.o" "$work/archived.c"
"$ar" rcs "$work/libarchived.a" "$work/archived.o"
build archive-leaks uses-archive.c "${good[@]}" "$work/libarchived.a"
build archive-kept-in uses-archive.c "${good[@]}" -Wl,--dynamic-list="$work/dynamic.list" \
    -Wl,--exclude-libs,ALL "$work/libarchived.a"
for obj in tls_reserve shell-tls zeroed-tls initialised-tls aligned-tls; do
    "$cc" -O2 -c -fPIC -fvisibility=hidden -I"$work" -o "$work/$obj.o" "$work/$obj.c"
done
# link <name> <objects...>: the objects in this order, then strip.
link() {
    local name=$1
    shift
    "$cc" -O2 -o "$work/$name" "$@" "${good[@]}" 2>/dev/null &&
        "$strip" --strip-all "$work/$name"
}
link tls-reserved "$work/tls_reserve.o" "$work/shell-tls.o"
link tls-zeroed "$work/tls_reserve.o" "$work/shell-tls.o" "$work/zeroed-tls.o"
link tls-initialised "$work/tls_reserve.o" "$work/shell-tls.o" "$work/initialised-tls.o"
build tls-unreserved engine-tls-only.c "${good[@]}"
link tls-aligned "$work/tls_reserve.o" "$work/shell-tls.o" "$work/aligned-tls.o"
# lib <name> <soname> <flags...>: the engine's library, linked and stripped
# as scripts/cross-build-rust.sh links it, but for the flags given.
lib() {
    local name=$1 soname=$2
    shift 2
    "$cc" -O2 -fPIC -shared -o "$work/$name" "$work/engine.c" -Wl,-soname,"$soname" \
        -Wl,-z,relro -Wl,-z,now -Wl,-z,noexecstack "$@" 2>/dev/null &&
        "$strip" --strip-all "$work/$name"
}
mkdir -p "$work/lib"
lib lib/libsukkula_ffi.so libsukkula_ffi.so -Wl,--version-script="$work/engine.map"
lib lib-soname.so libsukkula_ffi.so -Wl,--version-script="$work/engine.map"
lib lib-exports.so lib-exports.so
lib lib-static-tls.so lib-static-tls.so -ftls-model=initial-exec -Wl,--version-script="$work/engine.map"
lib lib-execstack.so lib-execstack.so -Wl,--version-script="$work/engine.map" -Wl,-z,execstack
# The shell against it: the RPATH as DT_RPATH, as a RUNPATH, and missing.
shell() {
    local name=$1
    shift
    link "$name" "$work/tls_reserve.o" "$work/uses-engine.c" -I"$work" -L"$work/lib" -lsukkula_ffi "$@"
}
shell shell-rpath -Wl,--disable-new-dtags -Wl,-rpath,/usr/share/harbour-sukkula/lib
shell shell-runpath -Wl,--enable-new-dtags -Wl,-rpath,/usr/share/harbour-sukkula/lib
shell shell-two-rpaths -Wl,--disable-new-dtags -Wl,-rpath,/usr/share/harbour-sukkula/lib:/opt/lib
shell shell-no-rpath

expect pass "a binary built the way the package is" good
expect fail "a binary that was not stripped" unstripped
expect fail "a main() that is not exported" no-export
expect fail "no RELRO" no-relro
expect fail "lazy binding" lazy
expect fail "not a PIE" no-pie
expect fail "an executable stack" execstack
expect fail "an RPATH" rpath
expect fail "libutil.so.1, which Harbour does not allow" libutil
expect pass "-lutil dropped by --as-needed" libutil-asneeded
expect fail "a sukkula_* entry point exported beside main()" exports-ffi
expect fail "a Rust symbol exported beside main()" exports-rust
expect fail "an archive's symbols exported by -rdynamic" archive-leaks
expect pass "the archive kept out, as src/hardening.pri links" archive-kept-in
reserve=(--tls-reserve 4096)
expect pass "the reserve at tp+16 as the only thread-local" tls-reserved "${reserve[@]}"
expect fail "the reserve beside a zeroed thread-local" tls-zeroed "${reserve[@]}"
expect fail "the reserve beside an initialised thread-local" tls-initialised "${reserve[@]}"
expect fail "thread-locals and no reserve" tls-unreserved "${reserve[@]}"
expect fail "thread-locals and no reserve asked for" tls-unreserved
expect fail "no thread-locals where the reserve is asked for" good "${reserve[@]}"
expect fail "a TLS segment aligned past tp+16" tls-aligned "${reserve[@]}"
engine_rpath=(--rpath /usr/share/harbour-sukkula/lib --tls-reserve 4096)
expect pass "the shell against the engine's library, RPATH as DT_RPATH" shell-rpath "${engine_rpath[@]}"
expect fail "the same with a RUNPATH, which the validator does not read" shell-runpath "${engine_rpath[@]}"
expect fail "an RPATH beyond the engine's directory" shell-two-rpaths "${engine_rpath[@]}"
expect fail "the engine's library needed, and no RPATH to find it" shell-no-rpath "${engine_rpath[@]}"
expect fail "the engine's library needed, and no RPATH asked for" shell-rpath --tls-reserve 4096
exe_full=$full
full=$lib_full
only_abi=(--exports-only sukkula_version)
expect pass "the engine's library, linked as the package's is" lib/libsukkula_ffi.so "${only_abi[@]}"
expect fail "a library whose SONAME is not its file name" lib-soname.so "${only_abi[@]}"
expect fail "a library exporting beyond its export map" lib-exports.so "${only_abi[@]}"
expect fail "a library on the static TLS model" lib-static-tls.so "${only_abi[@]}"
expect fail "a library with an executable stack" lib-execstack.so "${only_abi[@]}"
expect fail "an executable checked as a library" good "${only_abi[@]}"
full=$exe_full
expect fail "a glibc newer than the ceiling" good --glibc-ceiling 2.17
expect fail "the wrong __libc_start_main version" good --libc-start-main 2.17
cases=$((cases + 1))
if "$check" "$(type -P true)" >/dev/null 2>&1; then
    echo "selftest: FAIL an x86-64 binary should fail" >&2
    status=1
else
    echo "selftest: ok   a binary for another architecture -> fail"
fi

echo
if [[ "$status" -eq 0 ]]; then
    echo "selftest: ok ($cases cases)"
else
    echo "selftest: FAILED" >&2
fi
exit "$status"
