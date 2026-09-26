// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * Send mode's radar (F-C6) with each protocol: peers from discovery and
 * the paired Bluetooth devices on the rings with their names as plain
 * text, each keeping its place; the send command's exact shape; the file
 * picker first when nothing is chosen; the "What to send" page; a send's
 * line, progress, cancel and end; refusals said and the chosen items
 * kept; Magic Wormhole's one-item rule, its code on the tile and on the
 * code page with the QR (F-MW1); Bluetooth's files-only rule (F-BT1);
 * more peers than the rings hold, as a list; and each protocol switched
 * off in Settings gone from the radar -- Magic Wormhole too, with Receive
 * mode's "Receive with a code" (F-C1).
 */
Script {
    id: test

    property Item main: null
    property Item view: null
    property Item page: null
    readonly property string downloads: "/home/defaultuser/Downloads/"

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

    function lastOf(type) {
        var all = test.commandsOfType(type)
        return all.length > 0 ? all[all.length - 1] : null
    }

    function lastId() {
        var cmds = bridge.parsedCommands()
        return cmds[cmds.length - 1].id
    }

    /// The radar's peers on show, by name.
    function shownNames() {
        var out = []
        var all = probe.findAll(test.view, "peerBubble")
        for (var i = 0; i < all.length; i++) {
            if (all[i].visible) {
                out.push(all[i].name)
            }
        }
        return out.sort()
    }

    function bubble(name) {
        var all = probe.findAll(test.view, "peerBubble")
        for (var i = 0; i < all.length; i++) {
            if (all[i].visible && all[i].name === name) {
                return all[i]
            }
        }
        return null
    }

    function find(name) {
        return probe.find(test.view, name)
    }

    steps: [
        function () {
            test.main = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.view = probe.find(test.main, "sendView")
            test.view.linger = 50
        },
        function () {
            test.verify(test.view.visible, "Send mode shows the radar")
            test.compare(test.commandsOfType("start_discovery").length, 1, "discovery runs in Send mode")
            test.compare(test.commandsOfType("list_bluetooth_devices").length, 1)
            test.compare(test.find("payloadSummary").text, "Tap to choose what to send")
            test.compare(test.find("radarHint").text, "Looking for devices nearby…")
            test.verify(test.find("wormholeTile").visible, "Magic Wormhole's tile")
            test.verify(!test.find("crocSlot").visible, "the tile beside it waits for croc")
            test.verify(probe.find(test.main, "choosePayload").visible)
            bridge.emitEvent(Ev.peerFound("p1", "local_send"))
            bridge.emitEvent(Ev.peerFound("q1", "quick_share", "Android"))
            bridge.emitEvent(Ev.bluetoothDevices([{ address: "AA:BB:CC:DD:EE:FF", name: "Car EVIL" }]))
        },
        function () {
            test.compare(test.shownNames(), [Ev.EVIL_NAME, "Android", "Car EVIL"].sort(), "every protocol's peers")
            test.compare(test.view.slots.slice(0, 3),
                         ["local_send:p1", "quick_share:q1", "bluetooth:AA:BB:CC:DD:EE:FF"])
            test.verify(!test.find("radarHint").visible)
            test.verifyPlainText(test.main, "the radar with peers")
            // Nothing chosen: the picker first, then the send.
            bridge.nextTransfer = 50
            test.bubble(Ev.EVIL_NAME).clicked()
            test.compare(test.commandsOfType("send").length, 0, "nothing to send yet")
            var picker = window.pageStack.currentPage
            test.verify(picker !== test.main, "the picker is up")
            picker.selectedContentProperties = { filePath: test.downloads + "a.txt" }
            test.compare(test.lastOf("send"), {
                type: "send",
                target: { protocol: "local_send", peer: "p1" },
                items: [{ kind: "file", path: test.downloads + "a.txt" }]
            })
            window.pageStack.pop()
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage", "no page to leave")
            test.compare(test.view.outgoing.transferId, 50)
            test.compare(test.shownNames(), [], "only the send's peer stays")
            var target = test.find("outgoingBubble")
            test.verify(target.visible, "drawn on its own")
            test.compare(target.name, Ev.EVIL_NAME)
            test.verify(test.find("sendLine").visible, "a line to it")
            test.verify(!test.find("wormholeLine").visible)
            test.compare(test.find("outgoingStatus").text, "Waiting for an answer…")
            bridge.emitEvent(Ev.transferStarted(50, "outgoing", {}))
            bridge.emitEvent(Ev.progress(50, 500, 2000))
        },
        function () {
            test.compare(test.find("outgoingPercent").text, "25%")
            test.compare(test.find("outgoingStatus").text, "500 B of 2.0 kB")
            test.compare(test.find("outgoingBubble").progress, 0.25, "the avatar fills")
            test.verify(probe.find(test.main, "cancelSending").visible)
            test.verify(!probe.find(test.main, "choosePayload").visible)
            test.find("cancelSend").clicked()
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 50 })
            bridge.emitEvent(Ev.finished(50, "cancelled"))
        },
        function () {
            test.compare(test.find("outgoingStatus").text, "Cancelled")
            test.compare(test.main.payload.itemCount, 1, "a send that did not go keeps what was chosen")
            test.find("outgoingBubble").clicked()
            test.verify(test.view.outgoing === null, "a tap goes back to the radar")
            test.compare(test.shownNames().length, 3, "with everyone on it")
            test.compare(test.find("payloadSummary").text, "a.txt")
            // The "What to send" page, from the centre.
            test.find("origin").clicked()
        },
        function () {
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "payloadPage")
            test.compare(probe.find(test.page, "sendFileName").text, "a.txt")
            probe.find(test.page, "sendText").text = "hello"
            test.compare(test.main.payload.typed, "hello", "typed goes with it")
            window.pageStack.pop()
        },
        function () {
            test.compare(test.find("payloadCount").text, "2")
            // A refused send is said, and everything stays.
            bridge.autoReply = false
            test.bubble("Android").clicked()
            test.compare(test.lastOf("send"), {
                type: "send",
                target: { protocol: "quick_share", peer: "q1" },
                items: [{ kind: "file", path: test.downloads + "a.txt" }, { kind: "text", text: "hello" }]
            })
            test.compare(test.find("outgoingStatus").text, "Connecting…")
            // No second send while one is on its way.
            test.view.choose(test.view.lanPeer({ protocol: "local_send", peerId: "p1", name: "x" }), 0)
            test.compare(test.commandsOfType("send").length, 2)
            bridge.emitEvent(Ev.reply(test.lastId(), false, "refused"))
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text, "Declined.")
            test.verify(test.view.outgoing === null)
            test.compare(test.shownNames().length, 3)
            test.compare(test.main.payload.itemCount, 2)
            bridge.autoReply = true
            // Bluetooth sends files only (F-BT1).
            test.bubble("Car EVIL").clicked()
            test.compare(test.commandsOfType("send").length, 2, "not with a text")
            test.compare(probe.find(test.main, "bannerLabel").text, "Bluetooth sends files only.")
            // Magic Wormhole: one item only (F-MW1).
            test.find("wormholeTile").clicked()
            test.compare(test.commandsOfType("send").length, 2, "not two items")
            test.compare(probe.find(test.main, "bannerLabel").text,
                         "Magic Wormhole sends one file or one text at a time.")
            test.main.payload.typed = ""
            bridge.nextTransfer = 77
            test.find("wormholeTile").clicked()
            test.compare(test.lastOf("send"), { type: "send", target: { protocol: "wormhole" },
                                                items: [{ kind: "file", path: test.downloads + "a.txt" }] })
        },
        function () {
            test.compare(test.view.outgoing.transferId, 77)
            var tile = test.find("wormholeTile")
            test.verify(tile.visible && tile.starting, "the tile waits for its code")
            test.verify(!test.find("wormholeTileCode").visible, "no code yet")
            test.verify(test.find("wormholeLine").visible, "the line goes up through the cloud")
            test.verify(!test.find("outgoingBubble").visible, "nobody has come yet")
            bridge.emitEvent(Ev.wormholeCode(77, "7-guitarist-revenge"))
            bridge.emitEvent(Ev.transferStarted(77, "outgoing", { protocol: "wormhole", peer: "" }))
        },
        function () {
            test.compare(test.find("wormholeTileCode").text, "7-guitarist-revenge")
            // The code, big, with its QR, a tap away.
            test.find("wormholeTile").clicked()
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "wormholeCodePage")
            test.compare(test.page.transferId, 77)
        },
        function () {
            test.compare(probe.find(test.page, "wormholeCode").text, "7-guitarist-revenge")
            var qr = probe.find(test.page, "wormholeQr")
            test.verify(qr.valid && qr.visible, "the QR code is drawn")
            test.compare(qr.size, 21)
            test.compare(probe.find(test.page, "wormholeStatus").text, "Waiting for the receiver…")
            window.pageStack.pop()
            bridge.emitEvent(Ev.progress(77, 500, 1000))
        },
        function () {
            test.verify(!test.find("wormholeTile").visible, "the receiver has come")
            test.verify(test.find("outgoingBubble").visible, "in the tile's place")
            test.compare(test.find("outgoingPercent").text, "50%")
            bridge.emitEvent(Ev.finished(77, "done"))
        },
        function () {
            test.compare(test.find("outgoingStatus").text, "Sent")
            test.compare(test.find("outgoingPercent").text, "100%")
            test.compare(test.main.payload.itemCount, 0, "what was sent is done with")
            return 150
        },
        function () {
            test.verify(test.view.outgoing === null, "back to the radar by itself")
            test.verify(test.find("wormholeTile").visible)
            test.compare(test.find("payloadSummary").text, "Tap to choose what to send")
            // A file from outside Downloads -- the gallery's, through the
            // Share menu -- is flagged, and the engine's bad_file is
            // explained: Sailjail grants Downloads only.
            test.main.share([{ kind: "file", path: test.downloads + "ok.txt" },
                             { kind: "file", path: "/home/defaultuser/Pictures/Jolla/p.jpg" },
                             { kind: "file", path: "relative/path.txt" },
                             { kind: "file", path: test.downloads + "ok.txt" },
                             { kind: "weird" },
                             null])
            test.compare(test.main.payload.files.length, 2, "absolute paths only, once each")
            test.find("origin").clicked()
        },
        function () {
            test.page = window.pageStack.currentPage
            var flags = probe.findAll(test.page, "sendFileOutside")
            test.compare(flags.length, 2)
            test.compare([flags[0].visible, flags[1].visible], [false, true], "only the Pictures file")
            window.pageStack.pop()
            bridge.autoReply = false
            test.bubble(Ev.EVIL_NAME).clicked()
            bridge.emitEvent(Ev.reply(test.lastId(), false, "bad_file"))
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text,
                         "A file could not be read. Sukkula can read files in Downloads only.")
            test.compare(test.main.payload.files.length, 2, "kept, to fix")
            bridge.autoReply = true
            // More peers than the rings hold.
            for (var i = 0; i < 8; i++) {
                bridge.emitEvent(Ev.peerFound("m" + i, "local_send", "Device " + i))
            }
        },
        function () {
            test.compare(test.shownNames().length, 7, "seven on the rings")
            test.compare(test.view.overflow, 4)
            var more = test.find("morePeers")
            test.verify(more.visible, "the rest behind +N")
            test.verify(probe.texts(more).indexOf("+4") >= 0)
            bridge.emitEvent(Ev.peerLost("p1"))
        },
        function () {
            var after = test.view.slots
            test.verify(after[0] !== "local_send:p1", "a peer that went leaves its place")
            test.verify(after[0] !== "", "to one that waited")
            test.compare(after[1], "quick_share:q1", "and nobody else moves")
            test.compare(test.view.overflow, 3)
            test.find("morePeers").clicked()
        },
        function () {
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "peerListPage")
            var rows = probe.findAll(test.page, "peerRow")
            test.compare(rows.length, 10, "every peer")
            var names = probe.findAll(test.page, "peerRowName")
            var shown = []
            for (var i = 0; i < names.length; i++) {
                shown.push(names[i].text)
            }
            test.verify(shown.indexOf("Car EVIL") >= 0 && shown.indexOf("Device 7") >= 0, shown.join(", "))
            test.verifyPlainText(test.page, "the device list")
            bridge.nextTransfer = 90
            for (var r = 0; r < rows.length; r++) {
                if (probe.find(rows[r], "peerRowName").text === "Device 7") {
                    rows[r].clicked()
                }
            }
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage", "back to the radar")
            var sent = test.lastOf("send")
            test.compare(sent.target, { protocol: "local_send", peer: "m7" }, "to the one tapped")
            test.verify(test.find("outgoingBubble").visible, "drawn, though it has no place on the rings")
            bridge.emitEvent(Ev.transferStarted(90, "outgoing", {}))
            // A share while it runs: the next thing to send.
            test.main.share([{ kind: "text", text: "next" }])
            bridge.emitEvent(Ev.finished(90, "done"))
            return 150
        },
        function () {
            test.verify(test.view.outgoing === null)
            test.compare(test.main.payload.texts, ["next"], "a share that came during a send outlives it")
            // F-C1: every protocol but Magic Wormhole off.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           bluetooth: { enabled: false } }))
        },
        function () {
            test.compare(test.shownNames(), [], "nobody on the rings")
            test.verify(!test.find("morePeers").visible)
            test.verify(test.find("wormholeTile").visible, "Magic Wormhole stays")
            test.verify(!test.find("radarHint").visible, "and nothing is looked for")
            // Magic Wormhole switched off as well.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           wormhole: { enabled: false, mailbox_url: null, relay_url: null },
                                           bluetooth: { enabled: false } }))
        },
        function () {
            test.verify(!test.find("wormholeTile").visible, "Magic Wormhole has a switch as well (F-C1)")
            test.verify(!test.find("cloud").visible)
            test.verify(test.find("radarHint").visible)
            test.compare(test.find("radarHint").text, "Every way of sending is switched off in Settings.")
            test.verify(!probe.find(test.main, "receiveWithCode").visible, "and nothing for Send mode")
            bridge.emitEvent(Ev.receiving(true))
        },
        function () {
            test.verify(!probe.find(test.main, "receiveWithCode").visible, "no receiving with a code either")
            // Magic Wormhole alone on.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           bluetooth: { enabled: false } }))
        },
        function () {
            test.verify(probe.find(test.main, "receiveWithCode").visible, "receiving with a code is back")
            bridge.emitEvent(Ev.receiving(false))
        },
        function () {
            test.verify(!probe.find(test.main, "receiveWithCode").visible, "in Receive mode only")
            // A reply after its page has gone is dropped quietly.
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            var other = probe.find(test.page, "sendView")
            test.page.payload.typed = "late"
            bridge.autoReply = false
            probe.find(other, "wormholeTile").clicked()
            test.compare(test.lastOf("send").items, [{ kind: "text", text: "late" }])
            test.pendingId = test.lastId()
            window.pageStack.pop()
            return 50
        },
        function () {
            bridge.emitEvent(Ev.reply(test.pendingId, true, "", 5))
            return 50
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage")
            test.compare(engine.discoveryUsers, 1, "the page that went gave its discovery back")
            var cmds = bridge.parsedCommands()
            var last = ""
            for (var i = 0; i < cmds.length; i++) {
                if (cmds[i].cmd.type === "start_discovery" || cmds[i].cmd.type === "stop_discovery") {
                    last = cmds[i].cmd.type
                }
            }
            test.compare(last, "start_discovery", "and did not stop it for the page still in Send mode")
        }
    ]

    property var pendingId: -1
}
