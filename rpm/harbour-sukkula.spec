# Sailfish packaging for Sukkula: the Qt/C++ shell and the QML UI, built by
# mb2 inside the Sailfish SDK, linked against the Rust engine that
# scripts/cross-build-rust.sh has already built (spec §3).
#
# The engine is not built here. The SDK ships Rust 1.75 and the engine
# needs 1.97.1, so it is cross-compiled beforehand with the pinned
# toolchain, the SDK's own aarch64 GCC and this target's sysroot, and this
# spec only links the static library it leaves behind. docs/BUILDING.md
# has the whole route; .github/workflows/rpm.yml runs it.
#
# Harbour's listing rules apply to everything below -- the name, the
# installed paths, every Requires, no scriptlets -- and ci/harbour-check.sh
# fails a change that breaks one. docs/HARBOUR.md is the map.
#
# No bare percent signs in comments: rpm expands macros inside comments,
# and the SDK's older rpm turns a stray build-section name into a parse
# error that a host rpmspec never shows (ci/packaging-lint.sh checks).

Name:       harbour-sukkula
Summary:    Send and receive files over LocalSend, Quick Share, Wormhole and Bluetooth
Version:    0.1.0
# Stamped per build by rpm.yml (digits and periods only, as Harbour
# requires); a release builds the 1 written here.
Release:    1
# Sukkula's own code is GPL-3.0-or-later, and the engine statically links
# the LocalSend core (Apache-2.0), open-quickshare's rqs_lib (GPL-3.0),
# magic-wormhole (EUPL-1.2, conveyed under the GPL by its compatibility
# appendix) and permissively licensed crates; the tag describes what the
# binary package contains. deny.toml holds the full list.
License:    GPL-3.0-or-later AND GPL-3.0-only AND Apache-2.0 AND EUPL-1.2 AND MIT AND BSD-3-Clause AND ISC
URL:        https://github.com/muhnschein/sukkula
Source0:    %{name}-%{version}.tar.gz

# The Jolla Phone 2026 and nothing else: Sailfish OS 5.2 and later, aarch64
# (spec §2). There is no other architecture to build, so there is no
# architecture-conditional anything below.
ExclusiveArch: aarch64

# Unversioned, and only packages on Harbour's allowed_requires.conf: the
# validator reads every whitespace-separated token, so an operator or a
# version is rejected on its own. The OS floor (5.2) is the submission
# form's "From OS version" field, not a Requires.
Requires:   sailfishsilica-qt5
# One package per QML module the UI imports beyond Silica itself
# (spec §2). Sailfish.Share and Sailfish.Pickers ship with the platform
# and have no package on the allowed list to name.
Requires:   nemo-qml-plugin-notifications-qt5
Requires:   libkeepalive

# The shell and the engine's link.
BuildRequires:  pkgconfig(sailfishapp) >= 1.0.3
BuildRequires:  pkgconfig(Qt5Core)
BuildRequires:  pkgconfig(Qt5Gui)
BuildRequires:  pkgconfig(Qt5Qml)
BuildRequires:  pkgconfig(Qt5Quick)
BuildRequires:  pkgconfig(Qt5DBus)
# libdbus-1.so.3, which the engine reaches BlueZ and obexd through
# (spec §2). Also what scripts/cross-build-rust.sh finds in this target's
# sysroot when it builds the engine, which is why the SDK image bakes it in.
BuildRequires:  pkgconfig(dbus-1)
BuildRequires:  desktop-file-utils
# lrelease, for the translation catalogs.
BuildRequires:  qt5-qttools-linguist

# Harbour allows no Provides beyond the package's own, and rpm derives one
# from any shared library it finds. Nothing under the data directory is
# one; this keeps it that way if one ever appears there.
%global __provides_exclude_from ^%{_datadir}/%{name}/.*$

# rpm derives a Requires from every symbol version the binary references.
# libdbus-1 versions its whole API as LIBDBUS_1_3, so the engine's link
# produces libdbus-1.so.3(LIBDBUS_1_3)(64bit) beside the plain
# libdbus-1.so.3()(64bit). The plain one is on Harbour's allow-list and is
# kept -- it is the real dependency on the system libdbus-1.so.3 -- but the
# versioned form is not, and the validator rejects it as "Cannot require
# shared library". Only that one string is dropped; the Harbour FAQ names
# __requires_exclude for exactly this.
%global __requires_exclude ^libdbus-1\\.so\\.3\\(LIBDBUS_1_3\\)\\(64bit\\)$

# A -debuginfo package is nothing Harbour takes, and the binary is
# stripped in the install section below, which leaves find-debuginfo
# nothing to find.
%global debug_package %{nil}

# Where scripts/cross-build-rust.sh leaves the engine, relative to the
# source tree mb2 builds in. Overridable with
# --define "sukkula_rust_lib /path/to/libsukkula_ffi.a".
%{!?sukkula_rust_lib: %global sukkula_rust_lib target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.a}

%description
Sukkula sends and receives files and text over LocalSend, Quick Share,
Magic Wormhole and Bluetooth, from one Silica app. Every offer is shown
before a byte is written, received files go to Downloads/Sukkula, and
nothing runs while the app is closed.

%prep
%setup -q -n %{name}-%{version}

%build
# The engine has to be there already: this spec links it and never builds
# it, and a missing library is a clearer failure here than a link error.
rustlib=%{sukkula_rust_lib}
case "$rustlib" in
    /*) ;;
    *) rustlib="$PWD/$rustlib" ;;
esac
if [ ! -f "$rustlib" ]; then
    echo "error: no Rust engine at $rustlib" >&2
    echo "error: run scripts/cross-build-rust.sh first (docs/BUILDING.md)" >&2
    exit 1
fi

# Out of tree: qmake writes a Makefile where it runs, and the source root
# already has one (the developer targets). build-sfos/ is gitignored.
mkdir -p build-sfos
cd build-sfos
%qmake5 ../harbour-sukkula.pro SUKKULA_RUST_LIB="$rustlib"
make %{?_smp_mflags}

%install
rm -rf %{buildroot}
cd build-sfos
%qmake5_install

# Stripped here, because nothing else does it: rpmbuild in the SDK does not
# strip what it packages, and qmake's install is told not to. --strip-all
# drops .symtab and keeps .dynsym, which is where main() lives once
# Q_DECL_EXPORT and sailfishapp's -rdynamic have put it there -- and where
# the booster looks for it (docs/HARBOUR.md).
%{__strip} --strip-all %{buildroot}%{_bindir}/%{name}

# The Sailfish template's own step: it validates the entry as it installs
# it, so a malformed one fails the build rather than the launcher.
desktop-file-install --delete-original \
    --dir %{buildroot}%{_datadir}/applications \
    %{buildroot}%{_datadir}/applications/%{name}.desktop

%files
%defattr(-,root,root,-)
%{_bindir}/%{name}
%{_datadir}/%{name}
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/*/apps/%{name}.png
