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
    ("a misspelt PlainText", "qml/pages/HistoryPage.qml",
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
     """X-Share-Methods=files""",
     """X-Share-Methods=files;text""",
     ["static"]),
    ("a sandbox permission too many", "harbour-sukkula.desktop",
     """Permissions=Internet;Bluetooth;Camera;Downloads;Documents;Music;Pictures;Videos;RemovableMedia;MediaIndexing""",
     """Permissions=Internet;Bluetooth;Camera;Downloads;Documents;Music;Pictures;Videos;RemovableMedia;MediaIndexing;UserDirs""",
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
     ["tst_engine.qml", "tst_send.qml"]),
    ("discovery left running on the Receive tab", "qml/pages/MainPage.qml",
     """    readonly property bool wantDiscovery: page.alive && page.engine.running && !page.receiveTab
                                          && !page.engine.receiving && page.awake""",
     """    readonly property bool wantDiscovery: page.alive && page.engine.running
                                          && page.awake""",
     ["tst_main.qml", "tst_stack.qml"]),
    ("the page stack read off a page", "qml/pages/MainPage.qml",
     """        pageStack.push(Qt.resolvedUrl("HistoryPage.qml"), { engine: page.engine })""",
     """        page.pageStack.push(Qt.resolvedUrl("HistoryPage.qml"), { engine: page.engine })""",
     ["static"]),
    ("discovery kept running in the background", "qml/pages/MainPage.qml",
     """                                          && !page.engine.receiving && page.awake""",
     """                                          && !page.engine.receiving""",
     ["tst_app.qml"]),
    ("a main page that turns with the phone", "qml/pages/MainPage.qml",
     """    allowedOrientations: Orientation.Portrait""",
     """    allowedOrientations: Orientation.All""",
     ["tst_main.qml"]),
    ("a share that leaves the phone receiving", "qml/pages/MainPage.qml",
     """            page.sendView.dismiss()
        }
        page.showTab(0)""",
     """            page.sendView.dismiss()
        }""",
     ["tst_app.qml"]),
    ("a tab that does not switch the mode", "qml/pages/MainPage.qml",
     """    onWantReceivingChanged: page.setMode(page.wantReceiving)""",
     """""",
     ["tst_main.qml"]),
    ("a swipe back lost while a switch is on its way", "qml/pages/MainPage.qml",
     """                self.setMode(self.wantReceiving, receive)""",
     """                self.setMode(self.wantReceiving)""",
     ["tst_main.qml"]),
    ("devices listed before anything is chosen", "qml/components/SendView.qml",
     """            model: view.hasPayload ? view.devices : []""",
     """            model: view.devices""",
     ["tst_send.qml"]),
    ("a picked file's URL taken for its path", "qml/pages/Pickers.qml",
     """            return decodeURIComponent(url.substring(7))""",
     """            return url""",
     ["tst_send.qml"]),
    ("a cross that clears nothing", "qml/components/SendView.qml",
     """                view.dismiss()
                view.payload.clear()""",
     """                view.dismiss()""",
     ["tst_send.qml"]),
    ("a failed transfer listed as received", "qml/components/ReceiveView.qml",
     """        if (direction !== "incoming" || state !== "done" || view.ended[transferId] !== undefined) {""",
     """        if (direction !== "incoming" || view.ended[transferId] !== undefined) {""",
     ["tst_receive.qml"]),
    ("an ended transfer that never leaves Receiving", "qml/components/ReceiveView.qml",
     """                if (now - view.ended[id] < view.linger) {""",
     """                if (true) {""",
     ["tst_receive.qml"]),
    ("several files offered to Magic Wormhole", "qml/components/SendView.qml",
     """        if (view.wormholeOn && (view.payload.itemCount === 1 || !view.crocOn)) {""",
     """        if (view.wormholeOn) {""",
     ["tst_send.qml"]),
    ("a peer name drawn by Silica", "qml/components/SendView.qml",
     """                    subtitle: view.deviceLine(modelData)""",
     """                    subtitle: view.deviceLine(modelData)
                    PageHeader { title: modelData.name }""",
     ["tst_send.qml"]),
    ("a second send while one is on its way", "qml/components/SendView.qml",
     """        if (view.hasOutgoing && !view.outgoingEnded) {
            return
        }
        if (view.sending || !device || device.peers.length === 0 || !view.hasPayload) {""",
     """        if (!device || device.peers.length === 0 || !view.hasPayload) {""",
     ["tst_send.qml"]),
    ("a scanned croc code sent as a wormhole one", "qml/pages/ScanPage.qml",
     """            if (result.protocol === "croc") {""",
     """            if (false) {""",
     ["tst_scan.qml"]),
    ("a typed code sent to one protocol", "qml/pages/TypeCodePage.qml",
     """            page.engine.receiveCode(text, reply)""",
     """            page.engine.receiveWormhole(text, reply)""",
     ["tst_scan.qml", "tst_stack.qml"]),
    ("the clipboard pasted whatever it holds", "qml/pages/TypeCodePage.qml",
     """        if (typeof clip === "string" && page.looksLikeCode(clip)) {""",
     """        if (typeof clip === "string") {""",
     ["tst_scan.qml"]),
    ("croc's QR code dropped", "qml/engine/Engine.qml",
     """        engine._codes[id] = { code: code, qr: engine._qr(e.qr) }""",
     """        engine._codes[id] = { code: code, qr: e.type === "croc_code" ? null : engine._qr(e.qr) }""",
     ["tst_engine.qml", "tst_send.qml"]),
    ("a scanned code received with its protocol switched off", "qml/pages/ScanPage.qml",
     """        if (!page.engine.protocolEnabled(result.protocol)) {""",
     """        if (false) {""",
     ["tst_scan.qml"]),
    ("a scanned mailbox dropped", "qml/pages/ScanPage.qml",
     """                page.engine.receiveWormhole(result.code, reply, result.mailboxUrl)""",
     """                page.engine.receiveWormhole(result.code, reply)""",
     ["tst_scan.qml"]),
    ("the camera left on in the background", "qml/pages/ScanPage.qml",
     """    readonly property bool cameraOn: page.status === PageStatus.Active && page.foreground
                                     && !receiver.busy && !receiver.leaving""",
     """    readonly property bool cameraOn: page.status === PageStatus.Active
                                     && !receiver.busy && !receiver.leaving""",
     ["tst_scan.qml"]),
    ("frames scanned with no camera", "qml/components/ScanView.qml",
     """        running: view.looking && view.cameraReady && view.scanner !== null""",
     """        running: view.active && view.scanner !== null""",
     ["tst_scan.qml"]),
    ("a missing camera not noticed", "qml/components/ScanView.qml",
     """    readonly property bool cameraMissing: camera.availability !== Camera.Available""",
     """    readonly property bool cameraMissing: false""",
     ["tst_scan.qml"]),
    ("the camera never asked to focus", "qml/components/ScanView.qml",
     """        running: view.looking && view.cameraReady
        onTriggered: view.refocus()""",
     """        running: false
        onTriggered: view.refocus()""",
     ["tst_scan.qml"]),
    ("a QR code of anything else taken as a code", "qml/components/ScanView.qml",
     """        if (result.protocol === "") {
            view.sawOther = true""",
     """        if (false) {
            view.sawOther = true""",
     ["tst_scan.qml"]),
    ("a scanned code taken unchecked", "qml/engine/Engine.qml",
     """                || v.code.length < 6 || v.code.length > 128 || !printable.test(v.code)) {""",
     """                ) {""",
     ["tst_engine.qml"]),
    ("the camera imported outside the scan page", "qml/pages/MainPage.qml",
     """import Sailfish.Silica 1.0""",
     """import Sailfish.Silica 1.0
import QtMultimedia 5.6""",
     ["static"]),
    ("the Camera permission dropped", "harbour-sukkula.desktop",
     """Permissions=Internet;Bluetooth;Camera;Downloads;""",
     """Permissions=Internet;Bluetooth;Downloads;""",
     ["static"]),
    ("a croc relay with a scheme let through", "qml/pages/SettingsPage.qml",
     """/^[A-Za-z0-9.-]+(?::([0-9]{1,5}))?$/""",
     """/^[A-Za-z0-9.:\\/-]+(?::([0-9]{1,5}))?$/""",
     ["tst_settings.qml"]),
    ("the tabs' words each in the middle of its half", "qml/components/ModeTabs.qml",
     """                    x: button.first ? button.width - label.width - Theme.paddingMedium
                       : button.last ? Theme.paddingMedium
                       : (button.width - label.width) / 2""",
     """                    x: (button.width - label.width) / 2""",
     ["tst_main.qml"]),
    ("rows drawn behind the tabs", "qml/pages/MainPage.qml",
     """        clip: page.yOffset > 0""",
     """        clip: false""",
     ["tst_main.qml"]),
    ("the tab on screen, tapped, left scrolled", "qml/pages/MainPage.qml",
     """                pager.currentItem.item.scrollToTop()""",
     """                pager.currentItem.item.contentY = pager.currentItem.item.contentY""",
     ["tst_main.qml"]),
    ("the radar held to the canvas's size", "qml/components/ReceiveView.qml",
     """Math.min(view.radarBase * 1.35, view.radarRoom / 1.15)))""",
     """view.radarBase))""",
     ["tst_receive.qml"]),
    ("the radar's pulses going out together", "qml/components/ReceiveView.qml",
     """view.easeOut((radar.phase + index / 3) % 1)""",
     """view.easeOut(radar.phase % 1)""",
     ["tst_receive.qml"]),
    ("the radar's pulses not eased", "qml/components/ReceiveView.qml",
     """        return 3 * m * m - 2 * m * m * m""",
     """        return t""",
     ["tst_receive.qml"]),
    ("the radar's pulses filled", "qml/components/ReceiveView.qml",
     """                            color: "transparent"
                            border.width: 1.5 * radar.line""",
     """                            color: Theme.highlightColor
                            border.width: 1.5 * radar.line""",
     ["tst_receive.qml"]),
    ("no plain phone in the radar", "qml/components/ReceiveView.qml",
     """                        objectName: "radarGlyph"
                        anchors.centerIn: parent
                        kind: "phone""",
     """                        objectName: "radarGlyph"
                        anchors.centerIn: parent
                        kind: "file""",
     ["tst_receive.qml"]),
    ("the QR code icon asked of the theme", "qml/components/Glyph.qml",
     """        case "qr": return []""",
     """        case "qr": return ["icon-m-camera"]""",
     ["tst_receive.qml"]),
    ("piirit's QR code not drawn", "qml/components/Glyph.qml",
     """        visible: glyph.kind === "qr""",
     """        visible: false""",
     ["tst_receive.qml"]),
    ("one picker per kind again", "qml/pages/Pickers.qml",
     """        return kind === "files" ? pickers.files : pickers.content""",
     """        return pickers.files""",
     ["tst_send.qml"]),
    ("+ not in the picker used last", "qml/components/SendView.qml",
     """                onClicked: view.pick(view.lastPicker)""",
     """                onClicked: view.pick("content")""",
     ["tst_send.qml"]),
    ("a way that could not start, not said on the Receive tab", "qml/components/ReceiveView.qml",
     """    readonly property bool someFailed: view.receiving && view.nearbyEnabled && view.anyFailed""",
     """    readonly property bool someFailed: false""",
     ["tst_receive.qml", "tst_main.qml"]),
    ("typing a code with no page of its own", "qml/components/ReceiveView.qml",
     """        pageStack.push(Qt.resolvedUrl("../pages/TypeCodePage.qml"), { engine: view.engine })""",
     """        pageStack.push(Qt.resolvedUrl("../pages/ScanPage.qml"), { engine: view.engine })""",
     ["tst_receive.qml"]),
    ("no way to type a code without a camera", "qml/pages/ScanPage.qml",
     """            visible: page.noCamera && !receiver.busy""",
     """            visible: false""",
     ["tst_scan.qml"]),
    ("a way that failed shown as nothing in Settings", "qml/pages/SettingsPage.qml",
     """        return on && page.engine.receiving && status && status.state === "failed" ? [status] : []""",
     """        return []""",
     ["tst_settings.qml"]),
    ("a ready way shown as nothing in Settings", "qml/pages/SettingsPage.qml",
     """            return qsTr("Ready to receive")""",
     """            return """"",
     ["tst_settings.qml"]),
    ("empty server fields that all say the same", "qml/pages/SettingsPage.qml",
     """                placeholderText: qsTr("Magic Wormhole transit relay")""",
     """                placeholderText: qsTr("Magic Wormhole mailbox server")""",
     ["tst_settings.qml"]),
    ("a code's reply that pops whatever is on top", "qml/components/CodeReceiver.qml",
     """        if (pageStack.currentPage !== receiver.host || receiver.host.status !== PageStatus.Active) {
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
    # Spec v0.7: the redesigned tabs, the code page, the cover.
    ("receiving kept on in the background", "qml/pages/MainPage.qml",
     """                                          && (page.awake || page.engine.offers.count > 0 || page.incoming)""",
     """                                          && true""",
     ["tst_main.qml"]),
    ("a waiting offer dropped when the app goes to the background", "qml/pages/MainPage.qml",
     """                                          && (page.awake || page.engine.offers.count > 0 || page.incoming)""",
     """                                          && page.awake""",
     ["tst_main.qml"]),
    ("a code left running when its page is left unused", "qml/pages/SendCodePage.qml",
     """        if (page.view && page.waiting) {""",
     """        if (false) {""",
     ["tst_send.qml"]),
    ("a peer's certificate shown unchecked", "qml/engine/Engine.qml",
     """        return typeof v === "string" && /^[0-9A-F]{64}$/.test(v) ? v : \"\"""",
     """        return typeof v === "string" ? v : \"\"""",
     ["tst_engine.qml"]),
    ("the MediaIndexing permission dropped", "harbour-sukkula.desktop",
     """;RemovableMedia;MediaIndexing\n""",
     """;RemovableMedia\n""",
     ["static"]),
    ("a peer's words on the cover", "qml/cover/CoverPage.qml",
     """                return cover.engine.countWords(cover.engine.commonKind(names, t.fileCount), t.fileCount)""",
     """                return t.peer""",
     ["tst_main.qml"]),
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
