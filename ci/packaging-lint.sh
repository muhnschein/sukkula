#!/bin/sh
# Static checks on what decides whether the RPM builds, installs and
# launches -- everything checkable without the SDK (docs/BUILDING.md). The
# Harbour rules themselves are ci/harbour-check.sh's.
#
# PACKAGING_LINT_STRICT=1 (CI) turns "the tool for this check is missing"
# into a failure, so the job cannot quietly stop checking when the apt line
# that installs the tools changes. A missing *input* -- the spec, the
# .desktop file -- is a failure either way.
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
spec="$root/rpm/harbour-sukkula.spec"
desktop="$root/harbour-sukkula.desktop"
pro="$root/harbour-sukkula.pro"
status=0
ran=0

strict=${PACKAGING_LINT_STRICT:-0}
skip() {
    # POSIX sh has no `local`; nothing else in the file uses these names.
    tool=$1
    hint=$2
    if [ "$strict" = 1 ]; then
        echo "packaging-lint: FAIL $tool is not installed ($hint) (strict mode)" >&2
        status=1
    else
        echo "packaging-lint: SKIP $tool ($hint)"
    fi
    return 0
}
fail() { echo "packaging-lint: FAIL $*" >&2; status=1; return 0; }

[ -f "$spec" ] || { echo "packaging-lint: FAIL rpm/harbour-sukkula.spec is missing" >&2; exit 1; }

if command -v rpmspec >/dev/null 2>&1; then
    ran=$((ran + 1))
    # -P expands and parses only; BuildRequires are the SDK's job.
    if rpmspec -P --target aarch64 "$spec" >/dev/null; then
        echo "packaging-lint: rpm/harbour-sukkula.spec parses"
    else
        fail "rpm/harbour-sukkula.spec does not parse"
    fi
else
    skip rpmspec "install rpm"
fi

# rpm expands macros inside comments, and the SDK's rpm still does even
# where a newer host rpm has stopped: a comment naming the build section
# expands to the whole build preamble, whose first line rpm then reads as a
# tag ("error: Unknown tag: LANG=C"). A host rpmspec parses the same file
# happily, so only a direct check catches it (piirit found it the hard way).
ran=$((ran + 1))
bare=$(awk '/^[[:space:]]*#/ {
        stripped = $0
        gsub(/%%/, "", stripped)
        if (stripped ~ /%/) printf "%s:%d: %s\n", FILENAME, FNR, $0
    }' "$spec")
if [ -z "$bare" ]; then
    echo "packaging-lint: spec comments escape their macros"
else
    echo "$bare" >&2
    fail "a spec comment has an unescaped % (write %%)"
fi

# qmake writes a Makefile where it runs, and the source root already has
# one: the developer targets. The spec builds in build-sfos/, and both of
# its sections that run qmake's output have to be there.
ran=$((ran + 1))
if awk '
    /^%build/ { sec = "build" } /^%install/ { sec = "install" } /^%files/ { sec = "" }
    sec != "" && /^cd build-sfos[[:space:]]*$/ { cd[sec] = 1 }
    sec == "build" && /%qmake5 / { if (!cd["build"]) bad = 1; qmake = 1 }
    sec == "install" && /%qmake5_install/ { if (!cd["install"]) bad = 1; inst = 1 }
    END { exit (bad || !qmake || !inst) }' "$spec"; then
    echo "packaging-lint: the spec builds out of tree (build-sfos/), leaving the Makefile alone"
else
    fail "the spec must run %qmake5 and %qmake5_install inside build-sfos/, or qmake overwrites the developer Makefile"
fi

if [ ! -f "$desktop" ]; then
    fail "harbour-sukkula.desktop is missing"
elif command -v desktop-file-validate >/dev/null 2>&1; then
    ran=$((ran + 1))
    # Sailfish's own keys are not in the freedesktop spec; each expected
    # message is named, and anything else still fails.
    out=$(desktop-file-validate "$desktop" 2>&1 |
        grep -v 'value "silica-qt5" for key "X-Nemo-Application-Type"' |
        grep -v 'key "X-Nemo-Application-Type" .* is not known' || true)
    if [ -z "$out" ]; then
        echo "packaging-lint: harbour-sukkula.desktop valid"
    else
        echo "$out" >&2
        fail "harbour-sukkula.desktop"
    fi
else
    skip desktop-file-validate "install desktop-file-utils"
fi

