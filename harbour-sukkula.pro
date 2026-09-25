# SPDX-License-Identifier: GPL-3.0-or-later
#
# The Qt/QML shell of Sukkula, built by the Sailfish SDK (spec §3).
#
# Everything but the start-up shim and the bridge is the Rust engine, a
# private shared library cross-built beforehand (scripts/cross-build-rust.sh)
# and handed in as SUKKULA_RUST_LIB:
#
#   %qmake5 SUKKULA_RUST_LIB=/path/to/libsukkula_ffi.so
#   make
#   make install INSTALL_ROOT=%{buildroot}
#
# Installed layout (Harbour 1.2): /usr/bin/harbour-sukkula,
# /usr/share/harbour-sukkula/{qml,translations,lib}, the desktop file in
# /usr/share/applications and the four icon sizes under
# /usr/share/icons/hicolor. Nothing else.
#
# The host tests build the same sources without libsailfishapp: see
# tests/cpp/ and tests/README.md.

TARGET = harbour-sukkula

# libsailfishapp's feature: links it, installs the binary to /usr/bin, the
# qml/ directory to /usr/share/$$TARGET, the desktop file and the icons
# named by SAILFISHAPP_ICONS (icons/<size>/$$TARGET.png).
CONFIG += sailfishapp
SAILFISHAPP_ICONS = 86x86 108x108 128x128 172x172

CONFIG += c++11

HEADERS += src/bridge.h src/tls_reserve.h
# src/tls_reserve.c: its array has to be the executable's only
# thread-local, at tp+16, where the phone's graphics stack keeps its own.
SOURCES += src/tls_reserve.c src/main.cpp src/bridge.cpp

INCLUDEPATH += $$PWD/crates/sukkula-ffi/include
DEPENDPATH += $$PWD/crates/sukkula-ffi/include

# The Rust engine: libsukkula_ffi.so, cross-built for aarch64 before qmake
# runs (the SDK's own Rust is too old, spec §3), and installed where
# Harbour lets an app keep its own libraries. The binary finds it through
# the RPATH the SDK's sailfishapp feature sets, /usr/share/$$TARGET/lib,
# which src/hardening.pri writes as DT_RPATH. A library and not a static
# archive linked in here: the silica-qt5 booster dlopen()s this binary,
# and the engine's thread-locals only work dlopen()ed from a library of
# their own (docs/FFI.md, Linking). What the library needs from the system
# -- libdbus-1, glibc -- it records itself.
isEmpty(SUKKULA_RUST_LIB): SUKKULA_RUST_LIB = $$PWD/target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.so
!exists($$SUKKULA_RUST_LIB): warning("SUKKULA_RUST_LIB does not exist yet: $$SUKKULA_RUST_LIB")
# A changed library relinks the binary; a missing one fails the build
# with make's "No rule to make target", which names the path.
PRE_TARGETDEPS += $$SUKKULA_RUST_LIB
LIBS += $$SUKKULA_RUST_LIB
engine.files = $$SUKKULA_RUST_LIB
engine.path = /usr/share/$${TARGET}/lib
# Installed even when qmake runs before the library exists: a package
# without it fails the validator ("Cannot link to shared library") rather
# than shipping a binary that cannot start.
engine.CONFIG += no_check_exist
INSTALLS += engine

# Stack protector, fortify, PIE, full RELRO, --as-needed, main() as the
# only export, the RPATH as DT_RPATH, and stripping at link: see
# src/hardening.pri, which the host build in tests/cpp/host_app shares.
include(src/hardening.pri)

# Translations (spec M5): compiled from translations/*.ts by lrelease here
# rather than by sailfishapp_i18n, which would also run lupdate over the
# sources and rewrite the catalogues in the build tree.
# harbour-sukkula.ts is the English source; its .qm carries the English
# plural forms and is the fallback for every other language.
TRANSLATIONS += \
    translations/harbour-sukkula.ts \
    translations/harbour-sukkula-de.ts \
    translations/harbour-sukkula-fi.ts \
    translations/harbour-sukkula-sv.ts

# lrelease from qt5-qttools-linguist (a BuildRequires); LRELEASE=...
# on the qmake line overrides.
isEmpty(LRELEASE) {
    LRELEASE = $$[QT_INSTALL_BINS]/lrelease
    !exists($$LRELEASE): LRELEASE = $$[QT_HOST_BINS]/lrelease
    !exists($$LRELEASE): LRELEASE = lrelease
}
sukkula_qm.input = TRANSLATIONS
sukkula_qm.output = $$OUT_PWD/${QMAKE_FILE_BASE}.qm
sukkula_qm.commands = $$LRELEASE -silent -nounfinished ${QMAKE_FILE_IN} -qm ${QMAKE_FILE_OUT}
sukkula_qm.CONFIG += no_link target_predeps
QMAKE_EXTRA_COMPILERS += sukkula_qm

for(ts, TRANSLATIONS) {
    qm_files += $$OUT_PWD/$$replace($$list($$basename(ts)), \\.ts$, .qm)
}
translations_install.files = $$qm_files
translations_install.path = /usr/share/$${TARGET}/translations
translations_install.CONFIG += no_check_exist
INSTALLS += translations_install

DISTFILES += \
    harbour-sukkula.desktop \
    $$files(qml/*.qml, true) \
    $$files(qml/*.js, true) \
    icons/harbour-sukkula.svg \
    $$TRANSLATIONS
