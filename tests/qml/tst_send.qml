// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The Send tab (F-C6) with each protocol: the question and its four
 * tiles, each opening its own picker, while discovery already runs and
 * the foot says who is about; files chosen, summed up as "2 photos" with
 * their size, more added and all cleared behind a remorse; the devices
 * nearby by name as plain text, a device found over two protocols as one
 * row whose menu says which way, the paired Bluetooth devices after
 * them; the send command's exact shape; a send's row with its progress,
 * cancel and end while the other rows wait, and the files still chosen
 * after it; refusals said; About this device (address, pinned
 * certificate); sending with a code: Magic Wormhole for one file, croc
 * for several, its code and QR, Copy and Share, "Their app" switching
 * with the old code given up, the code given up when its page is left
 * unused, and its progress on the tab once the receiver has come (F-MW1,
 * F-CR1); and each protocol switched off in Settings gone from the tab
 * (F-C1).
 */
Script {
    id: test

    property Item main: null
    property Item view: null
    property Item page: null
    property var pendingId: -1
    readonly property string downloads: "/home/defaultuser/Downloads/"
    readonly property string pictures: "/home/defaultuser/Pictures/"
    readonly property string fingerprint: "3FA2910C5B7ED4A10C9F22E87B316A0D91C45E02AA7F3D18B6E90417C2D57E44"

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

    function find(name) {
        return probe.find(test.view, name)
    }

    /// The device rows on show: [title, subtitle] each, in order.
    function rows() {
        var out = []
        var all = probe.findAll(test.view, "deviceRow")
        for (var i = 0; i < all.length; i++) {
            if (all[i].visible) {
                out.push([all[i].title, all[i].subtitle])
            }
        }
        return out
    }

    function row(title) {
        var all = probe.findAll(test.view, "deviceRow")
        for (var i = 0; i < all.length; i++) {
            if (all[i].visible && all[i].title === title) {
                return all[i]
            }
        }
        return null
    }

    /// The send's progress row on show, or null.
    function progress() {
        var all = probe.findAll(test.view, "sendProgress")
        for (var i = 0; i < all.length; i++) {
            if (all[i].visible) {
                return all[i]
            }
        }
        return null
    }

    /// A device row's menu, opened: its items' texts, and the items.
    function menuOf(item) {
        var menu = item.menu.createObject(item)
        var items = []
        var texts = []
        var kids = probe.findAll(menu, "sendWith").concat(probe.findAll(menu, "aboutDevice"))
        for (var i = 0; i < kids.length; i++) {
            items.push(kids[i])
            texts.push(kids[i].text)
        }
        return { menu: menu, items: items, texts: texts }
    }

    /// Ticks `rows` ({url} or {filePath}, with fileSize) in the picker on
    /// top, which must be `name`, and accepts.
    function pick(name, rows) {
        var picker = window.pageStack.currentPage
        test.compare(picker.objectName, name, "the picker up")
        for (var i = 0; i < rows.length; i++) {
            picker.selectedContent.append(rows[i])
        }
        picker.accept()
    }

    function peer(id, protocol, name, extra) {
        var p = { id: id, protocol: protocol, name: name, model: Ev.EVIL_MODEL, device_type: "phone" }
        for (var k in extra) {
            p[k] = extra[k]
        }
        return Ev.json({ type: "peer_found", peer: p })
    }

    steps: [
        function () {
            test.main = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.view = probe.find(test.main, "sendView")
            test.view.linger = 50
        },
        function () {
            test.verify(test.view.current, "the Send tab first")
            test.compare(test.commandsOfType("start_discovery").length, 1, "discovery runs from the start")
            test.compare(test.commandsOfType("list_bluetooth_devices").length, 1)
            test.compare(test.find("sendQuestion").text, "What would you like to send?")
            test.verify(test.find("pickPhotos").visible && test.find("pickVideos").visible
                        && test.find("pickDocuments").visible && test.find("pickFiles").visible, "four tiles")
            test.compare(test.find("nearbyLine").text, "Looking for devices nearby…")
            test.verify(!test.find("payloadRow").visible, "nothing chosen")
            test.compare(test.rows(), [], "no devices to pick from yet")
            bridge.emitEvent(Ev.peerFound("p1", "local_send"))
            bridge.emitEvent(Ev.peerFound("q1", "quick_share", "Android"))
            bridge.emitEvent(Ev.bluetoothDevices([{ address: "AA:BB:CC:DD:EE:FF", name: "Car EVIL" }]))
        },
        function () {
            test.compare(test.find("nearbyLine").text, Ev.EVIL_NAME + " and 1 more nearby",
                         "who is about, the paired ones not counted; the name's %2 stays text")
            test.verifyPlainText(test.main, "the tab with peers found")
            // Photos: Gallery's picker, several at once, with sizes.
            test.find("pickPhotos").clicked()
            test.pick("photoPicker", [{ url: "file://" + test.pictures + "a.jpg", fileSize: 1000 },
                                      { url: "file://" + test.pictures + "b%20c.jpg", fileSize: 3000 },
                                      { url: "https://evil.example/x.jpg" }])
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage", "the picker closes")
            test.compare(test.main.payload.files.length, 2, "local files only")
            test.compare(test.main.payload.files[1].path, test.pictures + "b c.jpg", "the URL decoded")
            test.verify(!test.find("sendQuestion").visible, "the question has its answer")
            test.compare(test.find("payloadSummary").text, "2 photos")
            test.compare(test.find("payloadSize").text, "4.0 kB")
            test.compare(test.rows(), [[Ev.EVIL_NAME, "Phone · LocalSend"], ["Android", "Phone · Quick Share"],
                                       ["Car EVIL", "Paired device · Bluetooth"]],
                         "by name, the paired ones last, the protocol in the grey line")
            test.compare(test.find("nearbyHint").text,
                         "Someone missing? They need to be on the same Wi-Fi, with their device ready to receive.")
            test.verify(test.find("sendWithCode").visible, "and far away, with a code")
            test.verifyPlainText(test.main, "the device list")
            // The cross clears it all, after a moment to change one's mind.
            test.find("clearPayload").clicked()
            var remorse = probe.find(test.main, "clearRemorse")
            test.verify(remorse.active, "a remorse first")
            remorse.cancel()
            test.compare(test.main.payload.itemCount, 2, "cancelled: still chosen")
            test.find("clearPayload").clicked()
            remorse.trigger()
            test.compare(test.main.payload.itemCount, 0)
            test.verify(test.find("pickVideos").visible, "the tiles again")
            test.compare(test.rows(), [], "the devices go with it")
            // Each tile its own picker.
            test.find("pickVideos").clicked()
            test.compare(window.pageStack.currentPage.objectName, "videoPicker")
            window.pageStack.pop()
            test.find("pickDocuments").clicked()
            test.compare(window.pageStack.currentPage.objectName, "documentPicker")
            window.pageStack.pop()
            // Any file: the file browser; one, by its path, size unknown.
            test.find("pickFiles").clicked()
            test.pick("filePicker", [{ filePath: test.downloads + "a.txt", url: "" }])
        },
        function () {
            test.compare(test.find("payloadSummary").text, "a.txt", "one file: its name")
            test.verify(!test.find("payloadSize").visible, "no size where none was said")
            // + adds more, in the same kind of picker.
            test.find("addMore").clicked()
            test.compare(window.pageStack.currentPage.objectName, "documentPicker", "a document's picker")
            window.pageStack.pop()
            bridge.nextTransfer = 50
            test.row(Ev.EVIL_NAME).clicked()
            test.compare(test.lastOf("send"), {
                type: "send",
                target: { protocol: "local_send", peer: "p1" },
                items: [{ kind: "file", path: test.downloads + "a.txt" }]
            })
        },
        function () {
            test.compare(test.view.outgoing.transferId, 50)
            var p = test.progress()
            test.verify(p !== null, "the device's row shows the send")
            test.compare(p.title, Ev.EVIL_NAME)
            test.compare(probe.find(p, "progressStatus").text, "Waiting for an answer…")
            test.verify(test.row("Android").dimmed && test.row("Car EVIL").dimmed, "the other rows wait")
            test.verify(!test.find("clearPayload").enabled, "nothing to clear while it goes")
            bridge.emitEvent(Ev.transferStarted(50, "outgoing", {}))
            bridge.emitEvent(Ev.progress(50, 500, 2000))
        },
        function () {
            var p = test.progress()
            test.compare(probe.find(p, "progressPercent").text, "25%")
            test.compare(probe.find(p, "progressStatus").text, "Sending…")
            test.compare(probe.find(p, "progressDetail").text, "500 B of 2.0 kB")
            test.compare(probe.find(p, "progressLine").value, 0.25)
            test.verifyPlainText(test.main, "a send's row")
            probe.find(p, "cancelTransfer").clicked()
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 50 })
            bridge.emitEvent(Ev.finished(50, "cancelled"))
        },
        function () {
            test.compare(probe.find(test.progress(), "progressStatus").text, "Cancelled")
            test.compare(test.main.payload.itemCount, 1, "a send that did not go keeps what was chosen")
            return 150
        },
        function () {
            test.verify(test.view.outgoing === null, "back to the list by itself")
            test.compare(test.rows().length, 3, "with everyone")
            test.verify(!test.row("Android").dimmed)
            // One device over two ways: one row, by its name.
            bridge.emitEvent(test.peer("q2", "quick_share", "Pixel 8", { address: "192.168.1.42", model: undefined }))
            bridge.emitEvent(test.peer("p2", "local_send", "Pixel 8", { address: "192.168.1.42:53317",
                                                                        fingerprint: test.fingerprint }))
        },
        function () {
            test.compare(test.row("Pixel 8").subtitle, "Phone · Quick Share, LocalSend", "one row, both ways")
            test.compare(test.rows().length, 4)
            var m = test.menuOf(test.row("Pixel 8"))
            test.compare(m.texts, ["Send with Quick Share", "Send with LocalSend", "About this device"])
            // Its menu sends the other way.
            bridge.autoReply = false
            m.items[1].clicked()
            m.menu.destroy()
            test.compare(test.lastOf("send").target, { protocol: "local_send", peer: "p2" }, "over LocalSend")
            test.compare(probe.find(test.progress(), "progressStatus").text, "Connecting…")
            // No second send while one is on its way.
            test.row("Android").clicked()
            test.compare(test.commandsOfType("send").length, 2)
            // A refused send is said, and everything stays.
            bridge.emitEvent(Ev.reply(test.lastId(), false, "refused"))
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text, "Declined.")
            test.verify(test.view.outgoing === null)
            test.compare(test.main.payload.itemCount, 1)
            bridge.autoReply = true
            // A tap sends the first way.
            bridge.nextTransfer = 51
            test.row("Pixel 8").clicked()
            test.compare(test.lastOf("send").target, { protocol: "quick_share", peer: "q2" }, "Quick Share first")
        },
        function () {
            bridge.emitEvent(Ev.transferStarted(51, "outgoing", { protocol: "quick_share", peer: "Pixel 8" }))
            bridge.emitEvent(Ev.finished(51, "done"))
        },
        function () {
            var p = test.progress()
            test.compare(probe.find(p, "progressStatus").text, "Sent")
            test.compare(probe.find(p, "progressPercent").text, "100%")
            test.compare(test.main.payload.itemCount, 1, "what was sent stays chosen, for another device")
            return 150
        },
        function () {
            test.verify(test.view.outgoing === null)
            // About this device.
            var m = test.menuOf(test.row("Pixel 8"))
            m.items[2].clicked()
            m.menu.destroy()
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "devicePage")
        },
        function () {
            test.compare(probe.find(test.page, "deviceName").text, "Pixel 8")
            var values = []
            var all = probe.findAll(test.page, "detailValue")
            for (var i = 0; i < all.length; i++) {
                values.push(all[i].text)
            }
            test.compare(values, ["Phone", "192.168.1.42",
                                  Ev.EVIL_MODEL, "Phone", "192.168.1.42:53317",
                                  "3FA2 910C 5B7E D4A1 0C9F 22E8 7B31 6A0D 91C4 5E02 AA7F 3D18 B6E9 0417 C2D5 7E44"],
                         "Quick Share's, then LocalSend's with its model and pinned certificate")
            test.verify(probe.find(test.page, "deviceNote").text.indexOf("checks this certificate") >= 0)
            test.verifyPlainText(test.page, "About this device")
            window.pageStack.pop()
            // The Share menu: files only, absolute paths, each once; and
            // the engine's bad_file explained with what Sailjail grants.
            test.main.share([{ kind: "file", path: test.downloads + "ok.txt" },
                             { kind: "file", path: "/home/defaultuser/Pictures/Jolla/p.jpg" },
                             { kind: "file", path: "relative/path.txt" },
                             { kind: "file", path: test.downloads + "ok.txt" },
                             { kind: "text", text: "not sent" },
                             { kind: "weird" },
                             null])
            test.compare(test.main.payload.files.length, 2, "absolute paths only, once each, no texts")
            test.compare(test.find("payloadSummary").text, "2 files")
            bridge.autoReply = false
            test.row(Ev.EVIL_NAME).clicked()
            bridge.emitEvent(Ev.reply(test.lastId(), false, "bad_file"))
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text,
                         "A file could not be read. Sukkula can send files from Downloads, Documents, "
                         + "Music, Pictures, Videos and memory cards only.")
            test.compare(test.main.payload.files.length, 2, "kept, to fix")
            bridge.autoReply = true
            // With a code: several files go with croc.
            bridge.nextTransfer = 60
            test.find("sendWithCode").clicked()
            test.page = window.pageStack.currentPage
            test.compare(test.page.objectName, "sendCodePage")
            test.compare(test.lastOf("send"), { type: "send", target: { protocol: "croc" },
                                                items: [{ kind: "file", path: test.downloads + "ok.txt" },
                                                        { kind: "file", path: "/home/defaultuser/Pictures/Jolla/p.jpg" }] })
        },
        function () {
            var box = probe.find(test.page, "theirApp")
            test.compare(box.currentIndex, 1, "croc")
            test.compare(box.description, "Sukkula, or the croc app or command, can take it.")
            test.compare(probe.find(test.page, "codeStatus").text, "Getting a code…")
            // Magic Wormhole cannot take two files.
            box.choose(0)
            test.compare(test.commandsOfType("send").length, 5, "no second send")
            test.compare(test.view.outgoing.protocol, "croc")
            bridge.emitEvent(Ev.crocCode(60, "gala-tulip-acorn"))
            bridge.emitEvent(Ev.transferStarted(60, "outgoing", { protocol: "croc", peer: "croc" }))
        },
        function () {
            test.compare(probe.find(test.page, "sendCode").text, "gala-tulip-acorn")
            var qr = probe.find(test.page, "sendQr")
            test.verify(qr.valid && qr.visible, "croc's QR code is drawn")
            test.compare(qr.size, 21)
            test.compare(probe.find(test.page, "codeStatus").text, "Waiting for them to type the code…")
            test.compare(probe.find(test.page, "codeServer").text,
                         "Goes through croc's public relay on the internet. Your own relay can be set in Settings.")
            // Leaving the page before anyone came gives the code up.
            window.pageStack.pop()
            return 50
        },
        function () {
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 60 }, "an unused code given up")
            test.verify(test.view.outgoing === null)
            // One file: Magic Wormhole.
            test.main.share([{ kind: "file", path: test.downloads + "a.txt" }])
            bridge.nextTransfer = 77
            test.find("sendWithCode").clicked()
            test.page = window.pageStack.currentPage
            test.compare(test.lastOf("send"), { type: "send", target: { protocol: "wormhole" },
                                                items: [{ kind: "file", path: test.downloads + "a.txt" }] })
        },
        function () {
            test.compare(probe.find(test.page, "theirApp").currentIndex, 0, "Magic Wormhole")
            test.compare(probe.find(test.page, "codePayload").text, "a.txt")
            bridge.emitEvent(Ev.wormholeCode(77, "7-guitarist-revenge"))
            bridge.emitEvent(Ev.transferStarted(77, "outgoing", { protocol: "wormhole", peer: "" }))
        },
        function () {
            test.compare(probe.find(test.page, "sendCode").text, "7-guitarist-revenge")
            test.verify(probe.find(test.page, "sendQr").visible)
            probe.find(test.page, "copyCode").clicked()
            test.compare(Clipboard.text, "7-guitarist-revenge", "Copy")
            var shareButton = probe.find(test.page, "shareCode")
            test.verify(shareButton.visible, "Share…")
            shareButton.clicked()
            var action = test.page.children[0].item
            test.compare(action.triggered, 1, "the share sheet")
            test.compare(action.resources, [{ data: "7-guitarist-revenge", name: "code.txt", type: "text/plain" }],
                         "with the code as text, and nothing else")
            // Their app: croc. The code shown is given up for a new one.
            bridge.nextTransfer = 78
            probe.find(test.page, "theirApp").choose(1)
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 77 }, "the old code given up")
            test.compare(test.lastOf("send"), { type: "send", target: { protocol: "croc" },
                                                items: [{ kind: "file", path: test.downloads + "a.txt" }] })
        },
        function () {
            test.compare(test.view.outgoing.transferId, 78)
            test.compare(probe.find(test.page, "sendCode").visible, false, "the old code is gone")
            bridge.emitEvent(Ev.crocCode(78, "gala-tulip-acorn"))
            bridge.emitEvent(Ev.transferStarted(78, "outgoing", { protocol: "croc", peer: "croc" }))
        },
        function () {
            test.compare(probe.find(test.page, "sendCode").text, "gala-tulip-acorn")
            bridge.emitEvent(Ev.progress(78, 500, 1000))
        },
        function () {
            // The receiver has come: progress, and no more code to show.
            test.verify(!probe.find(test.page, "sendQr").visible)
            var p = probe.find(test.page, "codeProgress")
            test.verify(p.visible)
            test.compare(probe.find(p, "progressPercent").text, "50%")
            test.verify(!probe.find(test.page, "theirApp").enabled, "no switching once it goes")
            // Leaving now keeps it going, on the tab.
            window.pageStack.pop()
            return 50
        },
        function () {
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 77 }, "nothing more given up")
            var p = test.find("codeProgress")
            test.verify(p.visible, "the tab's Far away row shows it")
            test.compare(probe.find(p, "progressPercent").text, "50%")
            test.verify(!test.find("sendWithCode").visible)
            bridge.emitEvent(Ev.finished(78, "done"))
        },
        function () {
            test.compare(probe.find(test.find("codeProgress"), "progressStatus").text, "Sent")
            return 150
        },
        function () {
            test.verify(test.view.outgoing === null)
            test.verify(test.find("sendWithCode").visible, "ready for another")
            // F-C1: every protocol but Magic Wormhole and croc off.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           bluetooth: { enabled: false } }))
        },
        function () {
            test.compare(test.rows(), [], "nobody nearby")
            test.compare(test.find("nearbyHint").text, "Sending nearby is switched off in Settings.")
            test.verify(test.find("sendWithCode").visible, "codes still go")
            // Magic Wormhole switched off: croc takes one file too.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           wormhole: { enabled: false, mailbox_url: null, relay_url: null },
                                           bluetooth: { enabled: false } }))
        },
        function () {
            bridge.nextTransfer = 79
            test.find("sendWithCode").clicked()
            test.page = window.pageStack.currentPage
            test.compare(test.lastOf("send").target, { protocol: "croc" }, "croc, the one left")
            test.verify(!probe.find(test.page, "theirApp").visible, "no app to choose")
            window.pageStack.pop()
            // And croc switched off: nothing far away.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           wormhole: { enabled: false, mailbox_url: null, relay_url: null },
                                           croc: { enabled: false, relay: null, password: null },
                                           bluetooth: { enabled: false } }))
        },
        function () {
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 79 })
            test.verify(!test.find("sendWithCode").visible, "croc has a switch too")
            bridge.emitEvent(Ev.settings({}))
        },
        function () {
            // A reply after its page has gone is dropped quietly.
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            var other = probe.find(test.page, "sendView")
            test.page.payload.load([{ kind: "file", path: test.downloads + "late.txt" }])
            bridge.autoReply = false
            var late = null
            var all = probe.findAll(other, "deviceRow")
            for (var i = 0; i < all.length; i++) {
                if (all[i].title === "Android") {
                    late = all[i]
                }
            }
            late.clicked()
            test.compare(test.lastOf("send").items, [{ kind: "file", path: test.downloads + "late.txt" }])
            test.pendingId = test.lastId()
            window.pageStack.pop()
            return 50
        },
        function () {
            bridge.emitEvent(Ev.reply(test.pendingId, true, "", 5))
            return 50
        },
        function () {
            bridge.autoReply = true
            test.compare(window.pageStack.currentPage.objectName, "mainPage")
            test.compare(engine.discoveryUsers, 1, "the page that went gave its discovery back")
            var cmds = bridge.parsedCommands()
            var last = ""
            for (var i = 0; i < cmds.length; i++) {
                if (cmds[i].cmd.type === "start_discovery" || cmds[i].cmd.type === "stop_discovery") {
                    last = cmds[i].cmd.type
                }
            }
            test.compare(last, "start_discovery", "and did not stop it for the page still on the Send tab")
        }
    ]
}