# Every build has to be a distinguishable package: the spec pins Version
# and Release, and mb2 runs with -X so nothing derives them from git, so
# without the workflow's stamp every build is the same NEVRA and `rpm -U`
# on a phone refuses it as already installed.
ran=$((ran + 1))
workflow="$root/.github/workflows/rpm.yml"
if [ ! -f "$workflow" ]; then
    fail ".github/workflows/rpm.yml is missing"
elif ! grep -q 'sed -i "s/\^Release:' "$workflow"; then
    fail "the rpm workflow no longer stamps Release, so every build would be the same NEVRA"
else
    echo "packaging-lint: the rpm workflow stamps a unique Release"
fi

# The workflow rules docs/BUILDING.md states, each once broken (the review
# findings named), each a line a later edit could quietly undo:
#
#   - the SDK image is never pulled by its tag, which anyone who can push
#     to the registry can repoint, and nothing that runs a pull request's
#     code or a crate's build script can push one: rpm.yml holds no
#     `packages: write`, the one workflow that does runs from main only,
#     and scripts/build-rpm.sh pulls by digest ("Any branch or dependency
#     build script with packages:write can overwrite the SDK image");
#   - a `v*` tag releases only a commit main contains ("A 'v*' tag on any
#     commit publishes a GitHub release");
#   - a pull request's runs are grouped by its number, never by the bare
#     head_ref two forks can share ("Concurrency groups keyed on the bare
#     head_ref").
ran=$((ran + 1))
wf="$root/.github/workflows"
policy_ok=1
if grep -n 'group:.*head_ref' "$wf"/*.yml >&2; then
    fail "a concurrency group keyed on head_ref: two forks' same-named branches cancel each other; key on github.event.pull_request.number"
    policy_ok=0
fi
for f in "$wf"/*.yml; do
    grep -q 'packages:[[:space:]]*write' "$f" || continue
    case "$(basename "$f")" in
        sdk-image.yml)
            grep -q "^    if: github.ref == 'refs/heads/main'\$" "$f" || {
                fail "sdk-image.yml holds packages: write but does not refuse to run from a branch other than main"
                policy_ok=0; }
            ;;
        *)
            fail "$(basename "$f") holds packages: write; only sdk-image.yml may publish the SDK image"
            policy_ok=0
            ;;
    esac
done
if grep -n '^[^#]*docker pull' "$wf/rpm.yml" >&2; then
    fail "rpm.yml pulls an image itself; the SDK image comes from scripts/build-rpm.sh pull, by digest"
    policy_ok=0
fi
# shellcheck disable=SC2016 # the pattern is the script's literal text
if grep -n '^[^#]*docker pull' "$root/scripts/build-rpm.sh" | grep -v '@\$digest' >&2; then
    fail "scripts/build-rpm.sh pulls an image other than by its pinned digest"
    policy_ok=0
fi
# shellcheck disable=SC2016 # the pattern is the workflow's literal text
if ! grep -q 'merge-base --is-ancestor "\$GITHUB_SHA"' "$wf/rpm.yml"; then
    fail "rpm.yml no longer proves a v* tag's commit is on main before releasing it"
    policy_ok=0
fi
[ "$policy_ok" = 1 ] &&
    echo "packaging-lint: workflows pull the SDK image by digest, release from main, group by pull request"

# mb2 derives the package from the directory it runs in and then looks for
# rpm/<that>.spec; scripts/build-rpm.sh mounts the tree at a path it
# chooses, so that name and the spec's have to agree.
ran=$((ran + 1))
spec_base=$(basename "$spec" .spec)
# shellcheck disable=SC2016 # the pattern is the script's literal text
if grep -q "^NAME=$spec_base\$" "$root/scripts/build-rpm.sh" 2>/dev/null &&
    grep -q 'builddir="$home/$NAME"' "$root/scripts/build-rpm.sh"; then
    echo "packaging-lint: mb2 builds in a directory named for the spec"
else
    fail "scripts/build-rpm.sh must mount the tree as \$home/$spec_base, or mb2 will not find the spec"
fi

# The catalogs the project names exist, and compile cleanly: lrelease is
# what the RPM runs on them, on the SDK, minutes into a build nobody
# watches, and every warning is a string that reaches the phone untranslated.
if [ -f "$pro" ]; then
    catalogs=$(sed -e 's/#.*//' -e 's/[\\]/ /g' "$pro" | grep -oE 'translations/[A-Za-z0-9_.-]+\.ts' | sort -u || true)
    for c in $catalogs; do
        [ -f "$root/$c" ] || fail "$c is named in harbour-sukkula.pro but missing"
    done
    lrelease=""
    for candidate in /usr/lib/qt5/bin/lrelease lrelease-qt5 lrelease; do
        if command -v "$candidate" >/dev/null 2>&1; then lrelease=$candidate; break; fi
    done
    if [ -n "$catalogs" ] && [ -n "$lrelease" ]; then
        ran=$((ran + 1))
        qm_dir=$(mktemp -d)
        for c in $catalogs; do
            [ -f "$root/$c" ] || continue
            if out=$("$lrelease" "$root/$c" -qm "$qm_dir/x.qm" 2>&1) && ! echo "$out" | grep -qi 'warning\|error'; then
                :
            else
                echo "$out" >&2
                fail "$c does not compile cleanly with lrelease"
            fi
        done
        rm -rf "$qm_dir"
        echo "packaging-lint: the translation catalogs compile cleanly"
    elif [ -n "$catalogs" ]; then
        skip lrelease "install qttools5-dev-tools"
    fi
