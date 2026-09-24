#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Refreshes the catalogues from the QML sources, keeping every existing
# translation. Run `sh translations/update.sh` after adding, changing or
# removing a qsTr() string, then translate what lupdate marks unfinished;
# tests/qml/static_checks.py fails while any catalogue is out of step with
# the sources. Not executable: nothing under translations/ may be (Harbour
# 1.2.9).
#
# harbour-sukkula.ts is the English source catalogue: only its plural forms
# need filling in (its .qm is the fallback that carries them); singulars
# come from the source text.
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
lupdate=${LUPDATE:-lupdate}

"$lupdate" -silent -noobsolete -locations none -extensions qml,js \
    -source-language en -target-language en \
    "$root/qml" -ts "$root/translations/harbour-sukkula.ts"
for lang in de "fi" sv; do
    "$lupdate" -silent -noobsolete -locations none -extensions qml,js \
        -source-language en -target-language "$lang" \
        "$root/qml" -ts "$root/translations/harbour-sukkula-$lang.ts"
done
