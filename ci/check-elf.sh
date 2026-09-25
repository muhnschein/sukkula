#!/bin/bash
# Check one aarch64 ELF file -- the executable, or with --library the
# engine's shared library -- against what the phone and Harbour need.
#
#     ci/check-elf.sh [options] <elf>
#
#   --glibc-ceiling X.Y   no symbol may need a glibc newer than X.Y (the
#                         device's; a newer one fails to load on the phone)
#   --libc-start-main X.Y it must link __libc_start_main@GLIBC_X.Y
#                         (Harbour requires 2.34, rpmvalidation.conf)
#   --main-export         main() must be a defined dynamic symbol, which is
#                         what the silica-qt5 booster dlopen()s and calls
#   --only-main           and nothing else of ours is: every symbol the
#                         binary exports is main() or one the C runtime and
#                         the linker put in every executable (RUNTIME_EXPORTS
#                         below) -- never a Bridge method, a sukkula_* entry
#                         point or a Rust symbol (src/dynamic.list)
#   --stripped            no .symtab: the package ships a stripped binary
#   --rpath DIR           it must carry exactly this RPATH, as DT_RPATH (what
#                         Jolla's validator reads) and no RUNPATH; without
#                         this option, no RPATH or RUNPATH at all
#   --library             a shared library, not an executable: a SONAME that
#                         is its file name, no PT_INTERP, and thread-locals
#                         reached through TLS descriptors or __tls_get_addr,
#                         which work when it is dlopen()ed -- never the
#                         static TLS models (STATIC_TLS, TPREL relocations)
#   --exports-only "A B"  the symbols it exports are exactly these
#   --tls-reserve N       its thread-locals are exactly src/tls_reserve.c's
#                         array: a TLS segment of N zero bytes (.tbss, no
#                         initialisation image) aligned to at most 16, so
#                         it starts at tp+16 and takes the words the phone's
#                         graphics stack uses. Without this option an
#                         executable may have no thread-locals at all
#   --readelf PATH        the readelf to use (default: readelf; GNU readelf
#                         reads any architecture's ELF)
#
# Always checked: every NEEDED library is on Harbour's allowed list
# (ci/harbour/allowed_libraries.conf) or, for the executable, is the
# engine's own library; the link carries the hardening an SDK build of C++
# would -- RELRO, BIND_NOW, PIE for an executable, no text relocations, a
# non-executable stack, only the RPATH asked for -- and an executable's
# thread-locals are the reserve at tp+16 or nothing.
#
# Run on the engine's library and on the probe scripts/cross-build-rust.sh
# links against it, and on both out of the built RPM (rpm.yml):
# /usr/share/harbour-sukkula/lib/libsukkula_ffi.so and
# /usr/bin/harbour-sukkula. The linked-library rule and the glibc versions
# are decided by the link, so no source-level check can answer them.
set -u
shopt -s extglob

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
allowed="$root/ci/harbour/allowed_libraries.conf"

ceiling=""
start_main=""
main_export=0
only_main=0
stripped=0
rpath=""
library=0
exports_only=""
exports_only_set=0
tls_reserve=""
readelf=readelf
elf=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --glibc-ceiling) ceiling=${2:?}; shift 2 ;;
        --libc-start-main) start_main=${2:?}; shift 2 ;;
        --main-export) main_export=1; shift ;;
        --only-main) only_main=1; shift ;;
        --stripped) stripped=1; shift ;;
        --rpath) rpath=${2:?}; shift 2 ;;
        --library) library=1; shift ;;
        --exports-only) exports_only=${2:?}; exports_only_set=1; shift 2 ;;
        --tls-reserve) tls_reserve=${2:?}; shift 2 ;;
        --readelf) readelf=${2:?}; shift 2 ;;
        -*) echo "check-elf: unknown option $1" >&2; exit 2 ;;
        *) elf=$1; shift ;;
    esac
done
[[ -n "$elf" ]] || { echo "usage: $0 [options] <elf>" >&2; exit 2; }
[[ -r "$elf" ]] || { echo "check-elf: FAIL $elf is not readable" >&2; exit 1; }
command -v "$readelf" >/dev/null 2>&1 ||
    { echo "check-elf: FAIL $readelf not found (install binutils)" >&2; exit 1; }

status=0
bad() { echo "check-elf: FAIL $*" >&2; status=1; return 0; }
ok() { echo "check-elf:   ok   $*"; return 0; }

header=$("$readelf" -h "$elf" 2>/dev/null)
dynamic=$("$readelf" -dW "$elf" 2>/dev/null)
segments=$("$readelf" -lW "$elf" 2>/dev/null)

