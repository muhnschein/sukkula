// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The Receive tab (F-C1, F-C5, F-MW2), round the anchor: ready, with the
 * name others see, the anchor pulsing while nothing comes, at a fifth of
 * the page's height as on the Send tab; a transfer coming in, the anchor
 * waiting, then filling with the percentage, the sender's name as plain
 * text, what comes in Sukkula's words and Cancel; then a check and where
 * it went, or a cross and why, for a moment; several at once, the others
 * under "Receiving"; what came today, files that came together as one
 * row ("2 files", "3 photos") that lists them on a page of its own, one
 * file by its name, a text that opens it, a failure not among them;
 * receiving with a code, by scanning its QR code or typing it in, each a
 * page of its own, gone when both code protocols are switched off; and a
 * way that could not start said in red, leading to Settings, which says
 * how each way is doing.
 */
Script {
    id: test

    property Item main: null
    property Item view: null
    property Item page: null

    ApplicationWindow {
        id: window
    }

    Engine {
        id: engine
        backend: bridge
    }

    function find(name) {
        return probe.find(test.view, name)
    }

    /// A label of the hero, and its anchor.
    function says(name) {
        return probe.find(test.find("receiveHero"), name).text
    }
    function anchor() {
        return probe.find(test.find("receiveHero"), "anchor")
    }

    function shown(name) {
        var out = []
        var all = probe.findAll(test.view, name)
        for (var i = 0; i < all.length; i++) {
            if (all[i].visible) {
                out.push(all[i])
            }
        }
        return out
    }

    /// The rows under "Received today": [title, subtitle, glyph] each.
    function received() {
        var out = []
        var all = test.shown("receivedRow")
        for (var i = 0; i < all.length; i++) {
            out.push([all[i].title, all[i].subtitle, all[i].glyph])
        }
        return out
    }

    function receivedRow(title) {
        var all = test.shown("receivedRow")
        for (var i = 0; i < all.length; i++) {
            if (all[i].title === title) {
                return all[i]
            }
        }
        return null
    }

    function lastOf(type) {
        var cmds = bridge.parsedCommands()
        for (var i = cmds.length - 1; i >= 0; i--) {
            if (cmds[i].cmd.type === type) {
                return cmds[i].cmd
            }
        }
        return null
    }

    steps: [
        function () {
            test.main = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.view = probe.find(test.main, "receiveView")
            test.view.linger = 60
            probe.find(test.main, "modeTabs").tabClicked(1)
        },
        function () {
            test.compare(probe.find(test.main, "modePager").currentIndex, 1, "the Receive tab")
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: true })
            test.compare(test.says("heroTitle"), "Switching on…")
            bridge.emitEvent(Ev.receiving(true))
        },
        function () {
            test.compare(test.says("heroTitle"), "Ready to receive")
            test.compare(test.says("heroSubtitle"), "Others nearby see you as")
            test.compare(test.says("heroLine"), "Jolla Phone")
            test.verify(test.view.pulsing, "the rings pulse")
            test.compare(test.anchor().mode, "ready")
            test.compare(probe.findAll(test.anchor(), "pulse").length, 3)
            test.verify(probe.findAll(test.anchor(), "pulse")[0].visible)
            test.verify(test.find("scanCode").visible, "receiving with a code")
            test.verify(test.find("typeCode").visible, "and by typing it in")
            // The QR code is piirit's, drawn: the theme has none.
            var qr = probe.find(test.find("scanCode"), "rowGlyph")
            test.compare(qr.kind, "qr")
            test.compare(String(qr.source), "", "no theme icon asked for")
            var mark = probe.find(qr, "qrMark")
            test.verify(mark.visible, "the drawing shows")
            test.compare(probe.findAll(mark, "qrFinder").length, 3, "three finder patterns")
            test.compare(probe.findAll(mark, "qrModule").length, 5, "and five modules")
            var phone = probe.find(test.anchor(), "anchorGlyph")
            test.compare(phone.names, ["icon-m-device", "icon-m-phone"], "a plain phone in the disc")
            test.verify(!probe.find(phone, "qrMark").visible, "only for a QR code")
            test.verify(!probe.find(test.find("scanCode"), "rowSubtitle").visible
                        && !probe.find(test.find("typeCode"), "rowSubtitle").visible, "no line under either")
            test.compare(test.received(), [], "nothing came yet")
            test.verify(!test.find("savedIn").visible)
            test.verify(!test.find("receiveFailed").visible, "nothing failed")
            // The anchor as the canvas draws it, at a fifth of the page's
            // height, as on the Send tab, with the pulses (out to 1.15
            // times it) within the margins.
            var hero = test.find("receiveHero")
            var a = test.anchor()
            test.compare(test.view.viewHeight, test.main.height, "the list's height, from the page")
            test.compare(hero.anchorTop, Math.max(Theme.paddingLarge,
                                                  Math.round(test.main.height * 0.2) - test.view.topInset))
            test.compare(hero.anchorTop, probe.find(test.main, "sendHero").anchorTop, "where the Send tab has it")
            test.compare(a.width, Math.min(Theme.iconSizeMedium * 1.9 / 0.432,
                                           (test.view.width - 2 * Theme.horizontalPageMargin) / 1.15))
            test.compare(probe.find(a, "anchorDisc").width, a.width * 0.432, "the disc, as on the canvas")
            // Three pulses, a third of the 3 s cycle apart, each going out
            // from the disc to past the outer ring and fading as it goes,
            // eased as CSS's ease-out.
            test.verify(Math.abs(a.easeOut(0.5) - 0.6846) < 1e-3, "ease-out: " + a.easeOut(0.5))
            test.verify(Math.abs(a.easeOut(0)) < 1e-6 && Math.abs(a.easeOut(1) - 1) < 1e-6)
            var pulses = probe.findAll(a, "pulse")
            a.phase = 0
            var near = function (x, y) { return Math.abs(x - y) < 1e-3 }
            test.verify(near(pulses[0].scale, 0.45) && near(pulses[0].opacity, 0.9), "the first, at the disc")
            test.verify(near(pulses[1].scale, 0.45 + 0.7 * a.easeOut(1 / 3))
                        && near(pulses[2].scale, 0.45 + 0.7 * a.easeOut(2 / 3)), "the others, a third on each")
            a.phase = 0.9999
            test.verify(near(pulses[0].scale, 1.15) && near(pulses[0].opacity, 0), "past the outer ring, faded")
            test.verify(String(pulses[0].border.color) === String(Theme.highlightColor)
                        && pulses[0].color.a === 0, "a ring, not a disc")
            // A transfer comes in, after its offer was accepted.
            bridge.emitEvent(Ev.transferStarted(20, "incoming", { peer: Ev.EVIL_MODEL }))
        },
        function () {
            test.compare(test.anchor().mode, "waiting", "the arc goes round until the bytes come")
            test.compare(test.says("heroTitle"), Ev.EVIL_MODEL, "who, as plain text")
            test.compare(test.says("heroSubtitle"), "Receiving 2 files…")
            test.verify(!test.view.pulsing, "the rings stop while it comes")
            bridge.emitEvent(Ev.progress(20, 500, 2000))
        },
        function () {
            test.compare(test.anchor().mode, "progress", "the ring fills, in the same place")
            test.compare(probe.find(test.anchor(), "anchorPercent").text, "25%")
            test.compare(test.says("heroSubtitle"), "500 B of 2.0 kB")
            test.compare(test.says("heroLine"), "2 files")
            test.compare(test.shown("receiveProgress").length, 0, "the hero shows it, not a row")
            test.verifyPlainText(test.main, "a transfer coming in")
            test.find("cancelReceive").clicked()
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 20 })
            bridge.emitEvent(Ev.finished(20, "done", [Ev.EVIL_FILE, "b.pdf"]))
        },
        function () {
            test.compare(test.anchor().mode, "done", "an ended transfer stays a moment")
            test.compare(probe.find(test.anchor(), "anchorGlyph").kind, "check")
            test.compare(test.says("heroTitle"), "Received")
            test.compare(test.says("heroSubtitle"), "Saved in Downloads › Sukkula")
            test.compare(test.says("heroLine"), "2 files from " + Ev.EVIL_MODEL)
            test.verify(!test.find("cancelReceive").visible)
            test.compare(test.received(), [], "and is not yet among what came")
            return 200
        },
        function () {
            test.compare(test.says("heroTitle"), "Ready to receive", "then back to ready")
            test.compare(test.received(), [["2 files", "From " + Ev.EVIL_MODEL + " · 2.0 kB", "file"]],
                         "files that came together: one row")
            test.verify(test.find("savedIn").visible, "and where they are")
            test.verifyPlainText(test.main, "what came today")
            test.receivedRow("2 files").clicked()
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "receivedPage")
        },
        function () {
            test.compare(probe.find(test.page, "pageHeaderTitle").text, "2 files", "Sukkula's words in the header")
            test.compare(probe.find(test.page, "receivedFrom").text, "From " + Ev.EVIL_MODEL + " · 2.0 kB")
            var names = []
            var all = probe.findAll(test.page, "receivedName")
            for (var i = 0; i < all.length; i++) {
                names.push(all[i].text)
            }
            test.compare(names, [Ev.EVIL_FILE, "b.pdf"], "each by the name it was saved under")
            test.verifyPlainText(test.page, "the files that came together")
            window.pageStack.pop()
            // One photo; three photos; a failure; a text.
            bridge.emitEvent(Ev.transferStarted(21, "incoming", { peer: "Pixel", file_count: 1, total_bytes: 3000,
                                                                  files: [{ name: "x.jpg", size: 3000 }] }))
            bridge.emitEvent(Ev.finished(21, "done", ["x.jpg"]))
            bridge.emitEvent(Ev.transferStarted(22, "incoming", { peer: "Pixel", file_count: 3, total_bytes: 3000,
                                                                  files: [{ name: "a.jpg", size: 1000 },
                                                                          { name: "b.jpg", size: 1000 },
                                                                          { name: "c.heic", size: 1000 }] }))
            bridge.emitEvent(Ev.finished(22, "done", ["a.jpg", "b (1).jpg", "c.heic"]))
            bridge.emitEvent(Ev.transferStarted(23, "incoming", { peer: "Pixel" }))
            bridge.emitEvent(Ev.finished(23, "failed", null, "network"))
            bridge.emitEvent(Ev.transferStarted(24, "incoming", { peer: "Pixel", file_count: 0, total_bytes: 11,
                                                                  files: [] }))
            bridge.emitEvent(Ev.textReceived(24, "hello there"))
            bridge.emitEvent(Ev.finished(24, "done"))
        },
        function () {
            // The newest in the hero, the others under Receiving.
            test.compare(test.says("heroTitle"), "Received")
            test.compare(test.says("heroSubtitle"), "Saved in History", "a text")
            test.compare(test.says("heroLine"), "Text message from Pixel")
            var others = test.shown("receiveProgress")
            test.compare(others.length, 3, "each ends under Receiving first")
            var statuses = []
            for (var i = 0; i < others.length; i++) {
                statuses.push(probe.find(others[i], "progressStatus").text)
            }
            test.verify(statuses.indexOf("Failed: The connection failed or timed out.") >= 0, statuses.join(" | "))
            return 200
        },
        function () {
            test.compare(test.received(), [["Text message", "From Pixel · 11 B", "text"],
                                           ["3 photos", "From Pixel · 3.0 kB", "photo"],
                                           ["x.jpg", "From Pixel · 3.0 kB", "photo"],
                                           ["2 files", "From " + Ev.EVIL_MODEL + " · 2.0 kB", "file"]],
                         "newest first; one file by its name; the failure not among them")
            // One file has no page of its own.
            test.receivedRow("x.jpg").clicked()
            test.compare(window.pageStack.currentPage.objectName, "mainPage")
            // A text opens as text.
            test.receivedRow("Text message").clicked()
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "textPage")
            window.pageStack.pop()
            test.receivedRow("3 photos").clicked()
            test.page = window.pageStack.currentPage
            test.compare(probe.find(test.page, "pageHeaderTitle").text, "3 photos")
            window.pageStack.pop()
            // Scan a QR code: the camera, for either protocol's code.
            test.find("scanCode").clicked()
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "scanPage")
            window.pageStack.pop()
            // Type in a code: a page of its own.
            test.find("typeCode").clicked()
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "typeCodePage")
            window.pageStack.pop()
            // One that fails: a cross, and why, in red.
            bridge.emitEvent(Ev.transferStarted(25, "incoming", { peer: "Pixel" }))
            bridge.emitEvent(Ev.finished(25, "failed", null, "storage"))
        },
        function () {
            test.compare(test.anchor().mode, "failed")
            test.compare(probe.find(test.anchor(), "anchorGlyph").kind, "cross")
            test.compare(test.says("heroTitle"), "Not received")
            test.compare(test.says("heroSubtitle"), "Could not save. The storage may be full.")
            test.verify(test.find("receiveHero").failed, "in red")
            return 200
        },
        function () {
            test.compare(test.says("heroTitle"), "Ready to receive")
            // A way that failed to start: said in red, leading to Settings.
            bridge.emitEvent(Ev.receiving(true, true))
        },
        function () {
            test.compare(test.says("heroTitle"), "Ready to receive", "Quick Share still sees it")
            var failed = test.find("receiveFailed")
            test.verify(failed.visible, "the failure said")
            var line = probe.find(failed, "receiveFailedLine")
            test.compare(line.text, "Some devices nearby cannot see you. Tap to see why.")
            test.verify(line.color === Theme.errorColor, "in red")
            test.verify(probe.texts(test.view).join("\n").indexOf("port 53317 in use") < 0,
                        "the engine's English is for Settings")
            failed.clicked()
            test.compare(window.pageStack.currentPage.objectName, "settingsPage", "why is in Settings")
            window.pageStack.pop()
            // Both nearby ways failed: nobody can see this phone.
            bridge.emitEvent(JSON.stringify({ type: "receiving", on: true, protocols: [
                { protocol: "local_send", state: "failed", error: { code: "network", message: "x" } },
                { protocol: "quick_share", state: "failed", error: { code: "network", message: "y" } }] }))
        },
        function () {
            test.compare(test.says("heroTitle"), "Others nearby cannot see you")
            test.compare(probe.find(test.view, "receiveFailedLine").text, "Tap to see why.")
            test.verify(!test.view.pulsing, "no pulse for nobody")
            bridge.emitEvent(Ev.receiving(true))
            // Magic Wormhole off: croc still receives with a code.
            bridge.emitEvent(Ev.settings({ wormhole: { enabled: false, mailbox_url: null, relay_url: null } }))
        },
        function () {
            test.verify(!test.find("receiveFailed").visible, "all up again")
            test.verify(test.find("scanCode").visible && test.find("typeCode").visible,
                        "croc still receives with a code")
            // And croc: no code at all.
            bridge.emitEvent(Ev.settings({ wormhole: { enabled: false, mailbox_url: null, relay_url: null },
                                           croc: { enabled: false, relay: null, password: null } }))
        },
        function () {
            test.verify(!test.find("scanCode").visible && !test.find("typeCode").visible,
                        "nothing to receive a code over")
            // Nearby switched off as well: nothing, then only codes.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "everyone", ble_nudge: true },
                                           wormhole: { enabled: false, mailbox_url: null, relay_url: null },
                                           croc: { enabled: false, relay: null, password: null } }))
        },
        function () {
            test.compare(test.says("heroTitle"), "Receiving is switched off in Settings.")
            test.compare(test.says("heroLine"), "", "no name to be seen by")
            test.compare(test.says("heroSubtitle"), "")
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "everyone", ble_nudge: true },
                                           croc: { enabled: true, relay: null, password: null } }))
        },
        function () {
            test.compare(test.says("heroTitle"), "Ready to receive codes")
        }
    ]
}
