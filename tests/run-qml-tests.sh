#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The QML side's host tests, offline, on a desktop Qt 5.15:
#
#   1. qmllint on every .qml file (app, stubs, tests);
#   2. tests/qml/static_checks.py: plain text everywhere (S2), no URL
#      opening (S8), Harbour's QML imports, Qt 5.6 JavaScript, the Share
#      menu wiring, translations complete;
#   3. every tests/qml/tst_*.qml in the runner (tests/qml/runner), headless;
#   4. tests/qml/selftest.py: planted faults, each of which must be caught.
#
# Needs: qtdeclarative5-dev, qtdeclarative5-dev-tools (qmllint),
# qml-module-qtquick2, qttools5-dev-tools (lupdate, lrelease), python3,
# make, g++.
#
# Usage: tests/run-qml-tests.sh [tst_name.qml ...]
# Env:   BUILD_DIR (default build/host-tests), QMAKE (default qmake).
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
build=${BUILD_DIR:-$root/build/host-tests}
qmake=${QMAKE:-qmake}
status=0

say() { printf '%s\n' "$*"; }

lrelease=${LRELEASE:-lrelease}
for tool in qmllint "$qmake" make python3 "$lrelease"; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        say "run-qml-tests: FAIL ($tool not found)" >&2
        exit 1
    fi
done

if [ -z "${SUKKULA_QML_TESTS_ONLY:-}" ]; then
    # 1. Syntax.
    count=0
    lint_failed=0
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        count=$((count + 1))
        if ! qmllint "$file"; then
            lint_failed=1
        fi
    done <<EOF
$(find "$root/qml" "$root/qml-stubs" "$root/tests/qml" -name '*.qml' | sort)
EOF
    if [ "$lint_failed" -ne 0 ]; then
        say "qmllint: FAIL"
        status=1
    else
        say "qmllint: ok ($count files)"
    fi

    # 2. Static rules.
    if ! python3 "$root/tests/qml/static_checks.py" "$root"; then
        status=1
    fi
fi

# 3. The runner.
mkdir -p "$build/qml-runner"
(
    cd "$build/qml-runner"
    "$qmake" "$root/tests/qml/runner/runner.pro" >/dev/null
    make -s >/dev/null
)
runner=$build/qml-runner/qml_runner
# The English catalogue, for its plural forms, as the phone loads it.
"$lrelease" -silent -nounfinished "$root/translations/harbour-sukkula.ts" \
    -qm "$build/qml-runner/harbour-sukkula.qm"

runtime=$(mktemp -d)
trap 'rm -rf "$runtime"' EXIT
chmod 700 "$runtime"
export XDG_RUNTIME_DIR="$runtime"
export QT_QPA_PLATFORM=offscreen
export QT_QUICK_BACKEND=software
# The host's own QML modules must not stand in for the stubs.
unset QML2_IMPORT_PATH QML_IMPORT_PATH

if [ "$#" -gt 0 ]; then
    tests=$(for t in "$@"; do printf '%s\n' "$root/tests/qml/$(basename "$t")"; done)
else
    tests=$(find "$root/tests/qml" -maxdepth 1 -name 'tst_*.qml' | sort)
fi
passed=0
failed=0
while IFS= read -r test; do
    [ -n "$test" ] || continue
    if "$runner" --app "$root/qml" --stubs "$root/qml-stubs" \
        --qm "$build/qml-runner/harbour-sukkula.qm" "$test"; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
    fi
done <<EOF
$tests
EOF
if [ "$failed" -ne 0 ] || [ "$passed" -eq 0 ]; then
    say "qml tests: FAIL ($passed passed, $failed failed)"
    status=1
else
    say "qml tests: ok ($passed passed)"
fi

# 4. The checks themselves: each planted fault must be caught.
if [ -z "${SUKKULA_QML_TESTS_ONLY:-}" ] && [ "$#" -eq 0 ] &&
    ! python3 "$root/tests/qml/selftest.py" "$root" "$runner" "$build/qml-runner/harbour-sukkula.qm"; then
    status=1
fi

exit "$status"
