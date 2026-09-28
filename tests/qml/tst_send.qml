// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The Send tab (F-C6) with each protocol, round the anchor: "Ready to
 * send" with who is about under it, the sweep going round while
 * discovery runs, and one button, the content picker; files chosen,
 * summed up as "2 photos" with their size, more added and all cleared
 * behind a remorse; the devices nearby by name as plain text, a device
 * found over two protocols as one row whose menu says which way, the
 * paired Bluetooth devices after them, and one "Looking for more"; the
 * send command's exact shape; a send as the whole tab: waiting for an
 * answer, the ring filling with the percentage, Cancel going straight
 * back, a check with "Send to another" and "Done", a cross with why,
 * "Try again" and "Back"; refusals said; About this device (address,
 * pinned certificate); sending with a code: Magic Wormhole for one file,
 * croc for several, its code as the title and its QR code in the
 * anchor's place, Copy and Share, "Receiver's app" switching with the
 * old code given up, the code given up when its page is left unused, no
 * server line, and the send going on on the tab once the receiver has
 * come (F-MW1, F-CR1); and each protocol switched off in Settings gone
 * from the tab (F-C1).
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

    /// The Send tab's hero, and a label of it.
    function hero() {
        return test.find("sendHero")
    }
    function says(name) {
        return probe.find(test.hero(), name).text
    }
    function anchor() {
        return probe.find(test.hero(), "anchor")
    }
    /// A label of the code page's hero.
    function pageSays(name) {
        return probe.find(probe.find(test.page, "codeHero"), name).text
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
        },
        function () {
            test.verify(test.view.current, "the Send tab first")
            test.compare(test.commandsOfType("start_discovery").length, 1, "discovery runs from the start")
            test.compare(test.commandsOfType("list_bluetooth_devices").length, 1)
            test.compare(test.says("heroTitle"), "Ready to send")
            test.compare(test.says("heroSubtitle"), "Looking for devices nearby…")
            test.compare(test.anchor().mode, "looking", "the sweep while devices are looked for")
            test.verify(probe.find(test.anchor(), "anchorSweep").visible)
            test.compare(test.anchor().glyph, "phone")
            test.verify(test.find("chooseFiles").visible, "one button")
            test.compare(test.find("pickContent"), null, "no tiles")
            test.compare(test.find("pickFiles"), null, "the content picker has the file system")
            test.verify(!test.find("addMore").visible && !test.find("clearPayload").visible, "nothing chosen")
            test.compare(test.rows(), [], "no devices to pick from yet")
            bridge.emitEvent(Ev.peerFound("p1", "local_send"))
            bridge.emitEvent(Ev.peerFound("q1", "quick_share", "Android"))
            bridge.emitEvent(Ev.bluetoothDevices([{ address: "AA:BB:CC:DD:EE:FF", name: "Car EVIL" }]))
        },
        function () {
            test.compare(test.says("heroSubtitle"), Ev.EVIL_NAME + " and 1 more nearby",
                         "who is about, the paired ones not counted; the name's %2 stays text")
            test.verifyPlainText(test.main, "the tab with peers found")
            // The content picker: pictures, videos, music and documents in
            // one, several at once, with sizes.
            test.find("chooseFiles").clicked()
            test.pick("contentPicker", [{ url: "file://" + test.pictures + "a.jpg", fileSize: 1000 },
                                        { url: "file://" + test.pictures + "b%20c.jpg", fileSize: 3000 },
                                        { url: "https://evil.example/x.jpg" }])
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage", "the picker closes")
            test.compare(test.main.payload.files.length, 2, "local files only")
            test.compare(test.main.payload.files[1].path, test.pictures + "b c.jpg", "the URL decoded")
            test.compare(test.says("heroTitle"), "2 photos", "what, in the anchor's title")
            test.compare(test.says("heroSubtitle"), "4.0 kB")
            test.compare(test.anchor().glyph, "photo", "their kind in the disc")
            test.compare(test.anchor().mode, "looking", "the anchor stays put, still looking")
            test.verify(!test.find("chooseFiles").visible && test.find("addMore").visible
                        && test.find("clearPayload").visible, "Add files and Clear")
            test.compare(test.rows(), [[Ev.EVIL_NAME, "Phone · LocalSend"], ["Android", "Phone · Quick Share"],
                                       ["Car EVIL", "Paired device · Bluetooth"]],
                         "by name, the paired ones last, the protocol in the grey line")
            test.compare(test.find("lookingLabel").text, "Looking for more", "one line says discovery goes on")
            test.compare(test.find("nearbyHint").text, "Devices must be on the same Wi-Fi and ready to receive.")
            // The ambience's own icons: a phone, a paired device.
            var phone = probe.find(test.row("Android"), "rowGlyph")
            test.compare(phone.names, ["icon-m-device", "icon-m-phone"])
            test.verify(String(phone.source).indexOf("image://theme/icon-m-") === 0, "from the theme: " + phone.source)
            test.compare(probe.find(test.row("Car EVIL"), "rowGlyph").names[0], "icon-m-bluetooth-device")
            test.compare(probe.find(test.find("sendWithCode"), "rowGlyph").kind, "qr")
            test.verify(test.find("sendWithCode").visible, "and far away, with a code")
            test.verify(!probe.find(test.find("sendWithCode"), "rowSubtitle").visible, "its title says it")
            test.verifyPlainText(test.main, "the device list")
            // Clear clears it all, after a moment to change one's mind.
            test.find("clearPayload").clicked()
            var remorse = probe.find(test.main, "clearRemorse")
            test.verify(remorse.active, "a remorse first")
            remorse.cancel()
            test.compare(test.main.payload.itemCount, 2, "cancelled: still chosen")
            test.find("clearPayload").clicked()
            remorse.trigger()
            test.compare(test.main.payload.itemCount, 0)
            test.verify(test.find("chooseFiles").visible, "the one button again")
            test.compare(test.rows(), [], "the devices go with it")
            // A file from the file system: by its path, size unknown.
            test.find("chooseFiles").clicked()
            test.pick("contentPicker", [{ filePath: test.downloads + "a.txt", url: "" }])
        },
        function () {
            test.compare(test.says("heroTitle"), "a.txt", "one file: its name")
            test.compare(test.says("heroSubtitle"), "", "no size where none was said")
            test.compare(test.anchor().glyph, "document")
            // Add files: the same picker.
            test.find("addMore").clicked()
            test.compare(window.pageStack.currentPage.objectName, "contentPicker")
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
            // The tab is the send: who, and waiting for them.
            test.compare(test.anchor().mode, "waiting")
            test.compare(test.anchor().glyph, "phone", "the device's kind in the disc")
            test.compare(test.says("heroTitle"), Ev.EVIL_NAME)
            test.compare(test.says("heroSubtitle"), "Waiting for them to accept…")
            test.compare(test.says("heroLine"), "a.txt")
            test.compare(test.rows(), [], "no other device while it goes")
            test.verify(!test.find("sendWithCode").visible && !test.find("addMore").visible)
            test.verify(test.find("cancelSend").visible, "Cancel, where the buttons were")
            bridge.emitEvent(Ev.transferStarted(50, "outgoing", {}))
            bridge.emitEvent(Ev.progress(50, 500, 2000))
        },
        function () {
            test.compare(test.anchor().mode, "progress", "the ring fills")
            test.compare(test.anchor().value, 0.25)
            test.compare(probe.find(test.anchor(), "anchorPercent").text, "25%")
            test.verify(!probe.find(test.anchor(), "anchorDisc").visible, "the percentage alone in the ring")
            test.compare(test.says("heroSubtitle"), "500 B of 2.0 kB")
            test.compare(test.says("heroLine"), "a.txt")
            test.verifyPlainText(test.main, "a send under way")
            test.find("cancelSend").clicked()
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 50 })
            bridge.emitEvent(Ev.finished(50, "cancelled"))
            return 20
        },
        function () {
            test.verify(test.view.outgoing === null, "cancelled here: straight back to the devices")
            test.compare(test.main.payload.itemCount, 1, "a send that did not go keeps what was chosen")
            test.compare(test.rows().length, 3, "with everyone")
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
            var android = test.view.devices[0]
            m.items[1].clicked()
            m.menu.destroy()
            test.compare(test.lastOf("send").target, { protocol: "local_send", peer: "p2" }, "over LocalSend")
            test.compare(test.says("heroSubtitle"), "Connecting…")
            // No second send while one is on its way.
            test.view.choose(android)
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
            test.compare(test.anchor().mode, "done")
            test.compare(probe.find(test.anchor(), "anchorGlyph").kind, "check", "a check in a filled disc")
            test.compare(test.says("heroTitle"), "Sent")
            test.compare(test.says("heroSubtitle"), "a.txt to Pixel 8")
            test.verify(test.find("sendAnother").visible && test.find("sendDone").visible)
            test.verify(!test.find("cancelSend").visible)
            test.compare(test.main.payload.itemCount, 1, "what was sent stays chosen, for another device")
            return 150
        },
        function () {
            test.verify(test.view.outgoing !== null, "the check stays until a button says where to")
            test.find("sendAnother").clicked()
            test.verify(test.view.outgoing === null)
            test.compare(test.says("heroTitle"), "a.txt", "back to who to, the file still chosen")
            test.compare(test.rows().length, 4)
            // A send that fails: why, then Try again or Back.
            bridge.nextTransfer = 52
            test.row("Android").clicked()
        },
        function () {
            bridge.emitEvent(Ev.transferStarted(52, "outgoing", { protocol: "quick_share", peer: "Android" }))
            bridge.emitEvent(Ev.finished(52, "failed", null, "refused"))
        },
        function () {
            test.compare(test.anchor().mode, "failed")
            test.compare(probe.find(test.anchor(), "anchorGlyph").kind, "cross", "a cross in a red disc")
            test.compare(test.says("heroTitle"), "Not sent")
            test.compare(test.says("heroSubtitle"), "Declined.")
            test.compare(test.says("heroLine"), "a.txt")
            test.verify(test.hero().failed && probe.find(test.hero(), "heroTitle").color === Theme.errorColor, "in red")
            test.verify(test.find("sendRetry").visible && test.find("sendBack").visible)
            bridge.nextTransfer = 53
            test.find("sendRetry").clicked()
            test.compare(test.lastOf("send").target, { protocol: "quick_share", peer: "q1" }, "the same way again")
            test.compare(test.view.outgoing.name, "Android")
        },
        function () {
            bridge.emitEvent(Ev.transferStarted(53, "outgoing", { protocol: "quick_share", peer: "Android" }))
            bridge.emitEvent(Ev.finished(53, "failed"))
        },
        function () {
            test.compare(test.says("heroSubtitle"), "The connection failed or timed out.")
            test.find("sendBack").clicked()
            test.verify(test.view.outgoing === null, "Back: to the devices")
            test.compare(test.main.payload.itemCount, 1)
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
            test.compare(test.says("heroTitle"), "2 files")
            bridge.autoReply = false
            test.row(Ev.EVIL_NAME).clicked()
            bridge.emitEvent(Ev.reply(test.lastId(), false, "bad_file"))
        },
        function () {
            test.compare(probe.find(test.main, "bannerLabel").text,
                         "A file could not be read. Sukkula can only send files from Downloads, Documents, "
                         + "Music, Pictures, Videos and memory cards.")
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
            test.compare(box.description, "Works with Sukkula and croc.")
            test.compare(test.pageSays("heroSubtitle"), "Getting a code…")
            test.compare(probe.find(test.page, "anchor").mode, "waiting", "the anchor, at the same place")
            // Magic Wormhole cannot take two files.
            box.choose(0)
            test.compare(test.commandsOfType("send").length, 7, "no second send")
            test.compare(test.view.outgoing.protocol, "croc")
            bridge.emitEvent(Ev.crocCode(60, "gala-tulip-acorn"))
            bridge.emitEvent(Ev.transferStarted(60, "outgoing", { protocol: "croc", peer: "croc" }))
        },
        function () {
            test.compare(test.pageSays("heroTitle"), "gala-tulip-acorn", "the code is the title")
            test.compare(test.pageSays("heroSubtitle"), "The receiver scans it or types it in.")
            var qr = probe.find(test.page, "sendQr")
            test.verify(qr.valid && qr.visible, "croc's QR code is drawn")
            test.compare(qr.size, 21)
            test.verify(qr.parent === probe.find(test.page, "anchorArea"), "in the anchor's place")
            test.verify(!probe.find(test.page, "anchor").visible)
            test.compare(probe.find(test.page, "codeServer"), null, "which server is Settings' business")
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
            test.compare(test.pageSays("heroTitle"), "a.txt", "what, until the code is there")
            bridge.emitEvent(Ev.wormholeCode(77, "7-guitarist-revenge"))
            bridge.emitEvent(Ev.transferStarted(77, "outgoing", { protocol: "wormhole", peer: "" }))
        },
        function () {
            test.compare(test.pageSays("heroTitle"), "7-guitarist-revenge")
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
            // Receiver's app: croc. The code shown is given up for a new one.
            bridge.nextTransfer = 78
            probe.find(test.page, "theirApp").choose(1)
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 77 }, "the old code given up")
            test.compare(test.lastOf("send"), { type: "send", target: { protocol: "croc" },
                                                items: [{ kind: "file", path: test.downloads + "a.txt" }] })
        },
        function () {
            test.compare(test.view.outgoing.transferId, 78)
            test.compare(test.pageSays("heroTitle"), "a.txt", "the old code is gone")
            bridge.emitEvent(Ev.crocCode(78, "gala-tulip-acorn"))
            bridge.emitEvent(Ev.transferStarted(78, "outgoing", { protocol: "croc", peer: "croc" }))
        },
        function () {
            test.compare(test.pageSays("heroTitle"), "gala-tulip-acorn")
            bridge.emitEvent(Ev.progress(78, 500, 1000))
        },
        function () {
            // The receiver has come: the ring fills where the QR code was.
            test.verify(!probe.find(test.page, "sendQr").visible)
            var a = probe.find(test.page, "anchor")
            test.verify(a.visible)
            test.compare(a.mode, "progress")
            test.compare(probe.find(a, "anchorPercent").text, "50%")
            test.compare(test.pageSays("heroTitle"), "a.txt")
            test.compare(test.pageSays("heroSubtitle"), "500 B of 1.0 kB")
            test.verify(!probe.find(test.page, "theirApp").visible, "no switching once it goes")
            test.verify(!probe.find(test.page, "copyCode").visible)
            test.verify(probe.find(test.page, "cancelSend").visible)
            test.verifyPlainText(test.page, "a send with a code")
            // Leaving now keeps it going, on the tab.
            window.pageStack.pop()
            return 50
        },
        function () {
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 77 }, "nothing more given up")
            test.compare(test.anchor().mode, "progress", "the tab shows it the same way")
            test.compare(test.says("heroTitle"), "a.txt")
            test.compare(test.says("heroSubtitle"), "500 B of 1.0 kB")
            test.verify(!test.find("sendWithCode").visible)
            bridge.emitEvent(Ev.finished(78, "done"))
        },
        function () {
            test.compare(test.says("heroTitle"), "Sent")
            test.compare(test.says("heroSubtitle"), "a.txt")
            // Done: back to the start, nothing chosen.
            test.find("sendDone").clicked()
            test.verify(test.view.outgoing === null)
            test.compare(test.main.payload.itemCount, 0)
            test.compare(test.says("heroTitle"), "Ready to send")
            // Sent from the code page: the same buttons there.
            test.main.share([{ kind: "file", path: test.downloads + "b.txt" }])
            bridge.nextTransfer = 80
            test.find("sendWithCode").clicked()
            test.page = window.pageStack.currentPage
            bridge.emitEvent(Ev.wormholeCode(80, "8-guitarist-revenge"))
            bridge.emitEvent(Ev.transferStarted(80, "outgoing", { protocol: "wormhole", peer: "" }))
            bridge.emitEvent(Ev.progress(80, 10, 20))
            bridge.emitEvent(Ev.finished(80, "done"))
        },
        function () {
            test.compare(probe.find(test.page, "anchor").mode, "done")
            test.compare(test.pageSays("heroTitle"), "Sent")
            test.compare(test.pageSays("heroSubtitle"), "b.txt")
            test.verify(probe.find(test.page, "sendAnother").visible && probe.find(test.page, "sendDone").visible,
                        "never half a code page")
            test.verify(!probe.find(test.page, "sendQr").visible && !probe.find(test.page, "theirApp").visible)
            probe.find(test.page, "sendAnother").clicked()
            return 50
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage", "the page goes")
            test.verify(test.view.outgoing === null)
            test.compare(test.says("heroTitle"), "b.txt", "the file still chosen")
            test.verify(test.find("sendWithCode").visible, "ready for another")
            // F-C1: every protocol but Magic Wormhole and croc off.
            bridge.emitEvent(Ev.settings({ localsend: { enabled: false, pin: null },
                                           quickshare: { enabled: false, visibility: "hidden", ble_nudge: false },
                                           bluetooth: { enabled: false } }))
        },
        function () {
            test.compare(test.rows(), [], "nobody nearby")
            test.compare(test.find("nearbyHint").text, "Sending nearby is switched off in Settings.")
            test.compare(test.anchor().mode, "idle", "nothing to look for")
            test.verify(!test.find("lookingLabel").visible)
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
