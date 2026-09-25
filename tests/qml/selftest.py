#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""The QML checks, checked: break the app in a copy of the tree, one fault
at a time, and require the check named for it to fail.

A gate that only ever prints "ok" cannot be told apart from one that has
stopped looking (the same reasoning as piirit's harbour-check-selftest).

Usage: selftest.py <repository root> <qml_runner> <english .qm>
"""

import os
import shutil
import subprocess
import sys
import tempfile

ROOT, RUNNER, QM = (os.path.abspath(a) for a in sys.argv[1:4])

# (what, file, old, new, how it must be caught: "static" or a test file)
MUTATIONS = [
    ("a peer label without PlainText", "qml/pages/ConsentDialog.qml",
     """                text: dialog.offer ? dialog.offer.sender : ""
                textFormat: Text.PlainText""",
     """                text: dialog.offer ? dialog.offer.sender : \"\"""",
     ["static", "tst_consent.qml"]),
    ("the sender handed to a Silica header", "qml/pages/ConsentDialog.qml",
     """            DialogHeader {""",
     """            DialogHeader {
                title: dialog.offer ? dialog.offer.sender : \"\"""",
     ["tst_consent.qml"]),
    ("a misspelt PlainText", "qml/pages/MainPage.qml",
     """                            text: model.text
                            textFormat: Text.PlainText""",
     """                            text: model.text
                            textFormat: Text.Plaintext""",
     ["tst_main.qml"]),
    ("a received text rendered as rich text", "qml/pages/TextPage.qml",
     """                text: page.text
                textFormat: Text.PlainText""",
     """                text: page.text
                textFormat: Text.RichText""",
     ["static", "tst_main.qml"]),
    ("a clickable link", "qml/pages/TextPage.qml",
     """                wrapMode: Text.WrapAtWordBoundaryOrAnywhere""",
     """                wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                onLinkActivated: Qt.openUrlExternally(link)""",
     ["static"]),
    ("Accept that does not accept", "qml/pages/ConsentDialog.qml",
     """    onAccepted: dialog.answer(true)""",
     """    onAccepted: {}""",
     ["tst_consent.qml"]),
    ("a countdown that never declines", "qml/pages/ConsentDialog.qml",
     """            if (dialog.remaining <= 0) {
                dialog.dismiss()
            }""",
     """            if (dialog.remaining < -1000) {
                dialog.dismiss()
            }""",
     ["tst_consent.qml"]),
    ("leaving the dialog without an answer", "qml/pages/ConsentDialog.qml",
     """    Component.onDestruction: {
        dialog.answer(false)
        dialog.gone()
    }""",
     """    Component.onDestruction: {
        dialog.gone()
    }""",
     ["tst_consent.qml"]),
    ("KeepAlive held all the time", "qml/harbour-sukkula.qml",
     """        enabled: sukkula.activeTransfers > 0""",
     """        enabled: true""",
     ["tst_app.qml"]),
    ("a peer name in a notification", "qml/harbour-sukkula.qml",
     """        summary: qsTr("Someone nearby wants to send you files")""",
     """        summary: sukkula.offers.count > 0 ? sukkula.offers.get(0).sender : \"\"""",
     ["tst_app.qml"]),
    ("an ES6 declaration", "qml/engine/Engine.qml",
     """        var id = engine._nextId
        engine._nextId = id + 1""",
     """        let id = engine._nextId
        engine._nextId = id + 1""",
     ["static"]),
    ("an import Harbour does not allow", "qml/pages/AboutPage.qml",
     """import Sailfish.Silica 1.0""",
     """import Sailfish.Silica 1.0
import QtQuick.Controls 2.0""",
     ["static"]),
    ("a share method nothing answers", "harbour-sukkula.desktop",
     """X-Share-Methods=files;text""",
     """X-Share-Methods=files;text;images""",
     ["static"]),
    ("a sandbox permission too many", "harbour-sukkula.desktop",
     """Permissions=Internet;Bluetooth;Downloads""",
     """Permissions=Internet;Bluetooth;Downloads;UserDirs""",
     ["static"]),
    ("an offer's expiry taken at its word", "qml/engine/Engine.qml",
     """Math.min(engine._count(o.expires_in), engine.offerTimeoutSeconds)""",
     """engine._count(o.expires_in)""",
     ["tst_engine.qml"]),
    ("an unbounded offer queue", "qml/engine/Engine.qml",
     """        if (engine.offers.count >= engine.maxOffers) {""",
     """        if (false) {""",
     ["tst_engine.qml"]),
    ("a second ending for a transfer", "qml/engine/Engine.qml",
     """        if (i < 0 || engine.transfers.get(i).state !== "active") {
            return
        }
        var outcome""",
     """        if (i < 0) {
            return
        }
        var outcome""",
     ["tst_engine.qml"]),
    ("a string no catalogue has", "qml/pages/AboutPage.qml",
     """qsTr("Built on")""",
     """qsTr("Built upon")""",
     ["static"]),
    # The page stack's races (review kept[4], kept[5], kept[21], kept[22]).
    ("Settings saved whenever a page covers them", "qml/pages/SettingsPage.qml",
     """    Component.onDestruction: page.save()""",
     """    onStatusChanged: {
        if (page.status === PageStatus.Deactivating) {
            page.save()
        }
    }""",
     ["tst_settings.qml", "tst_stack.qml"]),
    ("discovery stopped by any page that leaves", "qml/engine/Engine.qml",
     """        if (engine.discoveryUsers > 0) {
            return 0
        }""",
     """        if (false) {
            return 0
        }""",
     ["tst_engine.qml", "tst_stack.qml"]),
    ("a send's reply that pops whatever is on top", "qml/pages/SendPage.qml",
     """        if (pageStack.currentPage !== page || page.status !== PageStatus.Active) {
            return
        }""",
     """        if (false) {
            return
        }""",
     ["tst_stack.qml"]),
    ("a code's reply that pops whatever is on top", "qml/pages/WormholeReceivePage.qml",
     """        if (pageStack.currentPage !== page || page.status !== PageStatus.Active) {
            return
        }""",
     """        if (false) {
            return
        }""",
     ["tst_stack.qml"]),
    ("the next dialog let in while the last is still on the stack", "qml/pages/ConsentDialog.qml",
     """            dialog.engine.answer(dialog.offerId, accept)
        }
    }""",
     """            dialog.engine.answer(dialog.offerId, accept)
        }
        dialog.gone()
    }""",
     ["tst_stack.qml"]),
    ("a closed offer answered", "qml/engine/Engine.qml",
     """        if (engine.offer(offerId) === null) {
            return 0
        }""",
     """        if (false) {
            return 0
        }""",
     ["tst_engine.qml", "tst_stack.qml"]),
    ("Magic Wormhole that cannot be switched off", "qml/engine/Engine.qml",
     """            return !(s.wormhole && s.wormhole.enabled === false)""",
     """            return true""",
     ["tst_send.qml"]),
    ("the Magic Wormhole switch never saved", "qml/pages/SettingsPage.qml",
     """        s.wormhole.enabled = wormholeSwitch.checked""",
     """        s.wormhole.enabled = true""",
     ["tst_settings.qml"]),
]

