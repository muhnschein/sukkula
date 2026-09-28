// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "../../qml/cover"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The main page, the History page, the received-text page and the cover:
 * Send | Receive as tabs that are the engine's state while in front
 * (F-C1) -- discovery on the Send tab only, a tab swiped back to
 * mid-switch switched back to, a refused switch taking the tab back, the
 * page in portrait, and what receiving is doing on the Receive tab; after
 * a while in the background receiving stops, but not while an offer
 * waits, and starts again back in front (spec v0.7); every transfer with
 * progress and cancel on the History page (F-C5), received texts there
 * as plain text with a Copy button and nothing that opens them (F-C4);
 * About reached from Settings, not from a pulley; and a cover that names
 * nobody: Send and Receive with an action each while nothing goes on, an
 * offer waiting with its countdown, a transfer's ring and percentage.
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
            test.compare(probe.find(cover, "coverSend").text, "Send", "nothing going on: the cover's two halves")
            test.compare(probe.find(cover, "coverReceive").text, "Receive")
            test.verify(probe.find(cover, "coverIdle").visible)
            test.verify(test.textsOf(test.main).indexOf("About Sukkula") < 0, "About is not in a pulley")
            // The tabs as Silica's TabBar lays them out: the words side by
            // side in the middle, the one on screen underlined by a hairline
            // as wide as its word.
            var sendTab = probe.find(test.main, "modeSend")
            var receiveTab = probe.find(test.main, "modeReceive")
            var sendWord = sendTab.labelItem
            var receiveWord = receiveTab.labelItem
            var leftSpace = sendTab.x + sendWord.x
            var rightSpace = tabs.width - (receiveTab.x + receiveWord.x + receiveWord.width)
            test.verify(Math.abs(leftSpace - rightSpace) < 1, "the words centred as one: " + leftSpace + " " + rightSpace)
            test.compare(receiveTab.x + receiveWord.x - (sendTab.x + sendWord.x + sendWord.width),
                         2 * Theme.paddingMedium, "and close together")
            var underline = probe.find(test.main, "modeUnderline")
            test.compare(underline.width, sendWord.width, "the line as wide as the word")
            test.compare(underline.x, sendTab.x + sendWord.x)
            test.compare(underline.height, Theme._lineWidth, "a hairline")
            // Rows scrolled up are cut at the tabs' edge, not drawn behind
            // them; the tab on screen, tapped, takes its list to the top.
            var list = pager.currentItem.item
            var viewport = probe.find(test.main, "modeViewport")
            test.verify(!viewport.clip, "nothing clipped at rest")
            list.contentY = 50
            test.verify(viewport.clip, "clipped while scrolled")
            // F-C1: one tap, one command.
            bridge.autoReply = false
            probe.find(test.main, "modeSend").clicked()
            test.compare(list.contentY, 0, "the tab on screen, tapped: back to the top")
            test.verify(!viewport.clip)
            test.compare(test.count("set_receiving"), 0, "the tab shown is no change")
            probe.find(test.main, "modeReceive").clicked()
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: true })
            test.compare(test.lastCmd(), { type: "stop_discovery" }, "and the Send tab's discovery stops")
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
            test.compare(probe.find(receive, "receiveState").text, "Ready to receive", "Quick Share is up")
            test.compare(test.count("stop_discovery"), 3, "no discovery on the Receive tab")
            test.compare(engine.discoveryUsers, 0)
            var failed = probe.find(receive, "receiveFailed")
            test.verify(failed.visible, "a way that could not start is said")
            test.compare(probe.find(failed, "receiveFailedLine").text,
                         "Not every device nearby can see this phone. Tap to see why.")
            var all = test.textsOf(receive)
            test.verify(all.indexOf("port 53317 in use") < 0, "the engine's English stays off the tab")
            test.verify(all.indexOf("How others can reach this phone") < 0, "how each way does is in Settings")
            test.verify(probe.find(cover, "coverIdle").visible, "the cover the same: nothing going on")
            // A refused switch is reported, and the tab goes back.
            probe.find(test.main, "modeSend").clicked()
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: false })
            test.answerLast(false, "unavailable")
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text, "Not available. Is it switched off in Settings?")
            test.compare(probe.find(test.main, "modeTabs").currentIndex, 1, "the tab goes back")
            test.compare(engine.receiving, true, "still receiving")
            test.compare(test.count("start_discovery"), 3, "and not looking")
            bridge.autoReply = true
            // In the background for a while: receiving stops -- but not
            // while an offer waits for its answer.
            bridge.emitEvent(Ev.offer(8, {}))
            test.main.backgroundGrace = 30
            test.main.foreground = false
            return 100
        },
        function () {
            test.verify(!test.main.awake, "asleep")
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: false },
                         "not yet: the offer waits")
            test.compare(test.count("set_receiving"), 4)
            test.compare(probe.find(cover, "coverOffer").visible, true, "the cover says an offer waits")
            bridge.emitEvent(Ev.offerClosed(8, "timed_out"))
            return 50
        },
        function () {
            test.compare(test.count("set_receiving"), 5, "the offer gone, receiving stops")
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: false })
            bridge.emitEvent(Ev.receiving(false))
            test.compare(probe.find(test.main, "modeTabs").busyIndex, -1, "no spinner for it")
        },
        function () {
            test.compare(probe.find(test.main, "modeTabs").currentIndex, 1, "the tab stays")
            test.compare(test.count("start_discovery"), 3, "and nothing looks for devices instead")
            // Back in front: the Receive tab receives again.
            test.main.foreground = true
            test.compare(test.lastOf("set_receiving"), { type: "set_receiving", on: true })
            bridge.emitEvent(Ev.receiving(true))
        },
        function () {
            test.compare(probe.find(test.main, "modeTabs").currentIndex, 1)
            test.compare(engine.receiving, true)
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
            test.verify(probe.find(cover, "coverTransfer").visible, "the cover shows the transfer")
            test.compare(probe.find(cover, "coverPercent").text, "25%")
            test.compare(probe.find(cover, "coverRing").value, 0.25, "as one ring")
            test.compare(probe.find(cover, "coverDirection").text, "Receiving")
            test.compare(probe.find(cover, "coverWhat").text, "2 files", "how many, never a name")
            test.verify(!probe.find(cover, "coverIdle").visible)
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
            test.verify(probe.find(cover, "coverOffer").visible, "the cover says an offer waits")
            test.verify(/^Declined in (59|60) s$/.test(probe.find(cover, "coverCountdown").text),
                        probe.find(cover, "coverCountdown").text)
            test.verify(coverText.indexOf("EVIL") < 0, "and names nobody")
            // Its actions: only while nothing goes on, each to its tab.
            bridge.emitEvent(Ev.offerClosed(9, "timed_out"))
            bridge.emitEvent(Ev.finished(1, "done"))
        },
        function () {
            test.verify(probe.find(cover, "coverIdle").visible)
            test.verify(cover.children[cover.children.length - 1].enabled, "the actions are on")
            var opened = []
            cover.openTab.connect(function (index) { opened.push(index) })
            var actions = cover.children[cover.children.length - 1].actions
            actions[0].triggered()
            actions[1].triggered()
            test.compare(opened, [0, 1], "Send, then Receive")
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
