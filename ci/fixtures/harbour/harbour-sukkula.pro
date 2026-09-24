# Fixture: the shape of the real harbour-sukkula.pro, for the selftest.
TARGET = harbour-sukkula

CONFIG += sailfishapp sailfishapp_i18n
QT += dbus

isEmpty(SUKKULA_RUST_LIB): SUKKULA_RUST_LIB = $$PWD/target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.a
INCLUDEPATH += $$PWD/crates/sukkula-ffi/include
LIBS += $$SUKKULA_RUST_LIB \
    -ldbus-1 -lpthread -ldl -lm

SOURCES += src/main.cpp

DISTFILES += qml/harbour-sukkula.qml \
    qml/cover/CoverPage.qml \
    qml/pages/*.qml \
    harbour-sukkula.desktop

SAILFISHAPP_ICONS = 86x86 108x108 128x128 172x172

TRANSLATIONS += translations/harbour-sukkula-fi.ts