else
    fail "harbour-sukkula.pro is missing"
fi

# Every docs/<name>.md a comment, a script or a document points at has to
# exist; a reference to a file that is not there sends a reader nowhere.
# Not docs/handoff/: the agents' working notes, which cite the sibling
# projects' own docs/ by the same relative names.
ran=$((ran + 1))
missing=$(grep -rhoE 'docs/[A-Za-z0-9_-]+\.md' "$root" \
        --include='*.rs' --include='*.qml' --include='*.js' --include='*.sh' \
        --include='*.yml' --include='*.toml' --include='*.md' --include='*.spec' \
        --include='*.conf' --include='*.cpp' --include='*.h' --include='*.pro' \
        --include='Makefile' --include='.gitignore' \
        --exclude-dir=.git --exclude-dir=target --exclude-dir=vendor \
        --exclude-dir=third_party --exclude-dir=fixtures --exclude-dir=handoff |
    sort -u | while read -r ref; do
        [ -f "$root/$ref" ] || echo "  $ref"
    done)
if [ -z "$missing" ]; then
    echo "packaging-lint: every docs/*.md referenced exists"
else
    echo "$missing" >&2
    fail "these documents are referenced but do not exist; write, repoint or drop them"
fi

# Every script the workflows and the Makefile run is run as ./path, so it
# has to be executable: a mode lost in a commit fails CI with "Permission
# denied" on a line that looks fine.
ran=$((ran + 1))
not_exec=$(cd "$root" && find ci scripts tests -name '*.sh' -type f ! -perm -u+x -print 2>/dev/null | sort)
if [ -z "$not_exec" ]; then
    echo "packaging-lint: every script is executable"
else
    echo "$not_exec" >&2
    fail "these scripts are not executable (chmod +x, and commit the mode)"
fi

# Every shell script in the tree, through shellcheck: ours, the UI's QML test
# runner, the FFI harness -- anything with a .sh name or a shell shebang.
if command -v shellcheck >/dev/null 2>&1; then
    ran=$((ran + 1))
    scripts=$(cd "$root" && find . \( -path ./target -o -path ./.git -o -path ./vendor \
            -o -path ./third_party -o -path ./fuzz/target -o -path ./build-sfos \) -prune -o \
            -type f \( -name '*.sh' -o -perm /111 \) -print |
        while read -r f; do
            case "$f" in
                *.sh) echo "$f" ;;
                *) head -1 "$f" 2>/dev/null | grep -qE '^#!.*\b(ba|da|k)?sh\b' && echo "$f" ;;
            esac
        done | sort)
    # shellcheck disable=SC2086 # one path per word; none has a space
    if (cd "$root" && shellcheck -x $scripts); then
        echo "packaging-lint: shell scripts clean ($(echo "$scripts" | wc -w))"
    else
        fail "shellcheck"
    fi
else
    skip shellcheck "install shellcheck"
fi

# The workflows, by actionlint, which also runs shellcheck over every
# `run:` block.
if command -v actionlint >/dev/null 2>&1; then
    ran=$((ran + 1))
    if (cd "$root" && actionlint); then
        echo "packaging-lint: workflows clean"
    else
        fail "actionlint"
    fi
else
    skip actionlint "pip install actionlint-py"
fi

if [ "$ran" -eq 0 ]; then
    echo "packaging-lint: FAIL (no checker was available; this job proves nothing)" >&2
    exit 1
fi
exit "$status"
