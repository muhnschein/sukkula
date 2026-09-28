// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The Receive tab (F-C1, F-C5, F-MW2): ready, with the name others see,
 * round a radar that pulses while nothing comes and takes the room the
 * rest leaves; a transfer coming in, with the sender's name as plain
 * text, what comes in Sukkula's words, its progress and a cross to stop
 * it, staying a moment after it ends; what came today, files that came
 * together as one row ("2 files", "3 photos") that lists them on a page
 * of its own, one file by its name, a text that opens it, a failure not
 * among them; receiving with a code, by scanning its QR code or typing it
 * in, each a page of its own, gone when both code protocols are switched
 * off; and a way that could not start said in red, leading to Settings,
 * which says how each way is doing.
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
            test.compare(test.find("receiveState").text, "Switching on…")
            bridge.emitEvent(Ev.receiving(true))
        },
        function () {
            test.compare(test.find("receiveState").text, "Ready to receive")
            test.compare(test.find("deviceNameLabel").text, "Jolla Phone")
            test.verify(test.view.pulsing, "the rings pulse")
            test.verify(test.find("scanCode").visible, "receiving with a code")
            test.verify(test.find("typeCode").visible, "and by typing it in")
            test.compare(test.received(), [], "nothing came yet")
            test.verify(!test.find("savedIn").visible)
            test.verify(!test.find("receiveFailed").visible, "nothing failed")
            // The radar takes the room the rest leaves: the list's height
            // less the tabs, the words and the rows under it; at least
            // its least, at most the width within the margins.
            var radar = test.find("radar")
            var hero = test.find("receiveHero")
            var words = test.find("heroWords")
            var rest = test.find("receiveRest")
            var widest = test.view.width - 2 * Theme.horizontalPageMargin
            test.compare(test.view.viewHeight, test.main.height, "the list's height, from the page")
            var sizes = [4000, 700, 0]
            for (var i = 0; i < sizes.length; i++) {
                test.view.viewHeight = sizes[i]
                var room = sizes[i] - test.view.topInset - words.height - rest.height - 3 * Theme.paddingLarge
                test.compare(radar.width, Math.max(Theme.itemSizeExtraLarge * 1.6, Math.min(widest, room)),
                             "the radar in " + sizes[i])
                test.compare(hero.height, Math.max(hero.children[0].height + 2 * Theme.paddingLarge,
                                                   sizes[i] - test.view.topInset - rest.height),
                             "the hero fills what is left of " + sizes[i])
            }
            test.view.viewHeight = 4000
            test.compare(radar.width, widest, "room enough: as wide as the margins allow")
            test.view.viewHeight = 0
            test.compare(radar.width, Theme.itemSizeExtraLarge * 1.6, "no room: its least")
            test.view.viewHeight = test.main.height
            // A transfer comes in, after its offer was accepted.
            bridge.emitEvent(Ev.transferStarted(20, "incoming", { peer: Ev.EVIL_MODEL }))
            bridge.emitEvent(Ev.progress(20, 500, 2000))
        },
        function () {
            test.verify(!test.find("receiveHero").visible, "the hero gives way")
            test.verify(!test.view.pulsing, "the rings stop while it comes")
            var rows = test.shown("receiveProgress")
            test.compare(rows.length, 1)
            test.compare(rows[0].title, Ev.EVIL_MODEL, "who, as plain text")
            test.compare(probe.find(rows[0], "progressStatus").text, "Receiving 2 files…")
            test.compare(probe.find(rows[0], "progressPercent").text, "25%")
            test.compare(probe.find(rows[0], "progressDetail").text, "500 B of 2.0 kB")
            test.verifyPlainText(test.main, "a transfer coming in")
            probe.find(rows[0], "cancelTransfer").clicked()
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 20 })
            bridge.emitEvent(Ev.finished(20, "done", [Ev.EVIL_FILE, "b.pdf"]))
        },
        function () {
            var rows = test.shown("receiveProgress")
            test.compare(rows.length, 1, "an ended transfer stays a moment")
            test.compare(probe.find(rows[0], "progressStatus").text, "Saved in Downloads › Sukkula")
            test.compare(test.received(), [], "and is not yet among what came")
            return 200
        },
        function () {
            test.compare(test.shown("receiveProgress").length, 0, "then leaves")
            test.verify(test.find("receiveHero").visible)
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
            var failed = test.shown("receiveProgress")
            test.compare(failed.length, 4, "each ends under Receiving first")
            var statuses = []
            for (var i = 0; i < failed.length; i++) {
                statuses.push(probe.find(failed[i], "progressStatus").text)
            }
            test.verify(statuses.indexOf("Failed: Network error or timeout.") >= 0, statuses.join(" | "))
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
            // A way that failed to start: said in red, leading to Settings.
            bridge.emitEvent(Ev.receiving(true, true))
        },
        function () {
            test.compare(test.find("receiveState").text, "Ready to receive", "Quick Share still sees it")
            var failed = test.find("receiveFailed")
            test.verify(failed.visible, "the failure said")
            var line = probe.find(failed, "receiveFailedLine")
            test.compare(line.text, "Not every device nearby can see this phone. Tap to see why.")
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
            test.compare(test.find("receiveState").text, "Nobody nearby can see this phone")
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
            test.compare(test.find("receiveState").text, "Receiving is switched off in Settings.")
            test.verify(!test.find("deviceNameLabel").visible, "no name to be seen by")
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "everyone", ble_nudge: true },
                                           croc: { enabled: true, relay: null, password: null } }))
        },
        function () {
            test.compare(test.find("receiveState").text, "Ready for codes")
        }
    ]
}
