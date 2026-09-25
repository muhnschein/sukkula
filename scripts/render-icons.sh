#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Renders icons/harbour-sukkula.svg to the four sizes Harbour requires
# (1.5), at icons/<size>/harbour-sukkula.png, which harbour-sukkula.pro
# installs under /usr/share/icons/hicolor/<size>/apps. The PNGs are
# committed; run scripts/render-icons.sh after changing the SVG. It lives
# here rather than in icons/ because nothing under icons/ may be executable
# (Harbour 1.2.9: qmake's install keeps modes). Needs rsvg-convert
# (librsvg2-bin).
set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")/../icons" && pwd)
for size in 86 108 128 172; do
    mkdir -p "$here/${size}x${size}"
    rsvg-convert --width "$size" --height "$size" --keep-aspect-ratio \
        "$here/harbour-sukkula.svg" -o "$here/${size}x${size}/harbour-sukkula.png"
done
