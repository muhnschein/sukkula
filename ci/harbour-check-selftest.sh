#!/bin/bash
# Prove that ci/harbour-check.sh still fails what it claims to fail, and
# that ci/harbour-validate-rpm.sh still judges the real validator's output
# the way docs/HARBOUR.md says.
#
# A gate that only ever prints "ok" is indistinguishable from a gate that
# has stopped looking, and every check in there is a regex over a file
# format someone will reformat one day. So each case below breaks one
# Harbour rule in a throwaway copy of a tree and asserts the check names
# it -- and a handful of cases change something the check must NOT object
# to, and assert it stays quiet, since a gate that fails everything is no
# better than one that fails nothing.
#
# The tree is the real packaging (rpm/, .github/, ci/, scripts/, the cargo
# manifests and crates) with ci/fixtures/harbour/ laid over it in place of
# the app's own Qt/QML files: the cases break known text, so they cannot
# depend on the real UI's wording. The real tree is judged by the check
# itself, in the same CI job. ci/fixtures/harbour/README says the rest.
#
# Strict by default (a missing rpmspec or file(1) fails the pristine case),
# because a selftest that skips the checks it tests proves nothing.
#
# The case scripts are single-quoted on purpose: eval expands them inside
# the copy, where $S and friends name its files.
# shellcheck disable=SC2016
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fixture="$root/ci/fixtures/harbour"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export HARBOUR_CHECK_STRICT=${HARBOUR_CHECK_STRICT:-1}

pristine="$work/pristine"
mkdir -p "$pristine"
for part in ci rpm .github scripts crates third_party Cargo.toml Cargo.lock; do
    [[ -e "$root/$part" ]] || continue
    tar -C "$root" -cf - --exclude=target --exclude=.git "$part" | tar -C "$pristine" -xf -
done
if [[ ! -d "$fixture" ]]; then
    echo "selftest: FAIL $fixture is missing" >&2
    exit 1
fi
# The fixture's files, dotfiles and all, minus its own README.
tar -C "$fixture" -cf - --exclude=./README . | tar -C "$pristine" -xf -

S=rpm/harbour-sukkula.spec
D=harbour-sukkula.desktop
P=harbour-sukkula.pro
M=src/main.cpp
Q=qml/cover/CoverPage.qml
R=qml/harbour-sukkula.qml
W=.github/workflows/rpm.yml
for f in "$S" "$D" "$P" "$M" "$Q" "$R" "$W" ci/harbour-check.sh ci/harbour-validate-rpm.sh; do
    if [[ ! -f "$pristine/$f" ]]; then
        echo "selftest: FAIL $f is not where this test expects it" >&2
        exit 1
    fi
done

status=0
cases=0

run_check() { "$1/ci/harbour-check.sh" 2>&1; }

# The unbroken tree has to pass, or every case below proves nothing.
cases=$((cases + 1))
if out=$(run_check "$pristine"); then
    echo "selftest: ok   the pristine fixture tree passes"
else
    echo "selftest: FAIL the pristine fixture tree does not pass:" >&2
    grep -E '^harbour-check: (FAIL|SKIP)' <<< "$out" >&2
    exit 1
fi

# stage <description> <script run inside a fresh copy>
stage() {
    local what=$1 script=$2
    dir="$work/case"
    rm -rf "$dir"
    cp -a "$pristine" "$dir"
    ( cd "$dir" && eval "$script" ) || {
        echo "selftest: FAIL could not apply '$what'" >&2
        status=1
        return 1
    }
    return 0
}

# break_and_expect <expected id> <description> <script>
break_and_expect() {
    local expect=$1 what=$2 script=$3 out
    cases=$((cases + 1))
    stage "$what" "$script" || return 0
    out=$(run_check "$dir")
    if grep -qF "FAIL [$expect]" <<< "$out"; then
        echo "selftest: ok   $what -> $expect"
    else
        echo "selftest: FAIL $what should have been reported as $expect" >&2
        grep -E '^harbour-check: (FAIL|WAIVED|SKIP)' <<< "$out" >&2 || echo "  (nothing failed)" >&2
        status=1
    fi
    return 0
}

