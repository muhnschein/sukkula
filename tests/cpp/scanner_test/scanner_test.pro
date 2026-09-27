# SPDX-License-Identifier: GPL-3.0-or-later
#
# src/scanner.cpp's host tests (tst_scanner.cpp), against the stub engine
# or the real one: see tests/run-cpp-tests.sh.

TEMPLATE = app
TARGET = tst_scanner
QT = core gui qml quick testlib

include(../common.pri)

HEADERS += $$SUKKULA_ROOT/src/scanner.h
SOURCES += $$SUKKULA_ROOT/src/scanner.cpp tst_scanner.cpp
