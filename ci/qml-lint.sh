#!/bin/sh
# Parse every .qml and .js file the package ships (qml/), and the host-side
# Silica stand-ins the QML tests load (qml-stubs/, when present).
#
# Syntax only: qmllint cannot resolve Sailfish.Silica, so whether a page
# actually loads is tests/run-qml-tests.sh's question, against the stubs.
# A typo in a shipped file fails on the phone and nowhere else, which is
# why this runs on every pull request (ci.yml's `qml` job).
#
# Missing inputs fail: no qmllint, or no .qml files, is a job that proves
# nothing.
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

qmllint=""
for candidate in qmllint qmllint-qt5 /usr/lib/qt5/bin/qmllint /usr/lib/x86_64-linux-gnu/qt5/bin/qmllint; do
    if command -v "$candidate" >/dev/null 2>&1; then
        qmllint=$candidate
        break
    fi
done
if [ -z "$qmllint" ]; then
    echo "qml-lint: FAIL (qmllint not found; install qtdeclarative5-dev-tools)" >&2
    exit 1
fi

if [ ! -d "$root/qml" ]; then
    echo "qml-lint: FAIL (no qml/ tree -- did it move?)" >&2
    exit 1
fi

dirs="$root/qml"
[ -d "$root/qml-stubs" ] && dirs="$dirs $root/qml-stubs"

count=0
status=0
# Read rather than word-split: a path with a space in it would otherwise
# arrive as two arguments and neither would exist.
# shellcheck disable=SC2086 # $dirs is a list of directories on purpose
while IFS= read -r file; do
    [ -n "$file" ] || continue
    count=$((count + 1))
    if ! "$qmllint" "$file"; then
        status=1
    fi
done <<EOF
$(find $dirs \( -name '*.qml' -o -name '*.js' \) -type f | sort)
EOF

if [ "$count" -eq 0 ]; then
    echo "qml-lint: FAIL (no .qml files found -- did the tree move?)" >&2
    exit 1
fi

[ "$status" -eq 0 ] && echo "qml-lint: ok ($count files)"
exit "$status"