# still_passes <description> <script>: a change the check must accept.
still_passes() {
    local what=$1 script=$2 out
    cases=$((cases + 1))
    stage "$what" "$script" || return 0
    if out=$(run_check "$dir"); then
        echo "selftest: ok   $what -> accepted"
    else
        echo "selftest: FAIL $what should have been accepted:" >&2
        grep -E '^harbour-check: (FAIL|SKIP)' <<< "$out" >&2
        status=1
    fi
    return 0
}

# No unescaped $ in a case script unless it is meant for the shell that
# eval runs it in: the scripts are single-quoted here and eval expands them.

# 1.1 Naming, and the workflow's stamp.
break_and_expect 1.1.1 "package name without the harbour- prefix" \
    'sed -i "s/^Name:.*/Name:       sukkula/" $S'
break_and_expect 1.1.3 "Version with a letter in it" \
    'sed -i "s/^Version:.*/Version:    0.1.0rc1/" $S'
break_and_expect 1.1.4 "Release with a letter in it" \
    'sed -i "s/^Release:.*/Release:    1.gabc123/" $S'
break_and_expect 1.1.4 "an rpm workflow that stamps a git hash into Release" \
    'sed -i "0,/release=\"1\.\${GITHUB_RUN_NUMBER}\"/s//release=\"1.\${GITHUB_RUN_NUMBER}.g\${short}\"/" $W'
break_and_expect 1.1.4 "an rpm workflow that no longer stamps Release" \
    'sed -i "/release=\"/d" $W'
break_and_expect 1.1.2 "a file name over Harbour's 100 characters" \
    'sed -i "s/^Version:.*/Version:    0.1.0.1234567890.1234567890.1234567890.1234567890.1234567890.1234567890.1234567890/" $S'

# 1.1.5, P.1, P.3: aarch64, Sailfish OS 5.2 and later, nothing else.
break_and_expect 1.1.5 "a spec that also builds armv7hl" \
    'sed -i "s/^ExclusiveArch:.*/ExclusiveArch: aarch64 armv7hl/" $S'
break_and_expect 1.1.5 "a spec without ExclusiveArch" \
    'sed -i "/^ExclusiveArch:/d" $S'
break_and_expect P.1 "an architecture-conditional block in the spec" \
    'printf "%%ifarch aarch64\n%%define x 1\n%%endif\n" >> $S'
break_and_expect P.1 "an rpm workflow that builds i486 too" \
    'sed -i "0,/ARCH: aarch64/s//ARCH: i486/" $W'
break_and_expect P.3 "an rpm workflow defaulting to a 4.x SDK" \
    'sed -i "s/5\.2\.0\.15/4.6.0.13/g" $W'

# 1.2 Layout, modes and ownership.
break_and_expect 1.2.1 "a file installed outside the allowed paths" \
    'sed -i "s|^%{_bindir}/%{name}\$|%{_bindir}/%{name}\n/etc/harbour-sukkula.conf|" $S'
break_and_expect 1.2.1 "a %license entry, which rpm installs outside the data directory" \
    'printf "%%license LICENSE\n" >> $S'
break_and_expect 1.2.6 "a file installed under /home" \
    'printf "/home/defaultuser/.config/sukkula.conf\n" >> $S'
break_and_expect 1.2.4 "debug symbols in the package" \
    'printf "/usr/lib/debug/usr/bin/harbour-sukkula.debug\n" >> $S'
break_and_expect 1.2.2 "the .desktop file left out of %files" \
    'sed -i "/applications\/%{name}.desktop\$/d" $S'
break_and_expect 1.2.3 "the binary left out of %files" \
    'sed -i "/^%{_bindir}\/%{name}\$/d" $S'
break_and_expect 1.5.2 "the icons left out of %files" \
    'sed -i "/hicolor/d" $S'
break_and_expect 1.2.8 "files owned by someone other than root" \
    'sed -i "s/^%defattr(-,root,root,-)/%defattr(-,nemo,nemo,-)/" $S'
break_and_expect 1.2.8 "a setuid binary" \
    'sed -i "s|^%{_bindir}/%{name}\$|%attr(4755,root,root) %{_bindir}/%{name}|" $S'
