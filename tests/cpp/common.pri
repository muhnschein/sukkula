# SPDX-License-Identifier: GPL-3.0-or-later
#
# Shared by the host test builds: the repository's own sources, strict
# warnings, and which engine to link.
#
#   SUKKULA_ENGINE=stub  (default) tests/cpp/stub/sukkula_stub.c
#   SUKKULA_ENGINE=rust  the real static library, SUKKULA_RUST_LIB,
#                        default target/debug/libsukkula_ffi.a

SUKKULA_ROOT = $$clean_path($$PWD/../..)

CONFIG += c++11 warn_on
CONFIG -= app_bundle
QMAKE_CXXFLAGS += -Wall -Wextra -Werror
QMAKE_CFLAGS += -Wall -Wextra -Werror -std=c11 -D_POSIX_C_SOURCE=200809L

INCLUDEPATH += $$SUKKULA_ROOT/src $$SUKKULA_ROOT/crates/sukkula-ffi/include

isEmpty(SUKKULA_ENGINE): SUKKULA_ENGINE = stub
equals(SUKKULA_ENGINE, rust) {
    isEmpty(SUKKULA_RUST_LIB): SUKKULA_RUST_LIB = $$SUKKULA_ROOT/target/debug/libsukkula_ffi.a
    PRE_TARGETDEPS += $$SUKKULA_RUST_LIB
    LIBS += $$SUKKULA_RUST_LIB -ldbus-1 -lpthread -ldl -lm -lrt
    DEFINES += SUKKULA_RUST_ENGINE
} else {
    INCLUDEPATH += $$PWD/stub
    SOURCES += $$PWD/stub/sukkula_stub.c
    LIBS += -lpthread
    DEFINES += SUKKULA_STUB_ENGINE
}

# ASan and UBSan with CONFIG+=sukkula_sanitize (the FFI rows of spec §7).
sukkula_sanitize {
    QMAKE_CFLAGS += -fsanitize=address,undefined -fno-omit-frame-pointer
    QMAKE_CXXFLAGS += -fsanitize=address,undefined -fno-omit-frame-pointer
    QMAKE_LFLAGS += -fsanitize=address,undefined
}