if grep -q 'Machine:.*AArch64' <<< "$header"; then
    ok "an AArch64 ELF"
else
    bad "$elf is not an AArch64 ELF: $(grep 'Machine:' <<< "$header")"
fi

# rpmvalidation.sh's check_contained_in, over allowed_libraries.conf.
lib_allowed() {
    local query=$1 pat
    while read -r pat; do
        [[ -z $pat || $pat == '#'* ]] && continue
        # shellcheck disable=SC2053 # a glob, as upstream matches it
        [[ $query == $pat ]] && return 0
    done < "$allowed"
    return 1
}

needed=$(sed -n 's/.*(NEEDED).*Shared library: \[\(.*\)\]/\1/p' <<< "$dynamic")
if [[ -z "$needed" ]]; then
    bad "no NEEDED entries: not a dynamically linked executable"
fi
while IFS= read -r lib; do
    [[ -n "$lib" ]] || continue
    if lib_allowed "$lib"; then
        ok "links $lib"
    elif [[ "$lib" = libsukkula_ffi.so && -n "$rpath" && "$library" = 0 ]]; then
        # The validator finds it in the package, under /usr/share/<NAME>/lib,
        # and then wants the RPATH checked below.
        ok "links $lib, the engine's own library, through the RPATH"
    else
        bad "links $lib, which is not on Harbour's allowed list (Cannot link to shared library)"
    fi
done <<< "$needed"