break_and_expect 1.2.7 "a world-writable file installed by hand" \
    'sed -i "s|^%qmake5_install\$|%qmake5_install\ninstall -m 0666 LICENSE %{buildroot}%{_datadir}/%{name}/LICENSE|" $S'
break_and_expect 1.2.5 "an editor backup inside qml/" \
    'cp $Q $Q~'
break_and_expect 1.2.9 "an executable QML file" \
    'chmod +x $Q'
break_and_expect 1.6.7 "an ELF file inside qml/" \
    'cp "$(type -P true)" qml/components/libhelper.so'

# 1.8 RPM metadata.
break_and_expect 1.8.1 "a Vendor: tag" \
    'sed -i "s|^URL:|Vendor:     acme\nURL:|" $S'
break_and_expect 1.8.2 "an explicit Provides:" \
    'sed -i "s|^URL:|Provides:   sukkula\nURL:|" $S'
break_and_expect 1.8.2 "an explicit Obsoletes:" \
    'sed -i "s|^URL:|Obsoletes:  harbour-parvi\nURL:|" $S'
break_and_expect 1.8.3 "a dependency that is not on the allowed list" \
    'sed -i "s|^Requires:   sailfishsilica-qt5\$|Requires:   sailfishsilica-qt5\nRequires:   openssl-libs|" $S'
break_and_expect 1.8.3 "a dependency that was dropped from the platform" \
    'sed -i "s|^Requires:   sailfishsilica-qt5\$|Requires:   sailfishsilica-qt5\nRequires:   qt5-qtqml-import-webkitplugin|" $S'
break_and_expect 1.8.4 "a versioned Requires" \
    'sed -i "s|^Requires:   sailfishsilica-qt5\$|Requires:   sailfishsilica-qt5 >= 0.10.9|" $S'
break_and_expect 1.8.5 "an RPM scriptlet" \
    'printf "\n%%post\n/bin/true\n" >> $S'
break_and_expect 1.8.5 "an RPM file trigger" \
    'printf "\n%%filetriggerin -- /usr/share\n/bin/true\n" >> $S'
break_and_expect 1.8.7 "requiring the sailfish-qml launcher in a C++ app" \
    'sed -i "s|^Requires:   sailfishsilica-qt5\$|Requires:   sailfishsilica-qt5\nRequires:   libsailfishapp-launcher|" $S'
break_and_expect 1.8.6 "XmlListModel imported without requiring its package" \
    'sed -i "1i import QtQuick.XmlListModel 2.0" $Q'
break_and_expect 1.8.8 "no filter for libdbus-1's versioned Requires" \
    'sed -i "/^%global __requires_exclude/d" $S'
break_and_expect 1.8.8 "a filter that also drops the real libdbus-1 dependency" \
    'sed -i "s|^%global __requires_exclude.*|%global __requires_exclude ^libdbus-1.*\$|" $S'
break_and_expect 2.7 "Nemo.Notifications imported without its package" \
    'sed -i "/^Requires:   nemo-qml-plugin-notifications-qt5\$/d" $S'
break_and_expect 2.7 "Nemo.KeepAlive imported without its package" \
    'sed -i "/^Requires:   libkeepalive\$/d" $S'

# 1.3 The .desktop file.
break_and_expect 1.3.1 "a missing .desktop file" \
    'rm $D'
break_and_expect 1.3.1 "an empty Name=" \
    'sed -i "s|^Name=.*|Name=|" $D'
break_and_expect 1.3.2 "an Exec= that is not the package name" \
    'sed -i "s|^Exec=.*|Exec=/usr/bin/harbour-sukkula|" $D'
break_and_expect 1.3.2 "the sailfish-qml launcher in a C++ app" \
    'sed -i "s|^Exec=.*|Exec=sailfish-qml harbour-sukkula|" $D'
break_and_expect 1.3.3 "an Icon= with a path in it" \
    'sed -i "s|^Icon=.*|Icon=/usr/share/icons/x.png|" $D'
break_and_expect 1.3.4 "a missing Type=Application" \
    'sed -i "s|^Type=Application\$|Type=Service|" $D'
break_and_expect 1.3.5 "an X-Nemo-Application-Type that is not silica-qt5" \
    'sed -i "s|^X-Nemo-Application-Type=.*|X-Nemo-Application-Type=generic|" $D'
