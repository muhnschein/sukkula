# SPDX-License-Identifier: GPL-3.0-or-later
# The bridge (src/bridge.cpp) against the stub or the real engine.
TEMPLATE = app
TARGET = tst_bridge
QT = core testlib
include(../common.pri)

HEADERS += $$SUKKULA_ROOT/src/bridge.h $$SUKKULA_ROOT/src/tls_reserve.h
SOURCES += $$SUKKULA_ROOT/src/tls_reserve.c $$SUKKULA_ROOT/src/bridge.cpp tst_bridge.cpp