# The glibc symbol versions it needs, highest first. A version the device's
# glibc does not define is a binary the loader refuses to start.
versions=$("$readelf" -VW "$elf" 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' | sort -u -V)
highest=$(tail -1 <<< "$versions")
echo "check-elf:        glibc versions needed: $(tr '\n' ' ' <<< "$versions")"
if [[ -n "$ceiling" ]]; then
    if [[ -z "$highest" ]]; then
        bad "no GLIBC_ version needs at all; is this linked against glibc?"
    elif [[ "$(printf '%s\n%s\n' "${highest#GLIBC_}" "$ceiling" | sort -V | tail -1)" != "$ceiling" ]]; then
        bad "needs $highest, newer than the device's glibc $ceiling"
        "$readelf" -sW --dyn-syms "$elf" 2>/dev/null | grep -E "@$highest\b" | head -5 >&2
    else
        ok "needs at most $highest (device glibc $ceiling)"
    fi
fi

if [[ -n "$start_main" ]]; then
    if "$readelf" -sW --dyn-syms "$elf" 2>/dev/null | grep -qE " UND __libc_start_main@GLIBC_$start_main\b"; then
        ok "links __libc_start_main@GLIBC_$start_main (Harbour)"
    else
        bad "does not link __libc_start_main@GLIBC_$start_main, which Harbour requires"
    fi
fi

if [[ "$main_export" = 1 ]]; then
    if "$readelf" -W --dyn-syms "$elf" 2>/dev/null |
        awk '$4 == "FUNC" && $5 == "GLOBAL" && $7 != "UND" && $8 == "main"' | grep -q .; then
        ok "exports main() in .dynsym (the booster's entry point)"
    else
        bad "main() is not a defined dynamic symbol; the booster cannot start it (Q_DECL_EXPORT, -rdynamic)"
    fi
fi

# What an executable exports that is not its author's. The booster needs
# main() and nothing else of ours, and the link is built to export exactly
# that (src/hardening.pri: -fvisibility=hidden, --dynamic-list with main
# alone, --exclude-libs,ALL for the Rust archive). But the SDK's
# sailfishapp feature links with -rdynamic, which also exports every
# global symbol of the objects the driver adds to any executable, and of
# the linker script -- and those are these, no more (read off -rdynamic
# links of a one-function program for aarch64 and x86-64 with GCC 13 and
# binutils 2.42; the three older crt files export are the host test's,
# tests/run-cpp-tests.sh):
#
#   _start                   crt1.o/Scrt1.o: the ELF entry point
#   _IO_stdin_used           crt1.o: tells glibc which stdio ABI it runs
#   __data_start, data_start crt1.o: the start of .data (the second weak)
#   _init, _fini             crti.o: global in older glibc's crt files
#   __dso_handle             crtbegin.o: global in older GCC's
#   _edata, _end, __bss_start
#                            the linker script's section bounds, defined
#                            unconditionally (no PROVIDE) on every target
#   __bss_start__, __bss_end__, _bss_end__, __end__
#                            the same, from the AArch64 (and ARM) script
#
# PROVIDE'd names (end, edata, etext) appear only when something refers
# to them, and nothing here does, so one showing up is news too.
#
# Anything else -- a Bridge method, sukkula_start, a _ZN or _R Rust
# symbol, a libstdc++ template -- is ours leaking, and a leak this check
# exists to fail rather than a host-only test to notice.
RUNTIME_EXPORTS='_start _IO_stdin_used __data_start data_start _init _fini __dso_handle
                 _edata _end __bss_start __bss_start__ __bss_end__ _bss_end__ __end__'
if [[ "$only_main" = 1 ]]; then
    # Defined (Ndx not UND), bound GLOBAL, WEAK or UNIQUE, and visible:
    # what another object can look up. Name and Ndx are read from the end
    # of the line -- AArch64 readelf can print a second Vis word -- after
    # the version index an imported symbol carries ("(2)").
    exports=$("$readelf" -W --dyn-syms "$elf" 2>/dev/null |
        awk '$1 ~ /^[0-9]+:$/ {
                 line = $0; sub(/[[:space:]]+\([0-9]+\)[[:space:]]*$/, "", line)
                 n = split(line, f, " ")
                 if ((f[5] == "GLOBAL" || f[5] == "WEAK" || f[5] == "UNIQUE") &&
                     f[6] != "HIDDEN" && f[6] != "INTERNAL" && f[n - 1] != "UND") print f[n]
             }' |
        sed 's/@.*//' | sort -u)
    extra=""
    while IFS= read -r sym; do
        [[ -n "$sym" && "$sym" != main ]] || continue
        [[ " $(tr -s '[:space:]' ' ' <<< "$RUNTIME_EXPORTS") " == *" $sym "* ]] && continue
        extra+=" $sym"
    done <<< "$exports"
    if [[ -n "$extra" ]]; then
        bad "exports more than main():$extra (src/dynamic.list; -fvisibility=hidden; --exclude-libs,ALL)"
    else
        ok "exports main() and nothing else of its own"
    fi
fi

if [[ "$exports_only_set" = 1 ]]; then
    # Defined, bound GLOBAL, WEAK or UNIQUE, and visible, as above; and no
    # version nodes (an unversioned library defines none).
    exported=$("$readelf" -W --dyn-syms "$elf" 2>/dev/null |
        awk '$1 ~ /^[0-9]+:$/ {
                 line = $0; sub(/[[:space:]]+\([0-9]+\)[[:space:]]*$/, "", line)
                 n = split(line, f, " ")
                 if ((f[5] == "GLOBAL" || f[5] == "WEAK" || f[5] == "UNIQUE") &&
                     f[6] != "HIDDEN" && f[6] != "INTERNAL" && f[n - 1] != "UND") print f[n]
             }' |
        sed 's/@.*//' | sort -u)
    want=$(tr -s '[:space:]' '\n' <<< "$exports_only" | sed '/^$/d' | sort -u)
    if [[ "$exported" = "$want" ]]; then
        ok "exports exactly: $(tr '\n' ' ' <<< "$want")"
    else
        bad "exports $(tr '\n' ' ' <<< "$exported")-- not exactly $(tr '\n' ' ' <<< "$want")(crates/sukkula-ffi/exports.map)"
    fi
fi

if [[ "$stripped" = 1 ]]; then
    if "$readelf" -SW "$elf" 2>/dev/null | grep -q ' \.symtab '; then
        bad "not stripped: .symtab is still there"
    else
        ok "stripped"
    fi
fi

# The hardening, read back off the ELF: a flag that stops being applied is
# invisible anywhere else.
if grep -q 'GNU_RELRO' <<< "$segments"; then ok "RELRO"; else bad "no RELRO segment (-Wl,-z,relro)"; fi
if grep -qE '\(FLAGS\).*BIND_NOW|\(FLAGS_1\).*NOW' <<< "$dynamic"; then
    ok "BIND_NOW"
else
    bad "lazy binding (-Wl,-z,now)"
fi
if [[ "$library" = 1 ]]; then
    soname=$(sed -n 's/.*(SONAME).*Library soname: \[\(.*\)\]/\1/p' <<< "$dynamic")
    if ! grep -q 'Type:.*DYN' <<< "$header" || grep -q 'INTERP' <<< "$segments"; then
        bad "not a shared library (ET_DYN without PT_INTERP)"
    elif [[ "$soname" != "$(basename -- "$elf")" ]]; then
        bad "its SONAME is '$soname', not its file name $(basename -- "$elf"): the executable would record a NEEDED the RPATH does not find"
    else
        ok "a shared library, SONAME $soname"
    fi
elif grep -q 'Type:.*DYN' <<< "$header" && grep -q 'INTERP' <<< "$segments"; then
    ok "PIE"
else
    bad "not a position-independent executable (-pie)"
fi
if grep -q '(TEXTREL)' <<< "$dynamic"; then bad "text relocations"; else ok "no text relocations"; fi
if grep -E 'GNU_STACK' <<< "$segments" | grep -q 'RWE'; then
    bad "executable stack"
else
    ok "non-executable stack"
fi
# The one RPATH asked for, as DT_RPATH -- the validator reads "Library
# rpath:" and nothing else, so a RUNPATH would fail it -- or none.
got_rpath=$(sed -n 's/.*(RPATH).*Library rpath: \[\(.*\)\]/\1/p' <<< "$dynamic")
if grep -q '(RUNPATH)' <<< "$dynamic"; then
    bad "carries a RUNPATH, which Jolla's validator does not read: $(grep '(RUNPATH)' <<< "$dynamic") (-Wl,--disable-new-dtags)"
elif [[ -n "$rpath" && "$got_rpath" = "$rpath" ]]; then
    ok "RPATH $rpath, and nothing else"
elif [[ -n "$rpath" ]]; then
    bad "its RPATH is '${got_rpath}', not exactly $rpath"
elif [[ -n "$got_rpath" ]]; then
    bad "carries an RPATH: $got_rpath"
else
    ok "no RPATH or RUNPATH"
fi

# The words at the thread pointer (src/tls_reserve.c). The phone's GL
# stack keeps bionic's TLS slots and its own thread-locals there, from tp+0
# on, and glibc puts the process's first TLS block at tp+16. So an
# executable's thread-locals must be the reserve alone: N bytes, all zero
# (.tbss, which Android code reads as the starting state bionic would give
# it), in a segment aligned to at most 16 bytes -- a larger alignment
# starts the block past tp+16 and leaves the gap to some library. An
# executable with no thread-locals leaves tp+16 to the first library, so
# the shell passes --tls-reserve.
tls=$(awk '$1 == "TLS" { print $5, $6, $NF }' <<< "$segments")
if [[ "$library" = 1 ]]; then
    # A library's thread-locals, when the executable that needs it is
    # dlopen()ed by the booster: a TLS descriptor or __tls_get_addr finds
    # them wherever glibc allocates them, but the static models (a TPREL
    # relocation, which STATIC_TLS announces) need a fixed offset that a
    # dlopen()ed library may not get.
    if grep -qE '\(FLAGS\).*STATIC_TLS' <<< "$dynamic" ||
        "$readelf" -rW "$elf" 2>/dev/null | grep -q 'TLS_TPREL'; then
        bad "uses the static TLS model (STATIC_TLS, TPREL relocations): its thread-locals need a fixed offset a dlopen()ed library may not get"
    elif [[ -z "$tls" ]]; then
        ok "no thread-locals"
    else
        ok "thread-locals reached through TLS descriptors, wherever glibc puts them"
    fi
elif [[ -z "$tls" && -z "$tls_reserve" ]]; then
    ok "no thread-locals of its own"
elif [[ -z "$tls" ]]; then
    bad "no thread-locals, so the first library's are at tp+16, where the GL stack writes: link src/tls_reserve.c"
elif [[ -z "$tls_reserve" ]]; then
    bad "thread-locals of its own at tp+16, where the GL stack writes (src/tls_reserve.c; --tls-reserve)"
else
    read -r tls_filesz tls_memsz tls_align <<< "$tls"
    if (( tls_align > 16 )); then
        bad "its TLS segment is aligned to $((tls_align)) bytes: the block starts past tp+16 and leaves the gap to a library's thread-locals (src/tls_reserve.c)"
    elif (( tls_filesz != 0 )); then
        bad "its TLS segment has $((tls_filesz)) initialised bytes: the reserve is all zero, and nothing else of the executable may be a thread-local (src/tls_reserve.c)"
    elif (( tls_memsz != tls_reserve )); then
        bad "its TLS segment is $((tls_memsz)) bytes, not the reserve's $tls_reserve: something else of the executable is a thread-local (src/tls_reserve.c)"
    else
        ok "its only thread-locals are the reserve: $tls_reserve zero bytes at tp+16"
    fi
fi

if [[ "$status" -eq 0 ]]; then
    echo "check-elf: ok ($elf)"
else
    echo "check-elf: FAILED ($elf)" >&2
fi
exit "$status"