break_and_expect 1.3.6 "a [Sailjail] section header" \
    'sed -i "s|^\[X-Sailjail\]\$|[Sailjail]|" $D'
break_and_expect 1.3.7 "no [X-Sailjail] section" \
    'sed -i "/^\[X-Sailjail\]\$/,\$d" $D'

# 1.4 Sailjail, and P.2/P.4.
break_and_expect 1.4.1 "an OrganizationName with illegal characters" \
    'sed -i "s|^OrganizationName=.*|OrganizationName=Sukkula!|" $D'
break_and_expect 1.4.2 "an OrganizationName component starting with a digit" \
    'sed -i "s|^OrganizationName=.*|OrganizationName=9sukkula|" $D'
break_and_expect 1.4.3 "a reserved OrganizationName" \
    'sed -i "s|^OrganizationName=.*|OrganizationName=com.jolla|" $D'
break_and_expect 1.4.4 "an ApplicationName with illegal characters" \
    'sed -i "s|^ApplicationName=.*|ApplicationName=.sukkula|" $D'
break_and_expect 1.4.5 "a permission that is not on the whitelist" \
    'sed -i "s|^Permissions=.*|Permissions=Internet;Bluetooth;Downloads;Telepathy|" $D'
break_and_expect 1.4.5 "the Compatibility permission" \
    'sed -i "s|^Permissions=.*|Permissions=Internet;Bluetooth;Downloads;Compatibility|" $D'
break_and_expect 1.4.6 "an ExecDBus that is not the Exec value" \
    'printf "ExecDBus=/usr/bin/harbour-sukkula --dbus\n" >> $D'
break_and_expect 1.4.7 "a key that is not allowed in [X-Sailjail]" \
    'printf "DBusName=org.example\n" >> $D'
break_and_expect P.2 "a whitelisted permission spec §2 does not grant" \
    'sed -i "s|^Permissions=.*|Permissions=Internet;Bluetooth;Downloads;Camera|" $D'
break_and_expect P.2 "a permission spec §2 requires, dropped" \
    'sed -i "s|^Permissions=.*|Permissions=Internet;Downloads|" $D'
break_and_expect P.2 "UserDirs in place of Downloads" \
    'sed -i "s|^Permissions=.*|Permissions=Internet;Bluetooth;UserDirs|" $D'
break_and_expect P.4 "a renamed ApplicationName" \
    'sed -i "s|^ApplicationName=.*|ApplicationName=Sukkula|" $D'
break_and_expect 2.5 "a data path the sandbox grant does not match" \
    'sed -i "/AppDataLocation/d" $M'

# 1.6 QML imports, and P.5.
break_and_expect 1.6.4 "a QML import at a version Harbour does not allow" \
    'sed -i "s|^import QtQuick 2.0\$|import QtQuick 2.7|" $Q'
break_and_expect 1.6.4 "an import that was dropped from the platform" \
    'sed -i "1i import QtWebKit 3.0" $Q'
break_and_expect 1.6.4 "a deprecated import" \
    'sed -i "1i import org.nemomobile.notifications 1.0" $Q'
break_and_expect 1.6.4 "a private QML module under a blocked prefix" \
    'sed -i "1i import Nemo.Sukkula 1.0" $Q'
break_and_expect 1.6.4 "a second import on one line, after a semicolon" \
    'sed -i "s|^import QtQuick 2.6; import Sailfish.Silica 1.0\$|import QtQuick 2.6; import QtQuick 2.7|" qml/components/OfferItem.qml'
break_and_expect 1.6.5 "an absolute-path QML import" \
    'sed -i "s|^import \"pages\"\$|import \"/usr/share/harbour-sukkula/qml/pages\"|" $R'
break_and_expect 1.6.6 "a relative import pointing outside the installed tree" \
    'sed -i "s|^import \"pages\"\$|import \"../icons\"|" $R'
break_and_expect 1.6.6 "a relative import that resolves to nothing" \
    'sed -i "s|^import \"pages\"\$|import \"dialogs\"|" $R'
break_and_expect 1.6.8 "a test directory inside qml/" \
    'mkdir -p qml/tests && cp tests/qml/tst_offer.qml qml/tests/'
