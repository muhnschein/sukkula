// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "../../qml/cover"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The main page, the History page, the received-text page and the cover:
 * Send | Receive as tabs that are the engine's state (F-C1) -- discovery
 * on the Send tab only, a tab swiped back to mid-switch switched back to,
 * a refused switch taking the tab back, the page in portrait, and what
 * receiving is doing on the Receive tab; every transfer with progress and
 * cancel on the History page (F-C5), received texts there as plain text
 * with a Copy button and nothing that opens them (F-C4); About reached
 * from Settings, not from a pulley; and a cover that says whether Sukkula
 * is receiving without naming anyone.
 */
Script {
    id: test

    property Item main: null
    property Item history: null

    ApplicationWindow {
        id: window
    }

    Engine {
        id: engine
        backend: bridge
    }

    CoverPage {
        id: cover
        engine: engine
        width: 234
        height: 374
    }

    function lastCmd() {
        var c = bridge.lastCommand()
        return c ? c.cmd : null
    }

    function count(type) {
        var n = 0
        var cmds = bridge.parsedCommands()
        for (var i = 0; i < cmds.length; i++) {
            if (cmds[i].cmd.type === type) {
                n++
            }
        }
        return n
    }

    /// Answers the last set_receiving.
    function answerLast(ok, code) {
        var cmds = bridge.parsedCommands()
        for (var i = cmds.length - 1; i >= 0; i--) {
            if (cmds[i].cmd.type === "set_receiving") {
                bridge.emitEvent(Ev.reply(cmds[i].id, ok, code))
                return
            }
        }
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

    function textsOf(root) {
        return probe.texts(root).join("\n")
    }

    steps: [
        function () {
            test.main = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.verify(test.main !== null, "the main page loads")
        },
        function () {
            var tabs = probe.find(test.main, "modeTabs")
            var pager = probe.find(test.main, "modePager")
            test.compare(tabs.currentIndex, 0, "the Send tab first: the engine starts not receiving")
            test.compare(pager.currentIndex, 0)
            test.verify(probe.find(test.main, "sendView").current, "the send radar")
            test.verify(!probe.find(test.main, "receiveView").current)
            test.compare(test.main.allowedOrientations, Orientation.Portrait, "in portrait only")
            test.compare(test.count("start_discovery"), 1, "Send mode looks for peers")
            test.compare(test.count("list_bluetooth_devices"), 1, "and lists the paired devices")
            test.verify(test.textsOf(cover).indexOf("Not receiving") >= 0, "the cover says off")
            test.verify(test.textsOf(test.main).indexOf("About Sukkula") < 0, "About is not in a pulley")
            // F-C1: one tap, one command.
            bridge.autoReply = false
            probe.find(test.main, "modeSend").clicked()
            test.compare(test.count("set_receiving"), 0, "the tab shown is no change")
            probe.find(test.main, "modeReceive").clicked()
            test.compare(test.lastCmd(), { type: "set_receiving", on: true })
            test.compare(pager.currentIndex, 1, "the tab moves at once")
            test.compare(tabs.busyIndex, 1, "busy until the reply")
            probe.find(test.main, "modeReceive").clicked()
            test.compare(test.count("set_receiving"), 1, "one set_receiving while busy")
            // Back to Send before the engine has answered: switched back
            // once it has.
            pager.moveTo(0)
            test.compare(test.count("set_receiving"), 1, "nothing more while busy")
            test.answerLast(true)
        },
        function () {
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: false }, "then back")
            bridge.emitEvent(Ev.receiving(true, true))
        },
        function () {
            var pager = probe.find(test.main, "modePager")
            test.compare(pager.currentIndex, 0, "a late event does not move the tab mid-switch")
            test.answerLast(true)
            bridge.emitEvent(Ev.receiving(false))
        },
        function () {
            var pager = probe.find(test.main, "modePager")
            test.compare(pager.currentIndex, 0)
            test.compare(engine.receiving, false)
            test.compare(probe.find(test.main, "modeTabs").busyIndex, -1)
            // A swipe does what a tap does.
            pager.moveTo(1)
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: true })
            test.answerLast(true)
            bridge.emitEvent(Ev.receiving(true, true))
        },
        function () {
            var tabs = probe.find(test.main, "modeTabs")
            test.compare(tabs.currentIndex, 1)
            test.compare(tabs.busyIndex, -1)
            var receive = probe.find(test.main, "receiveView")
            test.verify(receive.current, "the receive radar")
            test.verify(receive.pulsing, "pulsing: this phone can be seen")
            test.compare(test.main.allowedOrientations, Orientation.Portrait, "still portrait")
            test.compare(probe.find(receive, "deviceNameLabel").text, "Jolla Phone", "the name others see")
            test.compare(probe.find(receive, "receiveState").text,
                         "Waiting for offers. Nothing is saved until you accept it.")
            test.compare(probe.find(receive, "visibleVia").text, "Visible over Quick Share")
            test.compare(test.count("stop_discovery"), 2, "no discovery in Receive mode (the late event stopped it too)")
            test.compare(engine.discoveryUsers, 0)
            var all = test.textsOf(receive)
            test.verify(all.indexOf("LocalSend could not start: Network error or timeout.") >= 0,
                        "a failed protocol says so, by code: " + all)
            test.verify(all.indexOf("port 53317 in use") < 0, "the engine's English stays off the radar")
            test.verify(test.textsOf(cover).indexOf("Receiving") >= 0, "the cover says on")
            // A refused switch is reported, and the tab goes back.
            probe.find(test.main, "modeSend").clicked()
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: false })
            test.answerLast(false, "unavailable")
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text, "Not available. Is it switched off in Settings?")
            test.compare(probe.find(test.main, "modeTabs").currentIndex, 1, "the tab goes back")
            test.compare(engine.receiving, true, "still receiving")
            test.compare(test.count("start_discovery"), 2, "and not looking")
            bridge.autoReply = true
            // The cover's action: the tab follows the engine.
            bridge.emitEvent(Ev.receiving(false))
        },
        function () {
            test.compare(probe.find(test.main, "modeTabs").currentIndex, 0, "the tab follows the engine")
            test.compare(test.count("set_receiving"), 4, "and sends nothing of its own")
            bridge.emitEvent(Ev.receiving(true))
        },
        function () {
            test.compare(probe.find(test.main, "modeTabs").currentIndex, 1)
            // History, from the pulley: empty at first.
            var items = probe.findAll(test.main, "openHistory")
            test.compare(items.length, 2, "on both tabs")
            items[1].clicked()
            test.history = window.pageStack.currentPage
            test.compare(test.history.objectName, "historyPage")
            test.verify(probe.find(test.history, "emptyHint").visible, "nothing yet")
            test.compare(probe.find(test.history, "emptyHint").text, "Nothing sent or received yet.")
            test.verify(!test.history.clearable, "nothing to clear")
            // Transfers (F-C5).
            bridge.emitEvent(Ev.transferStarted(1, "incoming", {}))
            bridge.emitEvent(Ev.progress(1, 500, 2000))
            bridge.emitEvent(Ev.transferStarted(2, "outgoing", { peer: "", file_count: 0, files: [] }))
            bridge.emitEvent(Ev.finished(2, "done"))
            bridge.emitEvent(Ev.transferStarted(3, "incoming", { file_count: 1, files: [{ name: "x", size: 1 }] }))
            bridge.emitEvent(Ev.finished(3, "failed", null, "storage"))
            bridge.emitEvent(Ev.transferStarted(4, "incoming", {}))
            bridge.emitEvent(Ev.finished(4, "done", [Ev.EVIL_FILE, "b.pdf"]))
        },
        function () {
            test.verify(!probe.find(test.history, "emptyHint").visible)
            var peers = probe.findAll(test.history, "transferPeer")
            test.compare(peers.length, 4)
            test.compare(peers[3].text, "↓ " + Ev.EVIL_NAME, "the peer's name, plain")
            test.compare(peers[2].text, "↑ LocalSend", "no name: the protocol")
            var files = probe.findAll(test.history, "transferFiles")
            test.compare(files[3].text, Ev.EVIL_FILE + " and 1 more", "the first file and a count")
            test.compare(files[2].text, "Text")
            var status = probe.findAll(test.history, "transferStatus")
            test.compare(status[3].text, "500 B of 2.0 kB")
            test.compare(status[2].text, "Sent")
            test.compare(status[1].text, "Failed: Could not save. Is the storage full?")
            test.compare(status[0].text, "Saved in Downloads/Sukkula: " + Ev.EVIL_FILE + ", b.pdf")
            test.verify(test.textsOf(cover).indexOf("1 transfer, 25%") >= 0,
                        "the cover counts running transfers: " + test.textsOf(cover))
            test.verifyPlainText(test.history, "the history")
            // Cancel is on the running one only.
            var buttons = probe.findAll(test.history, "cancelButton")
            var shown = 0
            for (var i = 0; i < buttons.length; i++) {
                if (buttons[i].visible) {
                    shown++
                    buttons[i].clicked()
                }
            }
            test.compare(shown, 1)
            test.compare(test.lastCmd(), { type: "cancel", transfer: 1 })
            // Texts (F-C4).
            bridge.emitEvent(Ev.textReceived(5))
        },
        function () {
            var body = probe.find(test.history, "textBody")
            test.compare(body.text, Ev.EVIL_TEXT, "the text as sent, markup and all")
            test.compare(body.textFormat, Text.PlainText)
            test.compare(probe.find(test.history, "textFrom").text, Ev.EVIL_NAME)
            probe.find(test.history, "copyButton").clicked()
            test.compare(Clipboard.text, Ev.EVIL_TEXT, "Copy puts the text on the clipboard")
            test.compare(probe.find(test.history, "bannerLabel").text, "Copied")
            // A tap opens it in full, still plain.
            var item = probe.find(test.history, "textBody").parent.parent
            item.clicked()
        },
        function () {
            var page = window.pageStack.currentPage
            test.compare(page.objectName, "textPage")
            test.compare(probe.find(page, "textPageBody").text, Ev.EVIL_TEXT)
            test.compare(probe.find(page, "textPageFrom").text, Ev.EVIL_NAME)
            test.verifyPlainText(page, "the text page")
            window.pageStack.pop()
        },
        function () {
            // Clear list keeps what is still running.
            var clear = probe.find(test.history, "clearHistory")
            test.verify(test.history.clearable, "something to clear")
            clear.clicked()
            test.compare(engine.transfers.count, 1)
            test.compare(engine.texts.count, 0)
            window.pageStack.pop()
            // About: at the foot of Settings.
            test.main.openSettings()
        },
        function () {
            var settings = window.pageStack.currentPage
            test.compare(settings.objectName, "settingsPage")
            probe.find(settings, "openAbout").clicked()
            test.compare(window.pageStack.currentPage.objectName, "aboutPage")
            window.pageStack.pop()
            window.pageStack.pop()
            // An offer shows on the cover as a count, not a name.
            bridge.emitEvent(Ev.offer(9, {}))
        },
        function () {
            var coverText = test.textsOf(cover)
            test.verify(coverText.indexOf("1 offer waiting") >= 0, "the cover counts offers: " + coverText)
            test.verify(coverText.indexOf("EVIL") < 0, "and names nobody")
            // The engine failing to start is said on the main page.
            bridge.emitEvent('{"type":"fatal","error":{"code":"storage","message":"read-only file system"}}')
        },
        function () {
            test.compare(probe.find(test.main, "fatalLabel").text,
                         "Sukkula could not start: Could not save. Is the storage full?")
            test.verify(probe.find(test.main, "fatalLabel").visible, "shown")
            test.verify(!probe.find(test.main, "modeTabs").visible, "no tabs to switch")
            test.verify(!probe.find(test.main, "modePager").interactive, "nor to swipe")
            var before = test.count("set_receiving")
            test.main.setMode(false)
            test.compare(test.count("set_receiving"), before, "and nothing is sent")
            test.verify(test.textsOf(test.main).indexOf("read-only file system") >= 0, "the detail line")
        }
    ]
}
