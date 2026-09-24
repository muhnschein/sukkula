// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * Engine.qml against the JSON contract of api.rs: commands go out as
 * {v:1,id,cmd} with fresh ids and answer their callbacks; events fill the
 * models; anything malformed, unknown or oversized changes nothing; every
 * model is bounded.
 */
Script {
    id: test

    property var replies: []

    Engine {
        id: engine
        backend: bridge
    }

    function last() {
        return bridge.lastCommand()
    }

    steps: [
        function () {
            test.compare(bridge.startCount, 1, "start() once, after connecting")
        },
        function () {
            test.verify(engine.running, "running after started")
            test.compare(engine.version, "9.9.9-fake")
            test.compare(engine.protocols, ["local_send", "quick_share", "wormhole", "bluetooth"])
            test.verify(engine.settingsKnown, "settings known")
            test.compare(engine.effectiveDeviceName, "Jolla Phone")
            test.compare(engine.receiving, false)
            test.compare(engine.protocolStatuses.length, 4)
            test.compare(engine.protocolStatuses[2].state, "send_only")
        },
        function () {
            // Commands: the envelope, and ids that never repeat.
            var a = engine.setReceiving(true, function (ok) { test.replies.push(["a", ok]) })
            var b = engine.getSettings()
            test.verify(a > 0 && b === a + 1, "ids count up: " + a + ", " + b)
            var cmds = bridge.parsedCommands()
            var first = cmds[cmds.length - 2]
            test.compare(first, { v: 1, id: a, cmd: { type: "set_receiving", on: true } })
            test.compare(cmds[cmds.length - 1].cmd, { type: "get_settings" })
        },
        function () {
            test.compare(test.replies, [["a", true]], "the callback got its reply")
            // A refused reply reaches the callback with its error.
            bridge.autoReply = false
            var id = engine.receiveWormhole("7-guitarist-revenge", function (ok, error, transfer) {
                test.pending = { ok: ok, code: error ? error.code : "", transfer: transfer }
            })
            test.compare(last().cmd, { type: "receive_wormhole", code: "7-guitarist-revenge" })
            bridge.emitEvent(Ev.reply(id, false, "bad_code"))
        },
        function () {
            test.compare(test.pending, { ok: false, code: "bad_code", transfer: -1 })
            // A reply nobody waits for, failed: announced, translated.
            test.failedMessages = []
            engine.failed.connect(test.noteFailure)
            bridge.emitEvent(Ev.reply(9999, false, "storage"))
            // An engine that refuses the command outright: -3 is too large.
            bridge.commandResult = -3
            var r = null
            var id = engine.send({ protocol: "wormhole" }, [{ kind: "text", text: "x" }],
                                 function (ok, error) { r = error.code })
            test.compare(id, 0, "not taken")
            test.compare(r, "too_large", "callback told at once")
            bridge.commandResult = 0
            bridge.autoReply = true
        },
        function () {
            test.compare(test.failedMessages, [engine.errorText({ code: "storage" })])
            engine.failed.disconnect(test.noteFailure)
            test.verify(engine.errorText({ code: "no_such_code" }) === engine.errorText({ code: "internal" }),
                        "unknown codes read as internal")
        },
        function () {
            // Offers: the view is copied, capped and counted.
            bridge.emitEvent(Ev.offer(7, {}))
            bridge.emitEvent(Ev.offer(7, {})) // the same id again: ignored
        },
        function () {
            test.compare(engine.offers.count, 1)
            var o = engine.offer(7)
            test.compare(o.sender, Ev.EVIL_NAME, "sender verbatim, for a plain-text label")
            test.compare(o.files.length, 2)
            test.compare(o.moreFiles, 3)
            test.compare(o.fileCount, 5)
            test.compare(o.pin, "4821")
            test.verify(o.expiresAt - Date.now() <= 60000 && o.expiresAt - Date.now() > 55000, "60 s")
            // A lying offer: a far future expiry is held to 60 s, a
            // negative size reads 0, 600 files are cut to 50 listed.
            var many = []
            for (var i = 0; i < 600; i++) {
                many.push({ name: "f" + i, size: -5 })
            }
            bridge.emitEvent(Ev.offer(8, { expires_in: 1e12, files: many, file_count: 600,
                                           more_files: 0, total_bytes: -1, pin: 1234 }))
        },
        function () {
            var o = engine.offer(8)
            test.verify(o !== null, "offer 8 kept")
            test.verify(o.expiresAt - Date.now() <= 60000, "expiry capped at 60 s")
            test.compare(o.files.length, 50)
            test.compare(o.files[0].size, 0)
            test.compare(o.moreFiles, 550)
            test.compare(o.totalBytes, 0)
            test.compare(o.pin, "", "a PIN that is not a string is dropped")
            // More than four waiting: the rest are not queued.
            for (var i = 9; i < 20; i++) {
                bridge.emitEvent(Ev.offer(i, {}))
            }
        },
        function () {
            test.compare(engine.offers.count, 4)
            // Answering takes it off the queue at once.
            engine.answer(7, true)
            test.compare(last().cmd, { type: "answer", offer: 7, accept: true })
            test.compare(engine.offers.count, 3)
            test.verify(engine.offer(7) === null, "gone")
            var closed = []
            engine.offerClosed.connect(function (id, reason) { closed.push([id, reason]) })
            test.closed = closed
            bridge.emitEvent(Ev.offerClosed(8, "timed_out"))
            bridge.emitEvent(Ev.offerClosed(9, "nonsense"))
        },
        function () {
            test.compare(test.closed, [[8, "timed_out"], [9, ""]])
            test.compare(engine.offers.count, 1)
        },
        function () {
            // Transfers.
            bridge.emitEvent(Ev.transferStarted(1, "incoming", {}))
            bridge.emitEvent(Ev.progress(1, 500, 2000))
        },
        function () {
            test.compare(engine.transfers.count, 1)
            var t = engine.transfer(1)
            test.compare(t.peer, Ev.EVIL_NAME)
            test.compare(t.files, Ev.EVIL_FILE + "\nb.pdf")
            test.compare(t.bytes, 500)
            test.compare(t.state, "active")
            test.compare(engine.activeTransfers, 1)
            test.compare(engine.activeBytes, 500)
            // Progress past the total is held at the total; NaN and
            // negatives read 0.
            bridge.emitEvent(Ev.progress(1, 99999, 2000))
            bridge.emitEvent('{"type":"transfer_progress","transfer":1,"bytes":-1,"total":"x"}')
        },
        function () {
            var t = engine.transfer(1)
            test.compare(t.total, 0)
            test.compare(t.bytes, 0)
            bridge.emitEvent(Ev.progress(1, 1000, 2000))
            bridge.emitEvent(Ev.finished(1, "done", [Ev.EVIL_FILE, "b (1).pdf"]))
            bridge.emitEvent(Ev.progress(1, 5, 2000)) // after the end: ignored
        },
        function () {
            var t = engine.transfer(1)
            test.compare(t.state, "done")
            test.compare(t.bytes, 2000)
            test.compare(t.savedCount, 2)
            test.compare(t.saved, Ev.EVIL_FILE + "\nb (1).pdf")
            test.compare(engine.activeTransfers, 0)
            bridge.emitEvent(Ev.transferStarted(2, "outgoing", { protocol: "bluetooth" }))
            bridge.emitEvent(Ev.finished(2, "failed", null, "refused"))
            bridge.emitEvent('{"type":"transfer_finished","transfer":2,"outcome":"weird"}')
        },
        function () {
            var t = engine.transfer(2)
            test.compare(t.state, "failed")
            test.compare(t.error, "refused")
            // Cancel is a command, nothing more.
            engine.cancel(1)
            test.compare(last().cmd, { type: "cancel", transfer: 1 })
            // Bounded: 150 finished transfers keep the newest 100.
            for (var i = 10; i < 160; i++) {
                bridge.emitEvent(Ev.transferStarted(i, "incoming", {}))
                bridge.emitEvent(Ev.finished(i, "cancelled"))
            }
            return 200
        },
        function () {
            test.compare(engine.transfers.count, 100)
            test.compare(engine.transfers.get(0).transferId, 159, "newest first")
            // Texts, bounded to 50.
            for (var i = 0; i < 60; i++) {
                bridge.emitEvent(Ev.textReceived(i, "text " + i))
            }
            bridge.emitEvent('{"type":"text_received","transfer":1,"from":"x","text":""}')
            return 100
        },
        function () {
            test.compare(engine.texts.count, 50)
            test.compare(engine.texts.get(0).text, "text 59")
            test.compare(engine.texts.get(0).from, Ev.EVIL_NAME)
            // Peers, by protocol; lost; bounded.
            bridge.emitEvent(Ev.peerFound("p1", "local_send"))
            bridge.emitEvent(Ev.peerFound("p2", "quick_share"))
            bridge.emitEvent(Ev.peerFound("p3", "wormhole")) // no peers there
            bridge.emitEvent(Ev.peerFound("p1", "local_send", "Renamed"))
        },
        function () {
            test.compare(engine.localSendPeers.count, 1)
            test.compare(engine.localSendPeers.get(0).name, "Renamed")
            test.compare(engine.quickSharePeers.count, 1)
            bridge.emitEvent(Ev.peerLost("p2"))
            for (var i = 0; i < 150; i++) {
                bridge.emitEvent(Ev.peerFound("many" + i, "local_send"))
            }
            return 100
        },
        function () {
            test.compare(engine.quickSharePeers.count, 0)
            test.compare(engine.localSendPeers.count, 100)
            engine.stopDiscovery()
            test.compare(engine.localSendPeers.count, 0, "stopping forgets the peers")
            // Wormhole codes and their QR, checked for shape.
            bridge.emitEvent(Ev.wormholeCode(5, "7-guitarist-revenge"))
            bridge.emitEvent(Ev.wormholeCode(6, "8-a-b", { size: 21, rows: ["1"] }))
            bridge.emitEvent(Ev.wormholeCode(7, "9-c-d", { size: 200, rows: [] }))
            var bad = Ev.qr21()
            bad.rows[3] = bad.rows[3].substring(0, 20) + "2"
            bridge.emitEvent(Ev.wormholeCode(8, "10-e-f", bad))
        },
        function () {
            test.compare(engine.wormholeCode(5).code, "7-guitarist-revenge")
            test.compare(engine.wormholeCode(5).qr.size, 21)
            test.compare(engine.wormholeCode(6).qr, null)
            test.compare(engine.wormholeCode(7).qr, null)
            test.compare(engine.wormholeCode(8).qr, null)
            test.compare(engine.wormholeCode(8).code, "10-e-f", "the code stands without its QR")
            // Bluetooth: bad addresses are dropped, the list replaced.
            bridge.emitEvent(Ev.bluetoothDevices([
                { address: "aa:bb:cc:dd:ee:ff", name: "Car" },
                { address: "not-an-address", name: "Evil" },
                { address: "11:22:33:44:55:66" }
            ]))
        },
        function () {
            test.compare(engine.bluetoothDevices.count, 2)
            test.compare(engine.bluetoothDevices.get(0).address, "AA:BB:CC:DD:EE:FF")
            test.compare(engine.bluetoothDevices.get(1).name, "")
            // Garbage of every kind changes nothing and throws nothing.
            var before = JSON.stringify([engine.offers.count, engine.transfers.count, engine.texts.count,
                                         engine.receiving, engine.running])
            var junk = ["", "null", "[]", "42", "\"x\"", "{", "{\"type\":5}", "{\"type\":\"nope\"}",
                        "{\"type\":\"offer_pending\"}", "{\"type\":\"offer_pending\",\"offer\":[]}",
                        "{\"type\":\"offer_pending\",\"offer\":{\"id\":-1,\"protocol\":\"local_send\"}}",
                        "{\"type\":\"offer_pending\",\"offer\":{\"id\":1.5,\"protocol\":\"local_send\"}}",
                        "{\"type\":\"offer_pending\",\"offer\":{\"id\":3,\"protocol\":\"airdrop\"}}",
                        "{\"type\":\"transfer_started\",\"transfer\":{\"id\":3,\"direction\":\"sideways\",\"protocol\":\"local_send\"}}",
                        "{\"type\":\"receiving\",\"on\":\"yes\",\"protocols\":{}}",
                        "{\"type\":\"settings\",\"settings\":[]}",
                        "{\"type\":\"reply\",\"id\":{},\"ok\":true}",
                        "{\"type\":\"bluetooth_devices\",\"devices\":\"x\"}",
                        "{\"type\":\"text_received\",\"text\":{\"a\":1}}"]
            for (var i = 0; i < junk.length; i++) {
                engine.handleEvent(junk[i])
            }
            engine.handleEvent(null)
            engine.handleEvent(new Array(1048578).join("x"))
            var after = JSON.stringify([engine.offers.count, engine.transfers.count, engine.texts.count,
                                        engine.receiving, engine.running])
            // "receiving" with on:"yes" reads as off, which it already was.
            test.compare(after, before, "garbage changed nothing")
            test.verify(engine.settingsKnown && !Array.isArray(engine.settings), "settings kept")
        },
        function () {
            // A fatal event stops everything that needs the engine.
            engine.handleEvent('{"type":"fatal","error":{"code":"storage","message":"disk full"}}')
            test.compare(engine.running, false)
            test.compare(engine.fatalCode, "storage")
            test.compare(engine.fatalDetail, "disk full")
        }
    ]

    // Handed between steps.
    property var pending: null
    property var closed: []
    property var failedMessages: []
    function noteFailure(message) {
        var next = test.failedMessages.slice(0)
        next.push(message)
        test.failedMessages = next
    }

    // Something from qml/ must be on screen for the runner's check; the
    // engine has no view, so a plain-text label stands in.
    Loader {
        source: "../../qml/components/Banner.qml"
        onLoaded: item.show("engine test")
    }
}
