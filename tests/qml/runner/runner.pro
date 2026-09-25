# SPDX-License-Identifier: GPL-3.0-or-later
# The QML test runner: see runner.cpp and tests/README.md.
TEMPLATE = app
TARGET = qml_runner
QT = core gui qml quick
CONFIG += c++11 warn_on
CONFIG -= app_bundle
QMAKE_CXXFLAGS += -Wall -Wextra -Werror
SOURCES += runner.cpp
