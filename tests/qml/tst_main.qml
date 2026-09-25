// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "../../qml/cover"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The main page, the received-text page and the cover: the Receive switch
 * (F-C1) and each protocol's state, transfers with progress and cancel
 * (F-C5), received texts as plain text with a Copy button and nothing
 * that opens them (F-C4), and a cover that says whether Sukkula is
 * receiving without naming anyone.
 */
Script {
    id: test

    property Item main: null

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

    function textsOf(root) {
        return probe.texts(root).join("\n")
    }

    steps: [
        function () {
            test.main = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.verify(test.main !== null, "the main page loads")
        },
        function () {
            var toggle = probe.find(test.main, "receiveSwitch")
            test.compare(toggle.checked, false)
            test.verify(toggle.enabled, "enabled once the engine runs")
            test.compare(probe.find(test.main, "deviceNameLabel").text, "Shown to others as Jolla Phone")
            test.verify(test.textsOf(test.main).indexOf("LocalSend: Off") >= 0, "per-protocol state")
            test.verify(test.textsOf(cover).indexOf("Not receiving") >= 0, "the cover says off")
            // F-C1: one switch, one command.
            bridge.autoReply = false
            toggle.click()
            test.compare(test.lastCmd(), { type: "set_receiving", on: true })
            test.verify(toggle.busy, "busy until the reply")
            test.compare(toggle.checked, false, "the switch follows the engine, not the tap")
            // A second tap while busy sends nothing.
            toggle.click()
            var sets = 0
            var cmds = bridge.parsedCommands()
            for (var i = 0; i < cmds.length; i++) {
                if (cmds[i].cmd.type === "set_receiving") {
                    sets++
                }
            }
            test.compare(sets, 1, "one set_receiving while busy")
            bridge.emitEvent(Ev.reply(cmds[cmds.length - 1].id, true))
            bridge.emitEvent(Ev.receiving(true, true))
        },
        function () {
            var toggle = probe.find(test.main, "receiveSwitch")
            test.compare(toggle.checked, true)
            test.compare(toggle.busy, false)
            var all = test.textsOf(test.main)
            test.verify(all.indexOf("LocalSend: Failed") >= 0, "a failed protocol says so: " + all)
            test.verify(all.indexOf("Network error or timeout.") >= 0, "translated, by code")
            test.verify(all.indexOf("port 53317 in use") < 0, "the engine's English stays off the main page")
            test.verify(test.textsOf(cover).indexOf("Receiving") >= 0, "the cover says on")
            bridge.autoReply = true
            // A refused switch is reported.
            bridge.autoReply = false
            toggle.click()
            var cmds = bridge.parsedCommands()
            bridge.emitEvent(Ev.reply(cmds[cmds.length - 1].id, false, "unavailable"))
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text, "Not available. Is it switched off in Settings?")
            bridge.autoReply = true
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
            var peers = probe.findAll(test.main, "transferPeer")
            test.compare(peers.length, 4)
            test.compare(peers[3].text, "↓ " + Ev.EVIL_NAME, "the peer's name, plain")
            test.compare(peers[2].text, "↑ LocalSend", "no name: the protocol")
            var files = probe.findAll(test.main, "transferFiles")
            test.compare(files[3].text, Ev.EVIL_FILE + " and 1 more", "the first file and a count")
            test.compare(files[2].text, "Text")
            var status = probe.findAll(test.main, "transferStatus")
            test.compare(status[3].text, "500 B of 2.0 kB")
            test.compare(status[2].text, "Sent")
            test.compare(status[1].text, "Failed: Could not save. Is the storage full?")
            test.compare(status[0].text, "Saved in Downloads/Sukkula: " + Ev.EVIL_FILE + ", b.pdf")
            test.verify(test.textsOf(cover).indexOf("1 transfer, 25%") >= 0,
                        "the cover counts running transfers: " + test.textsOf(cover))
            // Cancel is on the running one only.
            var buttons = probe.findAll(test.main, "cancelButton")
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
            var body = probe.find(test.main, "textBody")
            test.compare(body.text, Ev.EVIL_TEXT, "the text as sent, markup and all")
            test.compare(body.textFormat, Text.PlainText)
            test.compare(probe.find(test.main, "textFrom").text, Ev.EVIL_NAME)
            probe.find(test.main, "copyButton").clicked()
            test.compare(Clipboard.text, Ev.EVIL_TEXT, "Copy puts the text on the clipboard")
            test.compare(probe.find(test.main, "bannerLabel").text, "Copied")
            // A tap opens it in full, still plain.
            var item = probe.find(test.main, "textBody").parent.parent
            item.clicked()
        },
        function () {
            var page = window.pageStack.currentPage
            test.compare(page.objectName, "textPage")
            test.compare(probe.find(page, "textPageBody").text, Ev.EVIL_TEXT)
            test.compare(probe.find(page, "textPageFrom").text, Ev.EVIL_NAME)
            test.verifyPlainText(page, "the text page")
            window.pageStack.pop()
            // Clear list keeps what is still running.
            engine.clearFinished()
            engine.clearTexts()
            test.compare(engine.transfers.count, 1)
            test.compare(engine.texts.count, 0)
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
            test.verify(!probe.find(test.main, "receiveSwitch").enabled, "nothing to switch")
            test.verify(test.textsOf(test.main).indexOf("read-only file system") >= 0, "the detail line")
        }
    ]
}