COPY = ["qml", "qml-stubs", "tests/qml", "translations", "harbour-sukkula.desktop", "src"]


def copy_tree(dest):
    for item in COPY:
        src = os.path.join(ROOT, item)
        dst = os.path.join(dest, item)
        if os.path.isdir(src):
            shutil.copytree(src, dst, ignore=shutil.ignore_patterns("runner"))
        else:
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(src, dst)


ENV = dict(os.environ, QT_QPA_PLATFORM="offscreen", QT_QUICK_BACKEND="software")
ENV.pop("QML2_IMPORT_PATH", None)
ENV.pop("QML_IMPORT_PATH", None)


def passes(tree, how):
    if how == "static":
        run = subprocess.run([sys.executable, os.path.join(tree, "tests/qml/static_checks.py"), tree],
                             capture_output=True, text=True, env=ENV)
    else:
        run = subprocess.run([RUNNER, "--app", os.path.join(tree, "qml"), "--stubs", os.path.join(tree, "qml-stubs"),
                              "--qm", QM, "--timeout", "15000", os.path.join(tree, "tests/qml", how)],
                             capture_output=True, text=True, env=ENV)
    return run.returncode == 0


def main():
    failures = []
    runtime = tempfile.mkdtemp()
    os.chmod(runtime, 0o700)
    ENV["XDG_RUNTIME_DIR"] = runtime
    with tempfile.TemporaryDirectory() as base:
        pristine = os.path.join(base, "pristine")
        copy_tree(pristine)
        # A check that fails on the untouched tree proves nothing when it
        # fails on a broken one: it must pass first.
        for how in sorted({h for m in MUTATIONS for h in m[4]}):
            if not passes(pristine, how):
                failures.append(f"{how} fails on the unmodified tree, so it cannot vouch for anything")
        if failures:
            print("qml selftest: FAIL")
            for f in failures:
                print("  - " + f)
            shutil.rmtree(runtime, ignore_errors=True)
            return 1
        for what, path, old, new, checks in MUTATIONS:
            tree = os.path.join(base, "mutant")
            shutil.rmtree(tree, ignore_errors=True)
            shutil.copytree(pristine, tree)
            target = os.path.join(tree, path)
            with open(target, encoding="utf-8") as f:
                text = f.read()
            if old not in text:
                failures.append(f"{what}: the text to mutate is not in {path} any more; update selftest.py")
                continue
            with open(target, "w", encoding="utf-8") as f:
                f.write(text.replace(old, new, 1))
            for how in checks:
                if passes(tree, how):
                    failures.append(f"{what}: {how} did not catch it")
    shutil.rmtree(runtime, ignore_errors=True)
    if failures:
        print("qml selftest: FAIL")
        for f in failures:
            print("  - " + f)
        return 1
    print(f"qml selftest: ok ({len(MUTATIONS)} faults, each caught)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