break_and_expect P.5 "a Harbour-allowed platform module spec §2 does not list" \
    'sed -i "1i import Sailfish.WebView 1.0" $Q'
break_and_expect 1.6.4 "a missing qml/ tree" \
    'rm -rf qml'

# 1.5 Icons.
break_and_expect 1.5.1 "a missing icon size" \
    'rm icons/128x128/harbour-sukkula.png'
break_and_expect 1.5.4 "an icon whose pixels do not match its directory" \
    'cp icons/86x86/harbour-sukkula.png icons/172x172/harbour-sukkula.png'
break_and_expect 1.5.3 "an icon that is not a PNG" \
    'printf "not a png" > icons/86x86/harbour-sukkula.png'
break_and_expect 1.5.2 "a .pro that installs only three icon sizes" \
    'sed -i "s|^SAILFISHAPP_ICONS = .*|SAILFISHAPP_ICONS = 86x86 108x108 128x128|" $P'

# The qmake project and the entry point.
break_and_expect 1.2.3 "a qmake TARGET other than the package name" \
    'sed -i "s|^TARGET = .*|TARGET = sukkula|" $P'
break_and_expect 1.2.3 "a missing qmake project" \
    'rm $P'
break_and_expect 1.7.3 "a main() without Q_DECL_EXPORT" \
    'sed -i "s|^Q_DECL_EXPORT int main|int main|" $M'
break_and_expect 1.7.3 "a missing src/main.cpp" \
    'rm $M'
break_and_expect 1.7.3 "a project without CONFIG += sailfishapp" \
    'sed -i "s|^CONFIG += sailfishapp sailfishapp_i18n\$|CONFIG += c++17|" $P'
break_and_expect 1.6.1 "a library Harbour does not allow" \
    'sed -i "s|-ldbus-1 -lpthread|-ldbus-1 -lutil -lpthread|" $P'
break_and_expect 1.6.1 "QtWidgets" \
    'sed -i "s|^QT += dbus\$|QT += dbus widgets|" $P'
break_and_expect 1.6.1 "a pkg-config library Harbour does not allow" \
    'printf "PKGCONFIG += openssl\n" >> $P'

# 2.x runtime policy.
break_and_expect 2.1 "a hardcoded /home/defaultuser in the C++ shell" \
    'sed -i "s|    Q_UNUSED(data);|    Q_UNUSED(data); const char *d = \"/home/defaultuser/Downloads\"; Q_UNUSED(d);|" $M'
break_and_expect 2.1 "a hardcoded /home/nemo in the engine" \
    'printf "pub const DIR: &str = \"/home/nemo/Downloads\";\n" >> crates/sukkula-core/src/lib.rs'
break_and_expect 2.1 "a hardcoded home directory in QML" \
    'sed -i "s|text: qsTr(\"Receiving\")|text: \"/home/nemo/\"|" $Q'
break_and_expect 2.6 "a write into the installed data directory" \
    'sed -i "s|    Q_UNUSED(data);|    QDir().mkpath(\"/usr/share/harbour-sukkula/cache\");|" $M'
# A line starting with `#` or `*` is code in every language 2.1 reads; only
# `//` and `/* */` are comments (finding "Harbour source gate skips every
# '#' line").
break_and_expect 2.1 "a home directory in a Rust attribute, which Display prints" \
    'printf "#[error(\"cannot write to /home/defaultuser/Downloads/Sukkula\")]\npub struct _E;\n" >> crates/sukkula-core/src/lib.rs'
break_and_expect 2.1 "a home directory in a C++ #define" \
    'sed -i "1i #define SUKKULA_DL \"/home/defaultuser/Downloads/\"" $M'
break_and_expect 2.1 "a C++ line starting with * that is code" \
    'printf "static void f(const char **out) {\n    *out = \"/home/nemo/x\";\n}\n" >> $M'
break_and_expect 2.1 "a JavaScript private field starting with #" \
    'printf "class A {\n    #home = \"/home/nemo/\";\n}\n" > qml/components/paths.js'
break_and_expect 2.1 "code after a block comment closes, on its last line" \
    'printf "/* a\n * b\n */ pub const D: &str = \"/home/nemo/y\";\n" >> crates/sukkula-core/src/lib.rs'
