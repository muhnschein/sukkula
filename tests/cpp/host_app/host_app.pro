# SPDX-License-Identifier: GPL-3.0-or-later
#
# src/main.cpp and src/bridge.cpp as the phone builds them -- same sources,
# same hardening (src/hardening.pri) -- against a desktop Qt, a host
# stand-in for libsailfishapp and the stub or the real engine. Runs the
# app's own qml/ with the stubs in qml-stubs/.
TEMPLATE = app
TARGET = harbour-sukkula-host
QT = core gui qml quick
include(../common.pri)
include(../../../src/hardening.pri)
# What the SDK's sailfishapp feature adds (sailfishapp.prf, for apps that
# ship private libraries); hardening.pri has to keep it out of the binary.
QMAKE_RPATHDIR += /usr/share/$$TARGET/lib

# Our <sailfishapp.h>, ahead of anything else of that name.
INCLUDEPATH = $$PWD/../sailfishapp $$INCLUDEPATH
DEFINES += SUKKULA_SOURCE_ROOT=\\\"$$SUKKULA_ROOT\\\"

HEADERS += $$SUKKULA_ROOT/src/bridge.h $$SUKKULA_ROOT/src/tls_reserve.h \
    $$PWD/../sailfishapp/sailfishapp.h
SOURCES += \
    $$SUKKULA_ROOT/src/tls_reserve.c \
    $$SUKKULA_ROOT/src/main.cpp \
    $$SUKKULA_ROOT/src/bridge.cpp \
    $$PWD/../sailfishapp/sailfishapp_host.cpp
