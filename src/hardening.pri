# SPDX-License-Identifier: GPL-3.0-or-later
#
# Compiler and linker hardening for the harbour-sukkula binary. Included by
# harbour-sukkula.pro and by the host build in tests/cpp/host_app, so the
# host tests link with exactly these flags and can check what they do.

# Stack protector and fortified libc calls. -U first: the SDK's optflags
# may already define _FORTIFY_SOURCE, and a second, different -D warns.
# -fPIE is asked for here; qmake's own -fPIC, which Qt requires and which
# comes later on the command line, supersedes it. Either one makes
# position-independent code for -pie.
QMAKE_CFLAGS += -fstack-protector-strong -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=2 -fPIE
QMAKE_CXXFLAGS += -fstack-protector-strong -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=2 -fPIE
QMAKE_CXXFLAGS += -Wall -Wextra

# A position-independent executable with full RELRO.
QMAKE_LFLAGS += -pie -Wl,-z,relro,-z,now
# Record only the libraries something actually uses: the Rust std names a
# few that Harbour does not allow (libutil) and uses none of them.
QMAKE_LFLAGS += -Wl,--as-needed
# Export main() and nothing else (src/dynamic.list; Harbour 1.7). The SDK's
# sailfishapp feature adds -rdynamic, which would export every global
# symbol of the Rust static library too; --exclude-libs,ALL keeps symbols
# from archives out of the dynamic symbol table, so main() -- from our own
# object file -- is all that is left.
QMAKE_LFLAGS += -Wl,--dynamic-list=$$PWD/dynamic.list -Wl,--exclude-libs,ALL
# Strip at link time: the SDK's rpmbuild does not, and Harbour rejects an
# unstripped binary. -s keeps .dynsym, so the booster still finds main().
QMAKE_LFLAGS += -s
# The RPATH the SDK's sailfishapp feature sets, /usr/share/$$TARGET/lib,
# is where the engine's library is installed. Written as DT_RPATH, not
# RUNPATH: Jolla's validator reads "Library rpath:" and nothing else, and
# fails a package that ships a library without one (docs/HARBOUR.md).
QMAKE_LFLAGS += -Wl,--disable-new-dtags