break_and_expect 2.1 "a home directory after a // inside a string" \
    'printf "pub const U: &str = \"http://x\"; pub const D: &str = \"/home/nemo/z\";\n" >> crates/sukkula-core/src/lib.rs'
break_and_expect 2.1 "a data file include_str! can read" \
    'printf "{\"dir\": \"/home/defaultuser/Downloads\"}\n" > crates/sukkula-core/src/defaults.json'

# P.6: Jolla's validator runs on every pull request that changes the package.
break_and_expect P.6 "rpm.yml no longer built for a change to the Rust crates" \
    'sed -i "\|^      - \"crates/\*\*\"\$|d" $W'
break_and_expect P.6 "rpm.yml no longer built for a change to the C++ shell" \
    'sed -i "\|^      - \"src/\*\*\"\$|d" $W'
break_and_expect P.6 "rpm.yml no longer built for a change to the QML" \
    'sed -i "\|^      - \"qml/\*\*\"\$|d" $W'
break_and_expect P.6 "rpm.yml with no pull_request trigger at all" \
    'sed -i "s/^  pull_request:\$/  pull_request_review:/" $W'
still_passes "rpm.yml built for every pull request, with no paths filter" \
    'sed -i "/^  pull_request:\$/,/^\$/{/^    paths:\$/d; /^      /d}" $W && grep -q "^  pull_request:\$" $W && ! grep -q "crates/\*\*" $W'

# And what the check must leave alone.
still_passes "a doc comment that mentions /home/defaultuser" \
    'printf "/// e.g. /home/defaultuser/Downloads/Sukkula\npub const _X: u8 = 0;\n" >> crates/sukkula-core/src/lib.rs'
still_passes "a block comment that mentions /home/defaultuser, * lines and all" \
    'printf "/*\n * e.g. /home/defaultuser/Downloads/Sukkula\n */\npub const _Y: u8 = 0;\n" >> crates/sukkula-core/src/lib.rs'
still_passes "a trailing comment that mentions /home/nemo" \
    'printf "pub const _Z: u8 = 0; // not /home/nemo/\n" >> crates/sukkula-core/src/lib.rs'
still_passes "bad imports in tests/ and qml-stubs/, which never ship" \
    'printf "import QtWebKit 3.0\nimport \"/abs\"\n" >> qml-stubs/Sailfish/Silica/Page.qml'
still_passes "ExecDBus naming the binary" \
    'printf "ExecDBus=harbour-sukkula\n" >> $D'
still_passes "a private module of the app's own" \
    'sed -i "1i import harbour.sukkula 1.0" $Q'

# The waiver file has to stay honest in both directions: an entry that
# stops matching is as much a defect as a missing check. Every line says
# which check it is for (ci/harbour-waivers.sh), and a check matches every
# field it has -- the source check the ID, the subject and the message.
# refused <what> <expected output> <script>: the check fails, saying so.
refused() {
    local what=$1 expect=$2 script=$3 out
    cases=$((cases + 1))
    stage "$what" "$script" || return 0
    if out=$(run_check "$dir"); then
        echo "selftest: FAIL $what should have failed the check" >&2
        status=1
    elif grep -qF -- "$expect" <<< "$out"; then
        echo "selftest: ok   $what -> $expect"
    else
        echo "selftest: FAIL $what should have said '$expect':" >&2
        grep -E 'FAIL' <<< "$out" >&2
        status=1
    fi
    return 0
}
OPENSSL='sed -i "s|^Requires:   sailfishsilica-qt5\$|Requires:   sailfishsilica-qt5\nRequires:   openssl-libs|" $S'
refused "a waiver that matches nothing" "stale waiver" \
    'printf "source  9.9.9  /nowhere  *nothing*  # excuses nothing\n" >> ci/harbour/waivers.conf'
refused "a waiver naming the finding's ID and subject but another message" "FAIL [1.8.3] openssl-libs" \
    "$OPENSSL"' && printf "source  1.8.3  openssl-libs  *deprecated*  # test\n" >> ci/harbour/waivers.conf'
refused "a waiver whose message glob matches anything" "matches any message" \
    "$OPENSSL"' && printf "source  1.8.3  openssl-libs  *  # test\n" >> ci/harbour/waivers.conf'
