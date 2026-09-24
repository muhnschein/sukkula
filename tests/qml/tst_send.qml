// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * "Send via…" (F-C6) with each protocol: discovery while the page is up,
 * peers shown as plain text, the send command's exact shape, the file
 * picker, Magic Wormhole's one-item rule and its code page with the QR
 * (F-MW1), and Bluetooth's paired list (F-BT1).
 */
Script {
    id: test

    property Item page: null

    ApplicationWindow {
        id: window
    }

    Engine {
        id: engine
        backend: bridge
    }

    function commandsOfType(type) {
        var out = []
        var cmds = bridge.parsedCommands()
        for (var i = 0; i < cmds.length; i++) {
            if (cmds[i].cmd.type === type) {
                out.push(cmds[i].cmd)
            }
        }
        return out
    }

    function choose(protocol) {
        var box = probe.find(test.page, "protocolChoice")
        box.choose(test.page.available.indexOf(protocol))
        test.compare(test.page.protocol, protocol)
    }

    steps: [
        function () {
            // A main page underneath, as in the app.
            window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SendPage.qml"), {
                engine: engine,
                items: [
                    { kind: "file", path: "/home/defaultuser/Downloads/" + Ev.EVIL_FILE },
                    { kind: "file", path: "relative/path.txt" },
                    { kind: "file", path: "/home/defaultuser/Downloads/" + Ev.EVIL_FILE },
                    { kind: "text", text: Ev.EVIL_TEXT },
                    { kind: "weird" },
                    null
                ]
            })
        },
        function () {
            test.compare(test.commandsOfType("start_discovery").length, 1, "discovery starts with the page")
            test.compare(test.page.files.length, 1, "absolute paths only, once each")
            test.compare(probe.find(test.page, "sendFileName").text, Ev.EVIL_FILE)
            test.compare(probe.find(test.page, "sharedText").text, Ev.EVIL_TEXT, "a shared text, plain")
            test.compare(probe.find(test.page, "sendText").text, "", "the text box is the user's own")
            test.compare(test.page.available, ["local_send", "quick_share", "wormhole", "bluetooth"])
            test.compare(test.page.protocol, "local_send")
            bridge.emitEvent(Ev.peerFound("p1", "local_send"))
            bridge.emitEvent(Ev.peerFound("q1", "quick_share", "Android"))
        },
        function () {
            var names = probe.findAll(test.page, "peerName")
            test.compare(names.length, 1, "LocalSend's peers only")
            test.compare(names[0].text, Ev.EVIL_NAME)
            test.compare(probe.find(test.page, "peerModel").text, Ev.EVIL_MODEL)
            test.verifyPlainText(test.page, "the send page with peers")
            probe.find(test.page, "peerItem").clicked()
            var sent = test.commandsOfType("send")
            test.compare(sent.length, 1)
            test.compare(sent[0], {
                type: "send",
                target: { protocol: "local_send", peer: "p1" },
                items: [{ kind: "file", path: "/home/defaultuser/Downloads/" + Ev.EVIL_FILE },
                        { kind: "text", text: Ev.EVIL_TEXT }]
            })
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage", "back to the transfers")
            return 50
        },
        function () {
            test.compare(test.commandsOfType("stop_discovery").length, 1, "discovery stops with the page")
            // Quick Share, and a refused send that stays on the page.
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SendPage.qml"), {
                engine: engine, items: [{ kind: "text", text: "hello" }] })
            bridge.emitEvent(Ev.peerFound("q1", "quick_share", "Android"))
        },
        function () {
            test.choose("quick_share")
            test.compare(probe.find(test.page, "peerName").text, "Android")
            bridge.autoReply = false
            probe.find(test.page, "peerItem").clicked()
            var sent = test.commandsOfType("send")
            test.compare(sent[sent.length - 1].target, { protocol: "quick_share", peer: "q1" })
            var last = bridge.parsedCommands()
            bridge.emitEvent(Ev.reply(last[last.length - 1].id, false, "refused"))
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "sendPage", "a refusal stays here")
            test.compare(probe.find(test.page, "bannerLabel").text, "Declined.")
            bridge.autoReply = true
            // The file picker adds what it is given.
            test.page.pickFile()
            var picker = window.pageStack.currentPage
            test.verify(picker !== test.page, "the picker is up")
            picker.selectedContentProperties = { filePath: "/home/defaultuser/Downloads/b.pdf" }
            window.pageStack.pop()
        },
        function () {
            test.compare(test.page.files.length, 1)
            test.compare(test.page.files[0].name, "b.pdf")
            test.compare(test.commandsOfType("start_discovery").length, 2, "not restarted by the picker")
            // Magic Wormhole: one item only (F-MW1).
            test.choose("wormhole")
            var button = probe.find(test.page, "wormholeSend")
            test.verify(!button.enabled, "a file and a text are two items")
            test.page.removeText(0)
        },
        function () {
            var button = probe.find(test.page, "wormholeSend")
            test.verify(button.enabled, "one file is fine")
            bridge.nextTransfer = 77
            button.clicked()
            var sent = test.commandsOfType("send")
            test.compare(sent[sent.length - 1], { type: "send", target: { protocol: "wormhole" },
                                                   items: [{ kind: "file", path: "/home/defaultuser/Downloads/b.pdf" }] })
        },
        function () {
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "wormholeCodePage", "the code page takes over")
            test.compare(test.page.transferId, 77)
            test.verify(!probe.find(test.page, "wormholeCode").visible, "no code yet")
            bridge.emitEvent(Ev.wormholeCode(77, "7-guitarist-revenge"))
            bridge.emitEvent(Ev.transferStarted(77, "outgoing", { protocol: "wormhole", peer: "" }))
        },
        function () {
            test.compare(probe.find(test.page, "wormholeCode").text, "7-guitarist-revenge")
            var qr = probe.find(test.page, "wormholeQr")
            test.verify(qr.valid && qr.visible, "the QR code is drawn")
            test.compare(qr.size, 21)
            test.compare(probe.find(test.page, "wormholeStatus").text, "Waiting for the receiver…")
            bridge.emitEvent(Ev.progress(77, 500, 1000))
        },
        function () {
            test.compare(probe.find(test.page, "wormholeStatus").text, "500 B of 1.0 kB")
            bridge.emitEvent(Ev.finished(77, "done"))
        },
        function () {
            test.compare(probe.find(test.page, "wormholeStatus").text, "Sent")
            // A code whose QR the engine got wrong still shows the code.
            bridge.emitEvent(Ev.wormholeCode(77, "8-other-code", { size: 21, rows: [] }))
        },
        function () {
            test.compare(probe.find(test.page, "wormholeCode").text, "8-other-code")
            test.verify(!probe.find(test.page, "wormholeQr").visible, "no QR from a bad one")
            window.pageStack.pop()
            // Bluetooth: the paired list, files only.
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SendPage.qml"), {
                engine: engine, items: [{ kind: "file", path: "/home/defaultuser/Downloads/c.jpg" }] })
        },
        function () {
            test.choose("bluetooth")
            test.compare(test.commandsOfType("list_bluetooth_devices").length, 1)
            bridge.emitEvent(Ev.bluetoothDevices([{ address: "AA:BB:CC:DD:EE:FF", name: Ev.EVIL_NAME }]))
        },
        function () {
            test.compare(probe.find(test.page, "bluetoothName").text, Ev.EVIL_NAME)
            var item = probe.find(test.page, "bluetoothItem")
            test.verify(item.enabled, "a file can go")
            probe.find(test.page, "sendText").text = "and a text"
            test.verify(!item.enabled, "a text cannot")
            probe.find(test.page, "sendText").text = ""
            item.clicked()
            var sent = test.commandsOfType("send")
            test.compare(sent[sent.length - 1].target, { protocol: "bluetooth", address: "AA:BB:CC:DD:EE:FF" })
        },
        function () {
            // Every protocol off: nothing to choose.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           bluetooth: { enabled: false } }))
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SendPage.qml"), { engine: engine })
        },
        function () {
            test.compare(test.page.available, ["wormhole"], "wormhole has no switch")
            test.compare(test.page.protocol, "wormhole")
            test.verify(!probe.find(test.page, "wormholeSend").enabled, "nothing chosen, nothing to send")
            // A reply after the page has gone is dropped quietly.
            bridge.autoReply = false
            probe.find(test.page, "sendText").text = "late"
            probe.find(test.page, "wormholeSend").clicked()
            window.pageStack.pop()
            return 50
        },
        function () {
            var last = bridge.parsedCommands()
            var id = -1
            for (var i = last.length - 1; i >= 0; i--) {
                if (last[i].cmd.type === "send") {
                    id = last[i].id
                    break
                }
            }
            bridge.emitEvent(Ev.reply(id, true, "", 5))
            return 50
        }
    ]
}
