// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The Receive tab (F-C1, F-C5, F-MW2): ready, with the name others see,
 * and rings that pulse while nothing comes; a transfer coming in, with
 * the sender's name as plain text, what comes in Sukkula's words, its
 * progress and a cross to stop it, staying a moment after it ends; what
 * came today, files that came together as one row ("2 files", "3
 * photos") that lists them on a page of its own, one file by its name, a
 * text that opens it, a failure not among them; receiving with a code,
 * gone when both code protocols are switched off; and how others can
 * reach this phone, unfolded, one row per way, each with its state and
 * its settings in a word, a failure in red.
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

    /// How others can reach this phone, unfolded: the rows' lines.
    function reach() {
        var out = []
        var all = test.shown("reachLine")
        for (var i = 0; i < all.length; i++) {
            out.push(all[i].text)
        }
        return out
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
            test.compare(test.received(), [], "nothing came yet")
            test.verify(!test.find("savedIn").visible)
            // How others can reach this phone, folded away.
            test.compare(test.reach(), [])
            test.find("reachToggle").clicked()
            test.compare(test.reach(), ["Ready · Quick Share, visible to everyone", "Ready · LocalSend, no PIN",
                                        "Ready · Magic Wormhole and croc", "In the phone's own Bluetooth settings"])
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
            // Scan a code: the camera, for either protocol's code.
            test.find("scanCode").clicked()
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "scanPage")
            window.pageStack.pop()
            // A way that failed to start, in red; Quick Share hidden; a PIN.
            bridge.emitEvent(Ev.receiving(true, true))
            bridge.emitEvent(Ev.settings({ localsend: { enabled: true, pin: "1234" },
                                           quickshare: { enabled: true, visibility: "hidden", ble_nudge: true } }))
        },
        function () {
            test.compare(test.reach(), ["Ready · Quick Share, hidden",
                                        "LocalSend could not start: Network error or timeout.",
                                        "Ready · Magic Wormhole and croc", "In the phone's own Bluetooth settings"])
            test.verify(test.shown("reachLine")[1].color === Theme.errorColor, "the failure in red")
            test.compare(test.find("receiveState").text, "Ready to receive", "Quick Share still sees it")
            // A row takes one to Settings.
            test.shown("reachRow")[0].clicked()
            test.compare(window.pageStack.currentPage.objectName, "settingsPage")
            window.pageStack.pop()
            // Magic Wormhole off: croc still receives with a code.
            bridge.emitEvent(Ev.settings({ wormhole: { enabled: false, mailbox_url: null, relay_url: null } }))
        },
        function () {
            test.verify(test.find("scanCode").visible, "croc still receives with a code")
            test.compare(test.reach()[2], "Ready · croc")
            // And croc: no code at all.
            bridge.emitEvent(Ev.settings({ wormhole: { enabled: false, mailbox_url: null, relay_url: null },
                                           croc: { enabled: false, relay: null, password: null } }))
        },
        function () {
            test.verify(!test.find("scanCode").visible, "nothing to receive a code over")
            test.compare(test.reach()[2], "Off")
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