refused "a waiver in the old format, with no checker" "is not a checker" \
    "$OPENSSL"' && printf "1.8.3  openssl-libs  *allowed list*  # test\n" >> ci/harbour/waivers.conf'
refused "a waiver with no reason" "no reason" \
    "$OPENSSL"' && printf "source  1.8.3  openssl-libs  *allowed list*\n" >> ci/harbour/waivers.conf'
refused "an rpm waiver whose id is not a severity" "ERROR or WARNING" \
    'printf "rpm  1.2.3  /usr/bin/harbour-sukkula  *stripped*  # test\n" >> ci/harbour/waivers.conf'

# A live waiver does excuse its own finding, and only that one.
cases=$((cases + 1))
stage "a waived finding" \
    "$OPENSSL"' && printf "source  1.8.3  openssl-libs  *not on Harbour*s allowed list  # test\n" >> ci/harbour/waivers.conf'
if out=$(run_check "$dir") && grep -qF 'WAIVED [1.8.3] openssl-libs' <<< "$out"; then
    echo "selftest: ok   a waiver excuses exactly its finding -> WAIVED"
else
    echo "selftest: FAIL a matching waiver should excuse its finding and pass" >&2
    grep -E '^harbour-(check|waivers): (FAIL|WAIVED)' <<< "$out" >&2
    status=1
fi

# An `rpm` waiver is the RPM check's business: a validator-only finding
# can be waived without the source check calling the waiver stale, which
# is what it once did to every one of them.
still_passes "an rpm waiver, which only the RPM check reads" \
    'printf "rpm  WARNING  /usr/bin/harbour-sukkula  file is not stripped!  # test\n" >> ci/harbour/waivers.conf'

#
# ci/harbour-validate-rpm.sh judges the real validator's output against the
# same waiver file. Fed saved logs rather than a built RPM, which needs the
# SDK. Sukkula waives nothing, so every ERROR and every WARNING fails it.
#
validate_rpm() {
    local expect=$1 what=$2 body=$3 tree=${4:-$pristine}
    local log="$work/validation.log" got
    cases=$((cases + 1))
    printf '%s' "$body" > "$log"
    if "$tree/ci/harbour-validate-rpm.sh" --log "$log" >/dev/null 2>&1; then
        got=pass
    else
        got=fail
    fi
    if [[ "$got" = "$expect" ]]; then
        echo "selftest: ok   $what -> $expect"
    else
        echo "selftest: FAIL $what should $expect, got $got" >&2
        "$tree/ci/harbour-validate-rpm.sh" --log "$log" >&2
        status=1
    fi
    return 0
}

validate_rpm pass "an RPM the validator accepts outright" \
'!BEGIN!x
=Package name
INFO|/usr/share/harbour-sukkula/qml/harbour-sukkula.qml|Uses Sailfish Silica Components (only reported once)
OK|rpath in binary seems to be ok: '"'"'empty'"'"'|
!END!PASS!x
'
validate_rpm fail "an RPM whose binary stopped exporting main()" \
'!BEGIN!x
ERROR|/usr/bin/harbour-sukkula|Binary must export main() symbol for booster to work (Q_DECL_EXPORT)
!END!FAIL!x
'
validate_rpm fail "an unstripped binary (a warning, and Sukkula fails warnings)" \
'!BEGIN!x
WARNING|/usr/bin/harbour-sukkula|file is not stripped!
!END!PASS!x
'
validate_rpm fail "the versioned libdbus Requires reaching the package" \
'!BEGIN!x
ERROR|libdbus-1.so.3(LIBDBUS_1_3)(64bit)|Cannot require shared library: '"'"'libdbus-1.so.3(LIBDBUS_1_3)(64bit)'"'"'
!END!FAIL!x
'
validate_rpm fail "a binary linking a library Harbour does not allow" \
'!BEGIN!x
ERROR|/usr/bin/harbour-sukkula|Cannot link to shared library: libutil.so.1
!END!FAIL!x
'
validate_rpm fail "a validation log that was cut off before the verdict" \
'ERROR|/usr/bin/harbour-sukkula|something
'
validate_rpm fail "a verdict of FAIL with no finding the wrapper could parse" \
'!BEGIN!x
!END!FAIL!x
'

