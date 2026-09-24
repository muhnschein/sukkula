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
command -v "$cc" >/dev/null 2>&1 || { echo "selftest: FAIL $cc not found" >&2; exit 1; }
command -v "$strip" >/dev/null 2>&1 || { echo "selftest: FAIL $strip not found" >&2; exit 1; }

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
full="--main-export --stripped --libc-start-main 2.34 --glibc-ceiling 2.39"
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
