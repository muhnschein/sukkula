#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Runs QML tests without the lint and static stages, for quick iteration:
#   tests/run-one-qml-test.sh tst_main.qml [tst_send.qml ...]
# tests/run-qml-tests.sh runs everything; this is only a shortcut.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SUKKULA_QML_TESTS_ONLY=1 exec "$root/tests/run-qml-tests.sh" "$@"