# The runner's Node parent ignores SIGPIPE and every child inherits it, so
# a validator pipeline whose reader stops early reports each write instead
# of dying. The wrapper resets the disposition before running the
# validator, and hides whatever still gets through -- so a log full of it
# is still judged on its markers.
validate_rpm pass "a log buried in broken-pipe noise" \
"!BEGIN!x
$(for _ in $(seq 1 50); do
    printf '%s\n' '/tmp/harbour-validator/rpmvalidation.sh: line 773: echo: write error: Broken pipe'
done)
!END!PASS!x
"

cases=$((cases + 1))
noise_log="$work/noisy.log"
printf '%s\n' \
    'sh: line 773: echo: write error: Broken pipe' \
    '!BEGIN!x' '!END!PASS!x' > "$noise_log"
noise_out=$("$pristine/ci/harbour-validate-rpm.sh" --log "$noise_log" 2>&1)
if grep -q 'Broken pipe' <<< "$noise_out"; then
    echo "selftest: FAIL the wrapper echoed the validator's broken-pipe noise" >&2
    status=1
else
    echo "selftest: ok   broken-pipe noise -> not echoed"
fi

# The waiver machinery, on a copy with one `rpm` waiver in it: the
# severity, the subject and the message all have to match, so a waived
# path with a new error about it is news.
# waiver_tree <name> <line>: a copy of the pristine tree with one waiver.
waiver_tree() {
    local tree="$work/$1"
    rm -rf "$tree"
    cp -a "$pristine" "$tree"
    printf '%s\n' "$2" >> "$tree/ci/harbour/waivers.conf"
    echo "$tree"
}
waived_tree=$(waiver_tree waived 'rpm  ERROR  /usr/share/harbour-sukkula/x  Installation not allowed*  # test')
validate_rpm pass "a finding an rpm waiver names: severity, subject and message" \
'!BEGIN!x
ERROR|/usr/share/harbour-sukkula/x|Installation not allowed in this location
!END!FAIL!x
' "$waived_tree"
validate_rpm fail "a waived path with a message the waiver does not name" \
'!BEGIN!x
ERROR|/usr/share/harbour-sukkula/x|Installation not allowed in this location
ERROR|/usr/share/harbour-sukkula/x|setuid, setgid or sticky bit set
!END!FAIL!x
' "$waived_tree"
validate_rpm fail "a waived message about a path the waiver does not name" \
'!BEGIN!x
ERROR|/usr/share/harbour-sukkula/x|Installation not allowed in this location
ERROR|/usr/share/harbour-sukkula/y|Installation not allowed in this location
!END!FAIL!x
' "$waived_tree"
validate_rpm fail "a waived subject and message at another severity" \
'!BEGIN!x
ERROR|/usr/share/harbour-sukkula/x|Installation not allowed in this location
WARNING|/usr/share/harbour-sukkula/x|Installation not allowed in this location
!END!FAIL!x
' "$waived_tree"
validate_rpm fail "an rpm waiver the package no longer needs (stale)" \
'!BEGIN!x
!END!PASS!x
' "$waived_tree"
# A source waiver excuses nothing here: once, one written for the source
# check with the message `*` waived every validator finding about its path.
source_tree=$(waiver_tree source-waived 'source  1.2.3  /usr/bin/harbour-sukkula  *must install its binary*  # test')
validate_rpm fail "a validator finding about a path only a source waiver names" \
'!BEGIN!x
ERROR|/usr/bin/harbour-sukkula|Binary must export main() symbol for booster to work (Q_DECL_EXPORT)
!END!FAIL!x
' "$source_tree"
star_tree=$(waiver_tree star 'rpm  ERROR  /usr/bin/harbour-sukkula  *  # test')
validate_rpm fail "an rpm waiver whose message glob matches anything" \
'!BEGIN!x
ERROR|/usr/bin/harbour-sukkula|Cannot link to shared library: libutil.so.1
!END!FAIL!x
' "$star_tree"

echo
if [[ "$status" -eq 0 ]]; then
    echo "selftest: ok ($cases cases)"
else
    echo "selftest: FAILED" >&2
fi
exit "$status"
