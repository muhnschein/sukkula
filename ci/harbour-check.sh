#!/bin/bash
# Harbour listing rules, checked against the source tree.
#
# `sfdk check -s harbour` is the authority, and it needs a built RPM and
# the Sailfish SDK -- minutes of runner time and a multi-gigabyte image
# (.github/workflows/rpm.yml, where ci/harbour-validate-rpm.sh runs it). This
# answers the same questions from the sources instead, so a change that
# would fail intake fails the pull request that introduces it. Spec §7
# makes it a gate; docs/HARBOUR.md is the map.
#
# The rules come from the vendored copies of the validator's own
# allow-lists (ci/harbour/, refreshed by scripts/update-harbour-rules.sh);
# only the logic around them is reimplemented, from rpmvalidation.sh. Check
# IDs are docs/HARBOUR.md's. Numeric ones follow the numbering this check
# shares with its siblings (piirit, vuo); `P.n` ones are Sukkula's own
# policy, stricter than Harbour: aarch64 and Sailfish OS 5.2+ only, exactly
# the three sandbox permissions of spec §2, exactly the platform QML modules
# spec §2 names.
#
# Missing inputs are failures, never skips: a tree without its .desktop
# file, icons, QML or C++ entry point is a tree Harbour would reject, and a
# check that passed it would be saying so. Only a missing *tool* is a skip,
# and HARBOUR_CHECK_STRICT=1 (CI) turns that into a failure too.
#
# bash, not sh, and deliberately: the .conf files are written as bash
# extglob patterns (`libcrypto.so.3?((OPENSSL_3.*))`), and matching them
# with anything else would silently accept what Harbour rejects.
set -u
shopt -s extglob

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
rules="$root/ci/harbour"
waivers="$rules/waivers.conf"
# shellcheck source=ci/harbour-waivers.sh
. "$root/ci/harbour-waivers.sh"

# The one device architecture (spec §2). rpmvalidation.conf's ICON_SIZES.
ARCH=aarch64
ICON_SIZES="86x86 108x108 128x128 172x172"

# Spec §2: these three and nothing else.
POLICY_PERMISSIONS="Bluetooth Downloads Internet"
# The sandbox names the app's data path is built from. Fixed: the LocalSend
# certificate and the settings live under them (spec F-LS2), so renaming
# either silently gives every user a new identity.
POLICY_ORG=sukkula
POLICY_APP=sukkula
# Spec §2's platform QML modules. Qt's own modules are Harbour's to judge,
# and the app's own module is ours; everything under a platform namespace
# has to be one of these.
POLICY_QML_MODULES="Sailfish.Silica Sailfish.Share Sailfish.Pickers Nemo.KeepAlive Nemo.Notifications"
# One extglob: `|` alternates only inside @( ).
POLICY_QML_NAMESPACES='@(Sailfish|Nemo|Amber|Mer|Meego|NemoMobile|Bluetooth|org.nemomobile|org.sailfishos|com.jolla|com.nokia|com.meego|org.kde).*'
# The package that provides each module above, where one exists on
# Harbour's allowed_requires.conf. Silica, Share and Pickers ship with the
# platform (sailfishsilica-qt5 is required unconditionally).
qml_module_package() {
    case "$1" in
        Nemo.Notifications) echo "nemo-qml-plugin-notifications-qt5 qml(Nemo.Notifications)" ;;
        Nemo.KeepAlive) echo "libkeepalive qml(Nemo.KeepAlive)" ;;
        *) echo "" ;;
    esac
}

status=0
findings=0
skipped=0

# This check's waivers: the `source` lines of ci/harbour/waivers.conf, by
# line number. The `rpm` lines are ci/harbour-validate-rpm.sh's and are
# neither used nor stale here -- but every line is parsed, so a malformed
# one of either kind fails on the pull request that writes it.
declare -a waiver_line=() waiver_id=() waiver_subject=() waiver_message=()
declare -A waiver_used=()
if records=$(waiver_records "$waivers"); then :; else status=1; fi
while IFS=$'\t' read -r wline wchecker wid wsubject wmessage; do
    [[ "${wchecker:-}" = source ]] || continue
    waiver_line+=("$wline")
    waiver_id+=("$wid")
    waiver_subject+=("$wsubject")
    waiver_message+=("$wmessage")
done <<< "$records"

# HARBOUR_CHECK_STRICT=1 turns "the tool for this check is missing" into a
# failure. CI sets it, so a check can never quietly stop running there.
strict=${HARBOUR_CHECK_STRICT:-0}

note() { echo "harbour-check: $*"; return 0; }

skip() {
    local id=$1 why=$2
    if [[ "$strict" = 1 ]]; then
        echo "harbour-check: FAIL [$id] cannot run: $why (strict mode)" >&2
        status=1
    else
        echo "harbour-check: SKIP [$id] $why"
        skipped=$((skipped + 1))
    fi
    return 0
}

# A finding is (check id, subject, message). The subject has to be the
# stable part -- a path, a dependency, an import -- so that a waiver's
# subject glob keeps naming it; the message glob names which of the
# findings about it is the known one.
fail() {
    local id=$1 subject=$2 message=$3
    local key
    if key=$(waived "$id" "$subject" "$message"); then
        waiver_used[$key]=1
        echo "harbour-check: WAIVED [$id] $subject -- $message"
        return 0
    fi
    echo "harbour-check: FAIL [$id] $subject -- $message" >&2
    findings=$((findings + 1))
    status=1
    return 0
}

# A `source` waiver whose id, subject glob and message glob all match.
# Prints the waiver's line number, so one that stops matching is reported.
waived() {
    local id=$1 subject=$2 message=$3 i
    for i in "${!waiver_line[@]}"; do
        # Unquoted on purpose: the patterns are globs, as upstream's own
        # allow-list matching is.
        # shellcheck disable=SC2053
        if [[ "$id" = "${waiver_id[$i]}" ]] && [[ $subject == ${waiver_subject[$i]} ]] &&
            [[ $message == ${waiver_message[$i]} ]]; then
            echo "${waiver_line[$i]}"
            return 0
        fi
    done
    return 1
}

# rpmvalidation.sh's check_contained_in: match a query against the
# patterns in one or more .conf files, comments and blank lines ignored.
contained_in() {
    local query=$1 pat
    shift
    while read -r pat; do
        [[ -z $pat || $pat == '#'* ]] && continue
        # shellcheck disable=SC2053
        [[ $query == $pat ]] && return 0
        # shellcheck disable=SC2053
        [[ $query == "$pat()(64bit)" ]] && return 0
    done < <(cat "$@")
    return 1
}

# Source files with their comments cut out: what the compiler, and so the
# binary, actually sees, as `file:line:code` for every line with code left
# on it. Only real comments go. Every language read here -- Rust, C++, QML,
# JavaScript -- writes them `//` to the end of the line and `/* ... */`
# (which may span lines, so the state is carried), and none of them uses
# `#`: a line starting with `#` is a Rust attribute (`#[error("...")]`, a
# string that reaches the binary), a C preprocessor line (`#define`), or a
# JavaScript private field, and is kept, as is a C line starting with `*`
# outside a comment (`*out = "...";`). The first version dropped every
# line starting with `#` or `*` (finding "Harbour source gate skips every
# '#' line").
#
# A comment marker inside a string is not one, so strings are tracked:
# "...", Rust's r#"..."# and its b/c/br/cr forms, and a char literal, whose
# quote must not open a string ('"'); in QML and JavaScript '...' and
# `...` are strings instead. A Rust string or a template literal may run
# over lines. Anything this misreads it misreads towards keeping text, so a
# hardcoded path is never hidden by the parser -- only a real comment is.
code_lines() {
    awk '
    FNR == 1 { block = 0; instr = ""; raw = ""
               js = (FILENAME ~ /\.(qml|js)$/); rust = (FILENAME ~ /\.rs$/) }
    {
        line = $0; n = length(line); out = ""; i = 1
        # A plain string ends at its line in C++ and JavaScript; a Rust
        # string and a template literal carry on.
        if (instr != "" && !(rust || instr == "`")) instr = ""
        # Most lines hold no comment or string at all.
        if (!block && raw == "" && instr == "" && line !~ /[\/"\047`]/) {
            if (line ~ /[^[:space:]]/) print FILENAME ":" FNR ":" line
            next
        }
        while (i <= n) {
            c = substr(line, i, 1); c2 = substr(line, i, 2)
            if (block) {
                if (c2 == "*/") { block = 0; i += 2; out = out " " } else i++
                continue
            }
            if (raw != "") {
                if (substr(line, i, length(raw)) == raw) {
                    out = out raw; i += length(raw); raw = ""
                } else { out = out c; i++ }
                continue
            }
            if (instr != "") {
                out = out c
                if (c == "\\") { out = out substr(line, i + 1, 1); i += 2; continue }
                if (c == instr) instr = ""
                i++
                continue
            }
            if (c2 == "//") break
            if (c2 == "/*") { block = 1; i += 2; continue }
            if (rust && match(substr(line, i), /^(b|c)?r#*"/) &&
                (i == 1 || substr(line, i - 1, 1) !~ /[A-Za-z0-9_]/)) {
                hashes = RLENGTH - 2 - (substr(line, i, 1) != "r")
                raw = "\""
                for (h = 0; h < hashes; h++) raw = raw "#"
                out = out substr(line, i, RLENGTH); i += RLENGTH
                continue
            }
            if (c == "\"" || (js && (c == "\047" || c == "`"))) {
                instr = c; out = out c; i++
                continue
            }
            # A char literal: an escape, a quote or a slash alone, or up to
            # four bytes (one UTF-8 character) with neither in them -- so
            # a lifetime (`&\047a str`) never swallows a string or a comment.
            if (!js && c == "\047" &&
                match(substr(line, i), /^\047(\\[^\047]+|"|\/|[^\\\047"\/][^\047"\/]?[^\047"\/]?[^\047"\/]?)\047/)) {
                out = out substr(line, i, RLENGTH); i += RLENGTH
                continue
            }
            out = out c; i++
        }
        if (out ~ /[^[:space:]]/) print FILENAME ":" FNR ":" out
    }' "$@" 2>/dev/null || true
}

#
# Inputs. Found rather than named, so the check follows the package's own
# idea of what it is called; every one of them is required.
#
spec=$(find "$root/rpm" -maxdepth 1 -name '*.spec' 2>/dev/null | sort | head -1)
if [[ -z "$spec" ]]; then
    echo "harbour-check: FAIL no .spec file in rpm/" >&2
    exit 1
fi

name=$(sed -n 's/^Name:[[:space:]]*//p' "$spec" | head -1 | tr -d '[:space:]')
version=$(sed -n 's/^Version:[[:space:]]*//p' "$spec" | head -1 | tr -d '[:space:]')
release=$(sed -n 's/^Release:[[:space:]]*//p' "$spec" | head -1 | tr -d '[:space:]')
if [[ -z "$name" ]]; then
    echo "harbour-check: FAIL $spec has no Name:" >&2
    exit 1
fi

# 1.3.3 wants Icon=<NAME> and 1.2.2 wants the file installed as
# <NAME>.desktop, so the source file is named for the package too. The
# .pro is what qmake builds the binary from, and TARGET is its name.
desktop="$root/$name.desktop"
pro="$root/$name.pro"
main_cpp="$root/src/main.cpp"
workflow="$root/.github/workflows/rpm.yml"

#
# 1.1 Naming
#
if [[ $name =~ ^harbour-[-a-z0-9_.]+$ ]]; then
    note "[1.1.1] package name '$name' is a valid Harbour name"
else
    fail 1.1.1 "$name" \
        "package name must match '^harbour-[-a-z0-9_.]+\$' (lowercase, harbour- prefix)"
fi

if [[ $version =~ ^[0-9.]+$ ]]; then
    note "[1.1.3] Version '$version' is digits and periods"
else
    fail 1.1.3 "$version" "Version may contain only digits and periods"
fi

# The tree's own Release. What a built package actually carries is what
# rpm.yml stamps, checked below.
if [[ $release =~ ^[0-9._]+$ ]]; then
    note "[1.1.4] Release '$release' is digits, underscores and periods"
else
    fail 1.1.4 "$release" "Release may contain only digits, underscores and periods"
fi

if [[ ! -f "$workflow" ]]; then
    fail 1.1.4 ".github/workflows/rpm.yml" \
        "the rpm workflow is missing, so nothing decides what a built package is called"
else
    # The Release the workflow stamps is what reaches an RPM, so it is the
    # one Harbour sees. Every assignment is judged, not only the first.
    stamps=0
    while IFS= read -r stamped; do
        [[ -n "$stamped" ]] || continue
        stamps=$((stamps + 1))
        # Substitute a plausible value for each shell expansion, then
        # judge the shape that leaves.
        # shellcheck disable=SC2016 # the patterns are the literal text.
        shape=$(sed -e 's/\${GITHUB_RUN_NUMBER}/17/g' -e 's/\$GITHUB_RUN_NUMBER/17/g' \
                    -e 's/\${GITHUB_RUN_ATTEMPT}/1/g' -e 's/\${short}/abc1234/g' <<< "$stamped")
        if [[ $shape =~ ^[0-9._]+$ ]]; then
            note "[1.1.4] the rpm workflow stamps a Harbour-legal Release ('$stamped')"
        else
            fail 1.1.4 "$stamped" \
                "the Release stamped by rpm.yml expands to '$shape', which is not digits, underscores and periods"
        fi
        # 1.1.2: harbour-NAME-VERSION-RELEASE.ARCH.rpm, at most 100
        # characters (vuo's rpm.yml measured the rule).
        file="$name-$version-$shape.$ARCH.rpm"
        if [[ ${#file} -gt 100 ]]; then
            fail 1.1.2 "$file" "the RPM file name is ${#file} characters; Harbour allows 100"
        fi
    done < <(sed -n 's/.*[[:space:]]release="\([^"]*\)".*/\1/p' "$workflow")
    if [[ "$stamps" -eq 0 ]]; then
        fail 1.1.4 ".github/workflows/rpm.yml" \
            "found no 'release=\"...\"' stamp; without one every build is the same NEVRA"
    fi
fi

#
# 1.1.5 and P.1: the architecture. Harbour takes armv7hl, aarch64, i486 or
# noarch; Sukkula builds aarch64 and nothing else, with no per-architecture
# branches anywhere in the packaging.
#
exclusive=$(sed -n 's/^ExclusiveArch:[[:space:]]*//p' "$spec" | tr -s '[:space:]' ' ' | sed 's/ $//')
if [[ "$exclusive" = "$ARCH" ]]; then
    note "[1.1.5] the spec builds $ARCH only"
else
    fail 1.1.5 "ExclusiveArch" \
        "the spec must say 'ExclusiveArch: $ARCH' and nothing else (found '${exclusive:-none}'); Sukkula targets the Jolla Phone 2026 only"
fi
if grep -qE '^[[:space:]]*%if(n)?arch\b' "$spec"; then
    fail P.1 "%ifarch" "per-architecture branches in the spec; there is one architecture"
fi
# Comments are dropped first: saying *why* there is no armv7hl build is
# documentation, building one is baggage.
for f in "$spec" "$workflow" "$root/.github/workflows/sdk-image.yml" \
         "$root/ci/build-sdk-image.sh" "$root/scripts/cross-build-rust.sh"; do
    [[ -f "$f" ]] || continue
    rel=${f#"$root"/}
    while IFS= read -r hit; do
        [[ -n "$hit" ]] || continue
        fail P.1 "$rel:${hit%%:*}" "names another device architecture: ${hit#*:}"
    done < <(sed 's/[[:space:]]#.*//; s/^[[:space:]]*#.*//' "$f" | grep -nE '\b(armv7hl|armv7|armhf|i486|i686)\b' || true)
done

#
# P.3: Sailfish OS 5.2 and later. Harbour requires the binary to link
# __libc_start_main@GLIBC_2.34, which only a 5.x SDK's glibc provides, and
# the Jolla Phone 2026 ships 5.2; every SDK version the packaging can be
# asked to build against has to be at least that.
#
for f in "$workflow" "$root/.github/workflows/sdk-image.yml" "$root/ci/build-sdk-image.sh"; do
    [[ -f "$f" ]] || continue
    rel=${f#"$root"/}
    while IFS= read -r v; do
        [[ -n "$v" ]] || continue
        major=${v%%.*}
        rest=${v#*.}
        minor=${rest%%.*}
        if (( major < 5 || (major == 5 && minor < 2) )); then
            fail P.3 "$rel:$v" "Sailfish OS $v is older than 5.2, the Jolla Phone 2026's baseline"
        fi
    done < <(grep -v '^[[:space:]]*#' "$f" | grep -oE '\b[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\b' | sort -u)
done
note "[P.3] every SDK version the packaging names is 5.2 or later"

#
# P.6: the authority runs on every pull request that changes the package.
# This check reads sources and cannot see what the link and rpm make of
# them, so rpm.yml runs Jolla's validator on the built RPM for any pull
# request touching what the package is built from -- the Rust crates and
# the C++ shell (the binary), the QML (strings(1) reads every shipped
# file), and the packaging. A pull_request trigger with no `paths:` runs
# on every pull request, which covers them too. The paths once left out
# crates/, src/ and qml/, so a home directory in a Rust attribute passed
# both gates (finding "Harbour source gate skips every '#' line").
#
P6_PATHS=("rpm/**" "crates/**" "src/**" "qml/**" "icons/**" "translations/**" "third_party/**"
          "Cargo.lock" "**/Cargo.toml" "**/build.rs" "rust-toolchain.toml"
          "$name.pro" "$name.desktop")
if [[ -f "$workflow" ]]; then
    # The `on.pull_request` block, as written in this tree: the trigger's
    # own line, then its `paths:` items (block style, one per line).
    pr=$(awk '
        /^on:[[:space:]]*$/ { on = 1; next }
        on && /^[^[:space:]#]/ { on = 0 }
        !on { next }
        /^  pull_request:/ { pr = 1; line = $0; sub(/^  pull_request:[[:space:]]*/, "", line)
                             sub(/[[:space:]]*#.*/, "", line); print "TRIGGER " line; next }
        pr && /^  [^[:space:]#]/ { pr = 0 }
        !pr { next }
        /^    paths-ignore:/ { print "IGNORE"; paths = 0; next }
        /^    paths:/ { print "PATHS"; paths = 1; next }
        /^    [^[:space:]#-]/ { paths = 0 }
        paths && /^[[:space:]]*- / { item = $0; sub(/^[[:space:]]*- [[:space:]]*/, "", item)
                                     sub(/[[:space:]]+#.*/, "", item); gsub(/["\047]/, "", item)
                                     print "PATH " item }
    ' "$workflow")
    if ! grep -q '^TRIGGER' <<< "$pr"; then
        fail P.6 ".github/workflows/rpm.yml" \
            "no pull_request trigger: Jolla's validator never runs before a change merges"
    elif grep -qE '^TRIGGER .+' <<< "$pr" || grep -q '^IGNORE' <<< "$pr"; then
        fail P.6 ".github/workflows/rpm.yml" \
            "write pull_request's paths as a block list, without paths-ignore, so this check can read them"
    elif grep -q '^PATHS' <<< "$pr"; then
        missing=0
        for want in "${P6_PATHS[@]}"; do
            grep -qxF "PATH $want" <<< "$pr" && continue
            missing=1
            fail P.6 "$want" \
                "rpm.yml's pull_request paths leave it out, so a change there merges without Jolla's validator"
        done
        [[ "$missing" = 1 ]] ||
            note "[P.6] rpm.yml runs the validator on every pull request that changes the package"
    else
        note "[P.6] rpm.yml runs the validator on every pull request"
    fi
fi

#
# 1.2 Installed file layout, and 1.8, all read off the spec
#
expanded=""
if ! command -v rpmspec >/dev/null 2>&1; then
    skip 1.2.1 "rpmspec not found (install rpm)"
else
    expanded=$(rpmspec -P --target "$ARCH" "$spec" 2>/dev/null)
    if [[ -z "$expanded" ]]; then
        fail 1.2.1 "$spec" "does not parse for $ARCH"
    fi
fi

if [[ -n "$expanded" ]]; then
    # %files, minus the directives and attributes that decorate an entry
    # rather than name a path.
    files_section=$(sed -n '/^%files/,/^%[a-z]*$/p' <<< "$expanded" | grep -v '^%files' | grep -v '^%changelog')
    files=$(grep -v '^%defattr' <<< "$files_section" |
        sed -e 's/^%dir[[:space:]]*//' -e 's/^%attr([^)]*)[[:space:]]*//' \
            -e 's/^%config([^)]*)[[:space:]]*//' -e 's/^%verify([^)]*)[[:space:]]*//' |
        grep -v '^[[:space:]]*$' || true)

    while read -r path; do
        [[ -n "$path" ]] || continue
        case "$path" in
            "/usr/bin/$name") ;;
            "/usr/share/applications/$name.desktop") ;;
            "/usr/share/icons/hicolor/"*"/apps/$name.png") ;;
            "/usr/share/$name"|"/usr/share/$name/"*) ;;
            %doc*|%license*)
                # rpm invents the path for these, under the doc and licence
                # directories, and both are outside what Harbour allows.
                fail 1.2.1 "$path" \
                    "%doc and %license install outside /usr/share/$name; install the file into the data directory instead"
                ;;
            /home/*)
                fail 1.2.6 "$path" "nothing may be installed under /home"
                ;;
            /usr/lib/debug*|/usr/src/debug*)
                fail 1.2.4 "$path" "debug symbols and sources must not be packaged"
                ;;
            *)
                fail 1.2.1 "$path" \
                    "installation not allowed in this location; only /usr/bin/$name, /usr/share/applications/$name.desktop, /usr/share/icons/hicolor/<size>/apps/$name.png and /usr/share/$name/** may be packaged"
                ;;
        esac
    done <<< "$files"
    note "[1.2.1] every %files entry is a location Harbour allows"

    if grep -qx "/usr/share/applications/$name.desktop" <<< "$files"; then
        note "[1.2.2] the package ships /usr/share/applications/$name.desktop"
    else
        fail 1.2.2 "/usr/share/applications/$name.desktop" "the .desktop file is not in %files"
    fi

    # 1.2.3: a C++/QML app, not a sailfish-qml one, so the binary is there.
    if grep -qx "/usr/bin/$name" <<< "$files"; then
        note "[1.2.3] the package ships /usr/bin/$name"
    else
        fail 1.2.3 "/usr/bin/$name" "a C++/QML app must install its binary as /usr/bin/$name"
    fi

    if grep -qE "^/usr/share/icons/hicolor/[^ ]*/apps/$name\.png$" <<< "$files"; then
        note "[1.5.2] the package ships its icons"
    else
        fail 1.5.2 "/usr/share/icons/hicolor/*/apps/$name.png" "the icons are not in %files"
    fi

    # 1.2.8: ownership and special bits, as %files states them. The
    # validator wants every file owned by root:root.
    while IFS= read -r attr; do
        [[ -n "$attr" ]] || continue
        inner=${attr#*[(]}
        inner=${inner%%[)]*}
        IFS=',' read -r a_mode a_user a_group _ <<< "$inner"
        a_mode=${a_mode//[[:space:]]/}
        a_user=${a_user//[[:space:]]/}
        a_group=${a_group//[[:space:]]/}
        for who in "$a_user" "$a_group"; do
            case "$who" in
                -|root|"") ;;
                *) fail 1.2.8 "$attr" "files must be owned by root:root, not '$who'" ;;
            esac
        done
        if [[ $a_mode =~ ^[0-7]{4}$ ]] && [[ ${a_mode:0:1} != 0 ]]; then
            fail 1.2.8 "$attr" "setuid, setgid or sticky bit set"
        fi
        if [[ $a_mode =~ ^[0-7]{3,4}$ ]]; then
            [[ ${a_mode: -1} == [2367] ]] && fail 1.2.7 "$attr" "world-writable"
            [[ ${a_mode: -2:1} == [2367] ]] && fail 1.2.7 "$attr" "group-writable"
        fi
    done < <(grep -oE '%(def)?attr\([^)]*\)' <<< "$files_section" || true)

    # %install's install(1) calls: the mode and destination of every file
    # installed by hand. Continuation lines are joined first, so that a call
    # split across two lines is still one source and one destination.
    install_section=$(sed -n '/^%install/,/^%files/p' <<< "$expanded" |
        sed -e ':a' -e '/\\$/{N;s/\\\n[[:space:]]*/ /;ta' -e '}' |
        sed -E 's#[^ ]*/BUILDROOT/[^/ ]+##g')
    while read -r line; do
        case "$line" in install*) ;; *) continue ;; esac
        mode=$(sed -n 's/.*-D\{0,1\}m[[:space:]]*\([0-7]\{3,4\}\).*/\1/p' <<< "$line")
        [[ -n "$mode" ]] || continue
        [[ ${#mode} -eq 3 ]] && mode="0$mode"
        dst=$(awk '{print $NF}' <<< "$line")
        [[ ${mode:0:1} == 0 ]] || fail 1.2.8 "$dst" "setuid, setgid or sticky bit set (mode $mode)"
        [[ ${mode:3:1} == [2367] ]] && fail 1.2.7 "$dst" "world-writable (mode $mode)"
        [[ ${mode:2:1} == [2367] ]] && fail 1.2.7 "$dst" "group-writable (mode $mode)"
    done <<< "$install_section"
    note "[1.2.7/1.2.8] install modes and ownership checked"

    #
    # 1.8 RPM metadata. Only the tags the spec states: rpm generates
    # Requires and Provides from the built binary too, and those are the
    # real validator's to judge (rpm.yml) -- except the one generated
    # Requires this tree knows about, 1.8.8 below.
    #
    if grep -qE '^Vendor:' <<< "$expanded"; then
        fail 1.8.1 "Vendor" "a Vendor: tag must not be set"
    else
        note "[1.8.1] no Vendor: tag"
    fi

    for tag in Provides Obsoletes Conflicts Recommends Suggests Supplements Enhances; do
        while read -r value; do
            [[ -n "$value" ]] || continue
            fail 1.8.2 "$tag: $value" "'$tag:' is not allowed in a Harbour RPM"
        done < <(sed -n "s/^$tag:[[:space:]]*//p" <<< "$expanded")
    done
    note "[1.8.2] no Provides:/Obsoletes:/Conflicts:/Recommends:/Suggests:/Supplements:/Enhances:"

    # BuildRequires are the SDK's business and are not shipped, so only
    # runtime Requires are judged. Requires(post) and friends are
    # scriptlet dependencies, and there are no scriptlets (1.8.5).
    requires=$(sed -n 's/^Requires\(([^)]*)\)\{0,1\}:[[:space:]]*//p' <<< "$expanded")
    while read -r req; do
        [[ -n "$req" ]] || continue
        # rpm hands the validator each whitespace-separated token, so a
        # versioned dependency arrives as three of them and the operator
        # and the version are both rejected.
        if [[ $req == *[[:space:]]* ]]; then
            fail 1.8.4 "$req" \
                "Requires: must not be versioned -- Harbour derives its own compatibility range"
            req=${req%%[[:space:]]*}
        fi
        if contained_in "$req" "$rules/allowed_libraries.conf" "$rules/allowed_requires.conf"; then
            continue
        fi
        if contained_in "$req" "$rules/deprecated_libraries.conf" "$rules/deprecated_requires.conf"; then
            fail 1.8.3 "$req" "dependency is deprecated and will stop being accepted"
            continue
        fi
        if contained_in "$req" "$rules/dropped_libraries.conf" "$rules/dropped_requires.conf"; then
            fail 1.8.3 "$req" "dependency was dropped from the platform and is no longer accepted"
            continue
        fi
        fail 1.8.3 "$req" "dependency is not on Harbour's allowed list"
    done <<< "$requires"
    note "[1.8.3/1.8.4] Requires: checked against ci/harbour/{allowed,deprecated,dropped}_*.conf"

    for scriptlet in pre post preun postun pretrans posttrans preuntrans postuntrans \
                     verifyscript triggerprein triggerin triggerun triggerpostun \
                     filetriggerin filetriggerun filetriggerpostun \
                     transfiletriggerin transfiletriggerun transfiletriggerpostun; do
        if grep -qE "^%$scriptlet\b" <<< "$expanded"; then
            fail 1.8.5 "%$scriptlet" "RPM scriptlets and triggers are not allowed"
        fi
    done
    note "[1.8.5] no RPM scriptlets or triggers"

    # 1.8.8: the engine links the system libdbus-1.so.3, whose whole API is
    # versioned LIBDBUS_1_3, so rpm derives
    # libdbus-1.so.3(LIBDBUS_1_3)(64bit) -- which allowed_requires.conf does
    # not carry and the validator rejects. The spec has to filter exactly
    # that string, and keep the unversioned libdbus-1.so.3()(64bit), which
    # is allowed and is the real dependency.
    if grep -q '^name = "libdbus-sys"$' "$root/Cargo.lock" 2>/dev/null; then
        exclude=$(sed -n 's/^%global[[:space:]]\{1,\}__requires_exclude[[:space:]]\{1,\}//p' "$spec" | head -1)
        exclude=${exclude//\\\\/\\}
        versioned='libdbus-1.so.3(LIBDBUS_1_3)(64bit)'
        if [[ -z "$exclude" ]]; then
            fail 1.8.8 "$versioned" \
                "the engine links libdbus-1 and rpm will require '$versioned', which Harbour rejects; filter it with __requires_exclude"
        elif ! grep -qE -- "$exclude" <<< "$versioned"; then
            fail 1.8.8 "$versioned" "__requires_exclude ('$exclude') does not filter it"
        else
            for keep in 'libdbus-1.so.3()(64bit)' 'libc.so.6(GLIBC_2.34)(64bit)' \
                        'libQt5Core.so.5(Qt_5)(64bit)' 'libsailfishapp.so.1()(64bit)'; do
                if grep -qE -- "$exclude" <<< "$keep"; then
                    fail 1.8.8 "$keep" \
                        "__requires_exclude ('$exclude') also drops this real dependency; filter only '$versioned'"
                fi
            done
            note "[1.8.8] the spec filters libdbus-1's versioned Requires and nothing else"
        fi
    fi
fi

#
# 1.2.5, 1.2.9, 1.6.7: what the source trees the package installs contain.
# qmake installs qml/ and translations/ as directories (sailfishapp's
# `qml.files = qml`), which copies everything in them and keeps each file's
# mode -- so a stray file or an executable bit here reaches the package.
#
shipped_dirs=()
for d in qml translations icons; do
    [[ -d "$root/$d" ]] && shipped_dirs+=("$root/$d")
done
if [[ ${#shipped_dirs[@]} -gt 0 ]]; then
    while IFS= read -r stray; do
        fail 1.2.5 "${stray#"$root"/}" \
            "this kind of file must not be packaged (source control, editor backups, .DS_Store)"
    done < <(find "${shipped_dirs[@]}" \( -name .git -o -name .svn -o -name .hg -o -name .bzr \
        -o -name .cvs -o -name .DS_Store -o -name '*~' -o -name '.*.swp' \) -print)
    while IFS= read -r exe; do
        fail 1.2.9 "${exe#"$root"/}" \
            "an executable file would ship with its mode; only /usr/bin/$name may be executable"
    done < <(find "${shipped_dirs[@]}" -type f -perm /111 -print)
    if command -v file >/dev/null 2>&1; then
        while IFS= read -r f; do
            case "$(file -b "$f" 2>/dev/null)" in
                ELF*) fail 1.6.7 "${f#"$root"/}" \
                    "ELF file in a shipped tree; only /usr/bin/$name may be ELF" ;;
                *) ;;
            esac
        done < <(find "${shipped_dirs[@]}" -type f -print)
    else
        skip 1.6.7 "file(1) not found"
    fi
fi
[[ -f "$desktop" && -x "$desktop" ]] && fail 1.2.9 "$name.desktop" "the .desktop file must not be executable"
note "[1.2.5/1.2.9/1.6.7] shipped trees hold no stray, executable or ELF files"

#
# 1.6 QML imports. Everything under qml/ ships, so everything under qml/ is
# judged; test harnesses and Silica stand-ins live outside it (tests/,
# qml-stubs/), and are not read here.
#
uses_xmllistmodel=0
qml_files=0
declare -A platform_modules=()
if [[ ! -d "$root/qml" ]]; then
    fail 1.6.4 "qml/" "the qml/ tree is missing"
else
    while IFS= read -r d; do
        fail 1.6.8 "${d#"$root"/}" \
            "a test or stub directory inside qml/ would be installed with the app; keep it in tests/ or qml-stubs/"
    done < <(find "$root/qml" -type d \( -name tests -o -name test -o -name 'qml-stubs' -o -name stubs \) -print)

    while IFS= read -r qml; do
        qml_files=$((qml_files + 1))
        relative=${qml#"$root"/}
        while IFS= read -r line; do
            # One line can carry several statements: `import a 1.0; import b 1.0`.
            while IFS= read -r statement; do
                [[ -n "$statement" ]] || continue
                # rpmvalidation.sh's normalisation: drop `as Foo`, collapse
                # whitespace, keep the module and its version.
                import=$(sed -e 's/^[[:space:]]*import/import/' -e 's/[[:space:]]\+/ /g' \
                    -e 's/ as .*$//' -e 's/;$//' <<< "$statement" | cut -f2-3 -d' ')
                [[ -n "$import" ]] || continue
                module=${import%% *}

                [[ $import == QtQuick.XmlListModel* ]] && uses_xmllistmodel=1

                # P.5: the platform modules spec §2 names, and no others.
                # shellcheck disable=SC2053 # a glob, deliberately
                if [[ $module == $POLICY_QML_NAMESPACES ]]; then
                    platform_modules[$module]=1
                    if [[ " $POLICY_QML_MODULES " != *" $module "* ]]; then
                        fail P.5 "$module" \
                            "not one of the platform modules spec §2 allows ($POLICY_QML_MODULES) ($relative)"
                    fi
                fi

                if contained_in "$import" "$rules/allowed_qmlimports.conf"; then
                    continue
                fi
                if contained_in "$import" "$rules/deprecated_qmlimports.conf"; then
                    fail 1.6.4 "$import" "QML import is deprecated and will stop being accepted ($relative)"
                    continue
                fi
                if contained_in "$import" "$rules/dropped_qmlimports.conf"; then
                    fail 1.6.4 "$import" \
                        "QML import was dropped from the platform and is no longer accepted ($relative)"
                    continue
                fi

                case "$import" in
                    [\"\']*)
                        # A path import. Strip the quotes; the version field
                        # cut(1) leaves is not part of a path.
                        path=${import%%[[:space:]]*}
                        path=${path//[\"\']/}
                        case "$path" in
                            /*)
                                fail 1.6.5 "$import" "absolute path imports are forbidden ($relative)"
                                ;;
                            qrc:/*)
                                ;;
                            *)
                                # qml/ installs as /usr/share/<NAME>/qml, so
                                # staying inside it keeps the import inside
                                # the package. A script import names a file;
                                # its directory is what has to be inside.
                                if [[ $path == *.js ]]; then
                                    target=$(cd "$(dirname "$qml")" 2>/dev/null &&
                                        [[ -f "$path" ]] &&
                                        cd "$(dirname "$path")" 2>/dev/null && pwd)
                                else
                                    target=$(cd "$(dirname "$qml")" 2>/dev/null &&
                                        cd "$path" 2>/dev/null && pwd)
                                fi
                                if [[ -z "$target" ]]; then
                                    fail 1.6.6 "$import" \
                                        "relative import does not resolve to a directory ($relative)"
                                elif [[ "${target#"$root/qml"}" = "$target" ]]; then
                                    fail 1.6.6 "$import" \
                                        "relative import resolves to '$target', outside the installed qml/ tree ($relative)"
                                fi
                                ;;
                        esac
                        ;;
                    *)
                        # Everything not explicitly blocked is allowed, which
                        # is what lets the app register its own QML module.
                        if contained_in "$import" "$rules/disallowed_qmlimport_patterns.conf"; then
                            fail 1.6.4 "$import" \
                                "QML import is not allowed at this version; see ci/harbour/allowed_qmlimports.conf ($relative)"
                        fi
                        ;;
                esac
            done < <(tr ';' '\n' <<< "$line")
        done < <(grep -e '^[[:space:]]*import[[:space:]]' "$qml" | sed -e 's/\x0D$//')
    done < <(find "$root/qml" -name '*.qml' | sort)

    if [[ "$qml_files" -eq 0 ]]; then
        fail 1.6.4 "qml/" "no .qml files were found -- did the tree move?"
    else
        note "[1.6.x] imports checked in $qml_files .qml files"
    fi
fi

#
# 1.3 The .desktop file, and 1.4 its [X-Sailjail] section
#
org=""
app=""
if [[ ! -f "$desktop" ]]; then
    fail 1.3.1 "$name.desktop" "the .desktop file is missing"
else
    if grep -qE '^Name=.+' "$desktop"; then
        note "[1.3.1] Name= is present and non-empty"
    else
        fail 1.3.1 "Name" "a non-empty Name= is required"
    fi

    # A C++ app: sailfish-qml is for QML-only packages, which this is not.
    if grep -qE "^Exec=${name}[[:space:]]*$" "$desktop"; then
        note "[1.3.2] Exec=$name"
    else
        fail 1.3.2 "Exec" \
            "must be exactly 'Exec=$name' (a C++ app, never sailfish-qml); found '$(grep -m1 '^Exec=' "$desktop")'"
    fi

    if grep -qE "^Icon=${name}[[:space:]]*$" "$desktop"; then
        note "[1.3.3] Icon=$name"
    else
        fail 1.3.3 "Icon" "must be the bare name 'Icon=$name', with no path and no extension"
    fi

    if grep -qE '^Type=Application[[:space:]]*$' "$desktop"; then
        note "[1.3.4] Type=Application"
    else
        fail 1.3.4 "Type" "must be exactly 'Type=Application'"
    fi

    # A Silica app is booted by the silica-qt5 booster, and nothing else
    # gives it the fast start and the exported-main contract (1.7.3).
    if grep -qE '^X-Nemo-Application-Type=silica-qt5[[:space:]]*$' "$desktop"; then
        note "[1.3.5] X-Nemo-Application-Type=silica-qt5"
    else
        fail 1.3.5 "X-Nemo-Application-Type" "must be silica-qt5: the app is a Silica app"
    fi

    if grep -qE '^\[Sailjail\][[:space:]]*$' "$desktop"; then
        fail 1.3.6 "[Sailjail]" "the section header must be [X-Sailjail]"
    fi

    if ! grep -qE '^\[X-Sailjail\][[:space:]]*$' "$desktop"; then
        fail 1.3.7 "[X-Sailjail]" "the section is missing"
    else
        sailjail=$(sed -n '/^\[X-Sailjail\]/,$p' "$desktop" |
            sed '1d;/^\[/,$d' | grep -vE '^[[:space:]]*(#|$)' || true)
        if [[ -z "$sailjail" ]]; then
            fail 1.3.7 "[X-Sailjail]" "the section must not be empty"
        fi

        permissions=""
        while IFS= read -r line; do
            [[ -n "$line" ]] || continue
            key=${line%%=*}
            value=${line#*=}
            if ! contained_in "$key" "$rules/allowed_sailjailkeys.conf"; then
                fail 1.4.7 "$key" "not an allowed [X-Sailjail] key"
                continue
            fi
            case "$key" in
                OrganizationName)
                    org=$value
                    if [[ ! $value =~ ^[0-9a-z._-]+$ ]]; then
                        fail 1.4.1 "$value" "OrganizationName must match '^[0-9a-z._-]+\$'"
                    fi
                    if [[ $value =~ (^|[.])[0-9] ]]; then
                        fail 1.4.2 "$value" \
                            "no dot-separated component of OrganizationName may start with a digit"
                    fi
                    if contained_in "$value" "$rules/disallowed_orgnames.conf"; then
                        fail 1.4.3 "$value" "OrganizationName is reserved"
                    fi
                    ;;
                ApplicationName)
                    app=$value
                    if [[ ! $value =~ ^[A-Za-z_-][A-Z0-9a-z_-]*$ ]]; then
                        fail 1.4.4 "$value" "ApplicationName must match '^[A-Za-z_-][A-Z0-9a-z_-]*\$'"
                    fi
                    ;;
                Permissions)
                    permissions="$permissions;$value"
                    while IFS= read -r permission; do
                        [[ -n "$permission" ]] || continue
                        if ! contained_in "$permission" "$rules/allowed_permissions.conf"; then
                            fail 1.4.5 "$permission" "permission is not on Harbour's whitelist"
                        elif [[ "$permission" = Compatibility ]]; then
                            fail 1.4.5 "$permission" \
                                "the Compatibility permission exists for pre-sandboxing apps and invites QA scrutiny"
                        fi
                    done < <(tr ';' '\n' <<< "$value")
                    ;;
                ExecDBus)
                    if [[ ! $value =~ ^$name([[:space:]]+[A-Za-z_-][A-Z0-9a-z_-]*)?$ ]]; then
                        fail 1.4.6 "$value" "ExecDBus must be the Exec value, optionally plus one argument"
                    fi
                    ;;
                # Any other key on the allow-list has nothing to check.
                *) ;;
            esac
        done <<< "$sailjail"
        note "[1.4.x] [X-Sailjail] keys, OrganizationName, ApplicationName and Permissions checked"

        # P.2: exactly spec §2's three. Each one missing is a feature that
        # does not work in the sandbox; each one extra is reach nobody
        # reviewed.
        have=$(tr ';' '\n' <<< "$permissions" | grep -v '^[[:space:]]*$' | sort -u | tr '\n' ' ' | sed 's/ $//')
        if [[ "$have" = "$POLICY_PERMISSIONS" ]]; then
            note "[P.2] Permissions are exactly $POLICY_PERMISSIONS"
        else
            for p in $have; do
                [[ " $POLICY_PERMISSIONS " == *" $p "* ]] ||
                    fail P.2 "$p" "spec §2 grants Internet;Bluetooth;Downloads and nothing else"
            done
            for p in $POLICY_PERMISSIONS; do
                [[ " $have " == *" $p "* ]] ||
                    fail P.2 "$p" "spec §2 requires this permission and the .desktop file does not ask for it"
            done
        fi

        # P.4: the sandbox names are fixed (see POLICY_ORG above).
        if [[ "$org" != "$POLICY_ORG" ]]; then
            fail P.4 "OrganizationName=${org:-(none)}" "must be '$POLICY_ORG'; the app's data directory is built from it"
        fi
        if [[ "$app" != "$POLICY_APP" ]]; then
            fail P.4 "ApplicationName=${app:-(none)}" "must be '$POLICY_APP'; the app's data directory is built from it"
        fi
    fi
fi

# 1.8.6/1.8.7/2.7: Requires the validator derives from what the app is
# rather than from what the spec says. After both the QML pass and the
# .desktop file, since it reads a conclusion from each.
if [[ -n "$expanded" ]]; then
    if [[ "$uses_xmllistmodel" = 1 ]] &&
        ! grep -qE '^Requires:[[:space:]]*qt5-qtdeclarative-import-xmllistmodel' <<< "$expanded"; then
        fail 1.8.6 "qt5-qtdeclarative-import-xmllistmodel" \
            "QtQuick.XmlListModel is imported but not required; it is not on devices by default"
    fi
    if grep -qE '^Requires:[[:space:]]*libsailfishapp-launcher' <<< "$expanded"; then
        fail 1.8.7 "libsailfishapp-launcher" \
            "required, but a C++ app does not use the sailfish-qml launcher; drop the dependency"
    fi
    required=$(sed -n 's/^Requires:[[:space:]]*//p' <<< "$expanded" | sed 's/[[:space:]]*$//')
    for module in "${!platform_modules[@]}"; do
        wanted=$(qml_module_package "$module")
        [[ -n "$wanted" ]] || continue
        found=0
        for pkg in $wanted; do
            grep -qxF -- "$pkg" <<< "$required" && found=1
        done
        if [[ "$found" = 0 ]]; then
            fail 2.7 "$module" "imported but the spec does not require its package (${wanted// / or })"
        fi
    done
    note "[2.7] every platform QML module the UI imports is required by package"
fi

if command -v desktop-file-validate >/dev/null 2>&1; then
    note "[1.3.x] desktop entry syntax is ci/packaging-lint.sh's"
fi

#
# 1.5 Icons
#
if ! command -v file >/dev/null 2>&1; then
    skip 1.5.3 "file(1) not found"
else
    for size in $ICON_SIZES; do
        icon="$root/icons/$size/$name.png"
        if [[ ! -f "$icon" ]]; then
            fail 1.5.1 "icons/$size/$name.png" "icon is missing"
            continue
        fi
        described=$(file -b "$icon")
        case "$described" in
            "PNG image data, ${size%x*} x ${size#*x},"*)
                ;;
            PNG*)
                fail 1.5.4 "icons/$size/$name.png" \
                    "pixel dimensions must match the directory name; file(1) reads '$described'"
                ;;
            *)
                fail 1.5.3 "icons/$size/$name.png" "must be a PNG; file(1) reads '$described'"
                ;;
        esac
    done
    note "[1.5.x] icons checked for: $ICON_SIZES"
fi

#
# 1.2.3, 1.5.2, 1.6.1, 1.7.3: the qmake project. The binary itself is the
# RPM validator's to judge; what can be read here is what decides it.
#
if [[ ! -f "$pro" ]]; then
    fail 1.2.3 "$name.pro" "the qmake project is missing, so nothing builds /usr/bin/$name"
else
    # Comments and continuation lines folded away first, so an assignment
    # split over lines is read whole and a commented-out one is not read.
    pro_text=$(sed -e 's/#.*//' "$pro" | sed -e ':a' -e '/\\[[:space:]]*$/{N;s/\\[[:space:]]*\n/ /;ta' -e '}')

    target=$(sed -n 's/^[[:space:]]*TARGET[[:space:]]*=[[:space:]]*\([^[:space:]]*\).*/\1/p' <<< "$pro_text" | tail -1)
    if [[ "$target" = "$name" ]]; then
        note "[1.2.3] the qmake TARGET is '$name'"
    else
        fail 1.2.3 "TARGET=${target:-(unset)}" \
            "$name.pro must build TARGET = $name, the name %files installs to /usr/bin"
    fi

    if grep -qE '^[[:space:]]*CONFIG[[:space:]]*\+=.*\bsailfishapp\b' <<< "$pro_text"; then
        note "[1.7.3] CONFIG += sailfishapp (the booster's link flags)"
    else
        fail 1.7.3 "CONFIG" \
            "$name.pro must use CONFIG += sailfishapp, which links with -rdynamic so Q_DECL_EXPORT main() reaches .dynsym"
    fi

    # 1.5.2: the four sizes are installed by sailfishapp's
    # SAILFISHAPP_ICONS or by hand; either way each size is named.
    for size in $ICON_SIZES; do
        grep -q "$size" <<< "$pro_text" ||
            fail 1.5.2 "icons/$size" "$name.pro does not install the $size icon"
    done

    # 1.6.1, from the source side: every library the project links has to
    # be on the allowed list. The real NEEDED list is the RPM validator's.
    while IFS= read -r lib; do
        [[ -n "$lib" ]] || continue
        if ! grep -qE "^lib${lib//+/\\+}\.so" "$rules/allowed_libraries.conf"; then
            fail 1.6.1 "-l$lib" "$name.pro links lib$lib, which is not on Harbour's allowed list"
        fi
    done < <(grep -E '^[[:space:]]*(LIBS|QMAKE_LFLAGS)[[:space:]]*[+*]?=' <<< "$pro_text" |
        grep -oE '(^|[[:space:]])-l[A-Za-z0-9_+.-]+' | sed 's/^[[:space:]]*-l//' | sort -u)
    # The Qt modules on allowed_libraries.conf, by their qmake names;
    # widgets is the one that most often slips in, and it is not there.
    while IFS= read -r module; do
        [[ -n "$module" ]] || continue
        case "$module" in
            core|gui|qml|quick|dbus|network|concurrent|multimedia|sql|svg|xml|xmlpatterns|sensors|positioning|websockets|location) ;;
            *) fail 1.6.1 "QT += $module" "Qt module '$module' is not on Harbour's allowed list" ;;
        esac
    done < <(grep -E '^[[:space:]]*QT[[:space:]]*\+=' <<< "$pro_text" |
        sed 's/^[[:space:]]*QT[[:space:]]*+=//' | tr -s '[:space:]' '\n' | grep -v '^$')
    # pkg-config names, mapped to the library each links.
    while IFS= read -r pc; do
        [[ -n "$pc" ]] || continue
        case "$pc" in
            dbus-1|sailfishapp|zlib|keepalive|nemonotifications-qt5) ;;
            Qt5Core|Qt5Gui|Qt5Qml|Qt5Quick|Qt5DBus|Qt5Network|Qt5Concurrent|Qt5Multimedia) ;;
            *) fail 1.6.1 "PKGCONFIG += $pc" "'$pc' links a library that is not on Harbour's allowed list" ;;
        esac
    done < <(grep -E '^[[:space:]]*PKGCONFIG[[:space:]]*\+=' <<< "$pro_text" |
        sed 's/^[[:space:]]*PKGCONFIG[[:space:]]*+=//' | tr -s '[:space:]' '\n' | grep -v '^$')
    note "[1.6.1] the libraries $name.pro links are on the allowed list"
fi

# The booster dlopen()s the binary and calls main() through .dynsym, so
# main has to be exported: Q_DECL_EXPORT in the source, sailfishapp's
# -rdynamic at the link (above), and the RPM validator on the result.
if [[ ! -f "$main_cpp" ]]; then
    fail 1.7.3 "src/main.cpp" "the C++ entry point is missing"
elif grep -qE '^[[:space:]]*Q_DECL_EXPORT[[:space:]]+int[[:space:]]+main[[:space:]]*\(' "$main_cpp"; then
    note "[1.7.3] src/main.cpp declares Q_DECL_EXPORT int main"
else
    fail 1.7.3 "src/main.cpp" \
        "main() must be declared 'Q_DECL_EXPORT int main(...)' for the booster to find it"
fi

#
# 2.1 Hardcoded home directories, in everything that is compiled or
# shipped. The validator runs strings(1) over the package; this reads the
# sources that become it, comments aside (code_lines: real comments only,
# never a `#[...]` attribute or a `#define`).
#
# Every other file under src/ and qml/, and under a crate's src/, is read
# whole: qmake ships qml/ as it is, and include_str!, include_bytes! and
# Qt resources put any file there into the binary, where a comment syntax
# this cannot know is no comment at all.
#
sources=()
for d in src qml crates third_party; do
    [[ -d "$root/$d" ]] && sources+=("$root/$d")
done
hits=""
if [[ ${#sources[@]} -gt 0 ]]; then
    mapfile -t compiled < <(find "${sources[@]}" -type f \
        \( -name '*.rs' -o -name '*.cpp' -o -name '*.h' -o -name '*.qml' -o -name '*.js' \) \
        -not -path '*/tests/*' -not -path '*/benches/*' -not -path '*/examples/*' \
        -not -path '*/target/*' -print)
    if [[ ${#compiled[@]} -gt 0 ]]; then
        hits=$(code_lines "${compiled[@]}" | grep -E '/home/(nemo|defaultuser)(/|")' || true)
    fi
    whole=()
    for d in "$root/src" "$root/qml" "$root"/crates/*/src "$root"/third_party/*/src; do
        [[ -d "$d" ]] && whole+=("$d")
    done
    if [[ ${#whole[@]} -gt 0 ]]; then
        hits+=$'\n'$(find "${whole[@]}" -type f \
            ! \( -name '*.rs' -o -name '*.cpp' -o -name '*.h' -o -name '*.qml' -o -name '*.js' \) \
            -not -path '*/tests/*' -not -path '*/target/*' -exec grep -nH -E '/home/(nemo|defaultuser)' {} + || true)
    fi
fi
# One hit per line: a substitution drops its trailing newline, so each
# list is joined on one of its own.
[[ -f "$desktop" ]] && hits+=$'\n'$(grep -nH -E '/home/(nemo|defaultuser)' "$desktop" || true)
hits=$(grep -v '^[[:space:]]*$' <<< "$hits" || true)
if [[ -n "$hits" ]]; then
    while IFS= read -r hit; do
        [[ -n "$hit" ]] || continue
        file_part=${hit%%:*}
        fail 2.1 "${file_part#"$root"/}" "hardcoded home directory: ${hit#*:}"
    done <<< "$hits"
else
    note "[2.1] no hardcoded /home/nemo or /home/defaultuser"
fi

#
# 2.5 The sandbox grants write access to $XDG_DATA_HOME/<Org>/<App>, so the
# path the app builds has to be spelled the same way. The shell can spell
# it out, or set the names, or rely on libsailfishapp setting them from
# this very section and ask QStandardPaths for AppDataLocation.
#
if [[ -n "$org" && -n "$app" && -d "$root/src" ]]; then
    if grep -rqF -- "$org/$app" "$root/src" "$root/qml" 2>/dev/null ||
       { grep -rqE "setOrganizationName\([^)]*\"$org\"" "$root/src" &&
         grep -rqE "setApplicationName\([^)]*\"$app\"" "$root/src"; } ||
       grep -rqE 'QStandardPaths::App(Local)?DataLocation' "$root/src"; then
        note "[2.5] the app's data path follows OrganizationName/ApplicationName ($org/$app)"
    else
        fail 2.5 "$org/$app" \
            "nothing in src/ builds the data path under '$org/$app', so the sandbox grant and the app disagree"
    fi
elif [[ -n "$org" && -n "$app" ]]; then
    fail 2.5 "src/" "the C++ shell is missing, so the data path cannot be checked against '$org/$app'"
fi

#
# 2.6 Nothing the package installs is written at runtime. Narrow -- a line
# that both names an installed path and calls a writing API -- but exact;
# the broad version of the rule is clippy.toml's S3 ban on the Rust side.
#
writes='create_dir_all|create_dir|File::create|fs::write|fs::copy|fs::rename|remove_file|remove_dir|OpenOptions|QFile|QSaveFile|QDir|mkpath|mkdir|WriteOnly|ReadWrite|fopen'
if [[ ${#sources[@]} -gt 0 ]] &&
    hits=$(grep -rnE "\"(/usr/(share|bin)/$name)" --include='*.rs' --include='*.cpp' --include='*.h' \
        --include='*.qml' --include='*.js' --exclude-dir=target "${sources[@]}" | grep -E "$writes"); then
    while IFS= read -r hit; do
        file_part=${hit%%:*}
        fail 2.6 "${file_part#"$root"/}" \
            "writes to a path the package installs, which the package manager owns: ${hit#*:}"
    done <<< "$hits"
else
    note "[2.6] nothing writes to an installed path"
fi

#
# Stale waivers. A waiver that no longer matches anything is a rule that
# was fixed and a licence that outlived it. Only this check's own: an `rpm`
# waiver names a finding only the built package can have, and
# ci/harbour-validate-rpm.sh holds it to the same rule.
#
for i in "${!waiver_line[@]}"; do
    if [[ -z "${waiver_used[${waiver_line[$i]}]:-}" ]]; then
        echo "harbour-check: FAIL stale waiver at ci/harbour/waivers.conf:${waiver_line[$i]}" \
             "('source ${waiver_id[$i]} ${waiver_subject[$i]} ${waiver_message[$i]}') matches nothing; delete it" >&2
        status=1
    fi
done

echo
if [[ "$status" -eq 0 ]]; then
    if [[ "$skipped" -gt 0 ]]; then
        note "ok, with $skipped check(s) skipped for want of a tool"
    else
        note "ok"
    fi
else
    if [[ "$findings" -gt 0 ]]; then
        note "FAILED: $findings finding(s)" >&2
    else
        note "FAILED" >&2
    fi
    note "the authority is 'sfdk check -s harbour' on a built RPM; see docs/HARBOUR.md" >&2
fi
exit "$status"
