// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * The engine as the pages see it: commands out, events in, and the state
 * they bind to.
 *
 * The only code that speaks the JSON contract of
 * crates/sukkula-engine/src/api.rs. Commands are `{v:1,id,cmd}` with an id
 * assigned here; every command is answered by one "reply" carrying it,
 * which goes to the callback the caller passed, if any.
 *
 * Events come from our own engine, which has already put every
 * peer-supplied string through sukkula-core (S1, S2). They are still read
 * here as if they were not: parsed inside try/catch, unknown types
 * ignored, every field type-checked and length-capped before a model sees
 * it, and every model bounded. A malformed event changes nothing. Pages
 * show what lands here with `textFormat: Text.PlainText` (S2).
 *
 * Qt 5.6 JavaScript: ES5 only -- no let/const, arrow functions or
 * template strings.
 */
QtObject {
    id: engine

    /// The C++ Bridge (src/bridge.h), or a test's stand-in: `start()`,
    /// `command(json)`, `stop()`, `version`, and the signal `event(json)`.
    property QtObject backend: null

    /// Started and not failed.
    property bool running: false
    /// Set when the engine could not start: the error code, and the
    /// engine's English detail for a secondary line.
    property string fatalCode: ""
    property string fatalDetail: ""
    /// The engine's version, from "started".
    property string version: ""
    /// The protocols this build contains, e.g. ["local_send", "wormhole"].
    property var protocols: []

    /// Receive mode (F-C1), and each protocol's state.
    property bool receiving: false
    /// [{protocol, state, errorCode, errorDetail}] from "receiving".
    property var protocolStatuses: []

    /// The settings as the engine last reported them, and whether it has.
    property var settings: ({
        device_name: "",
        localsend: { enabled: true, pin: null },
        quickshare: { enabled: true, visibility: "everyone", ble_nudge: true },
        wormhole: { enabled: true, mailbox_url: null, relay_url: null },
        bluetooth: { enabled: true },
        logging: false
    })
    property bool settingsKnown: false
    /// The name peers see (F-C7).
    property string effectiveDeviceName: ""

    /// Transfers in progress, for KeepAlive and the cover.
    property int activeTransfers: 0
    property real activeBytes: 0
    property real activeTotal: 0

    /// Transfers, newest first. Roles: transferId, direction, protocol,
    /// peer, files (names, one per line), fileCount, total, bytes, state
    /// ("active", "done", "cancelled", "failed"), error (a code), saved
    /// (names, one per line), savedCount.
    property ListModel transfers: ListModel {}
    /// Peers to send to, one model per protocol that discovers them.
    /// Roles: peerId, protocol, name, deviceModel, deviceType. (Not
    /// "model": a role of that name would hide a delegate's `model`.)
    property ListModel localSendPeers: ListModel {}
    property ListModel quickSharePeers: ListModel {}
    /// Offers waiting for the user (F-C2), oldest first. Roles: offerId,
    /// protocol, sender, fileCount, totalBytes, expiresAt. The whole offer
    /// is `offer(offerId)`.
    property ListModel offers: ListModel {}
    /// Paired Bluetooth devices that take Object Push. Roles: address, name.
    property ListModel bluetoothDevices: ListModel {}
    /// How many pages have asked for discovery and not yet given it back
    /// (startDiscovery / stopDiscovery).
    property int discoveryUsers: 0
    /// Received texts (F-C4), newest first. Roles: transferId, from, text,
    /// receivedAt.
    property ListModel texts: ListModel {}

    /// An offer left the queue on the engine's side: answered elsewhere,
    /// timed out, withdrawn by the sender, or the engine stopping.
    signal offerClosed(var offerId, string reason)
    /// A new offer is waiting.
    signal offerArrived(var offerId)
    /// A transfer changed: started, progressed or finished.
    signal transferUpdated(var transferId)
    /// A transfer ended; `state` is "done", "cancelled" or "failed".
    signal transferEnded(var transferId, string direction, string state)
    /// A wormhole send has its code (F-MW1): see `wormholeCode()`.
    signal wormholeCodeArrived(var transferId)
    /// A command without a callback failed; `message` is translated.
    signal failed(string message)

    // Bounds on what the UI keeps. The engine bounds its own events; these
    // bound how many of them pile up over a long session.
    readonly property int maxTransfers: 100
    readonly property int maxPeers: 100
    readonly property int maxOffers: 4
    readonly property int maxTexts: 50
    readonly property int maxDevices: 100
    readonly property int maxPending: 256
    // Longest event read at all; the bridge already drops longer ones.
    readonly property int maxEventChars: 1048576
    // Most files listed per offer, as the engine's MAX_LISTED_FILES.
    readonly property int maxListedFiles: 50
    // The engine declines an unanswered offer after 60 s (F-C3); the
    // dialog never counts longer than that, whatever an event says.
    readonly property int offerTimeoutSeconds: 60

    property int _nextId: 1
    property var _callbacks: ({})
    property var _callbackOrder: []
    property var _offerData: ({})
    property var _codes: ({})

    Component.onCompleted: engine._attach()
    onBackendChanged: engine._attach()

    property QtObject _attached: null
    function _attach() {
        if (engine._attached === engine.backend) {
            return
        }
        if (engine._attached) {
            engine._attached.event.disconnect(engine.handleEvent)
        }
        engine._attached = engine.backend
        if (!engine.backend) {
            return
        }
        // Listening first, then starting: the engine's first events
        // ("started", "settings", "receiving") are posted during start().
        engine.backend.event.connect(engine.handleEvent)
        engine.version = engine._str(engine.backend.version, 32)
        engine.backend.start()
    }

    // ---- Commands -------------------------------------------------------

    /// Sends one command. `callback(ok, error, transfer)` runs when its
    /// reply arrives -- or at once, when the engine did not take it.
    /// Returns the id, or 0 when it was not taken.
    function command(cmd, callback) {
        var id = engine._nextId
        engine._nextId = id + 1
        var json = JSON.stringify({ v: 1, id: id, cmd: cmd })
        var rc = -1
        if (engine.backend) {
            rc = engine.backend.command(json)
        }
        if (rc !== 0) {
            var error = engine._bridgeError(rc)
            if (callback) {
                callback(false, error, -1)
            } else {
                engine.failed(engine.errorText(error))
            }
            return 0
        }
        if (callback) {
            engine._callbacks[id] = callback
            engine._callbackOrder.push(id)
            // Every command is answered, so this only bounds a fault.
            while (engine._callbackOrder.length > engine.maxPending) {
                delete engine._callbacks[engine._callbackOrder.shift()]
            }
        }
        return id
    }

    function _quiet() {}

    function setReceiving(on, callback) {
        return engine.command({ type: "set_receiving", on: on === true }, callback)
    }
    function setSettings(settings, callback) {
        return engine.command({ type: "set_settings", settings: settings }, callback)
    }
    function getSettings() {
        return engine.command({ type: "get_settings" })
    }
    /// Asks for discovery (F-LS1, F-QS1); every call is given back by one
    /// stopDiscovery(). Counted, because two holders can overlap: one that
    /// replaces another asks before the old one is destroyed, and without
    /// the count the old one's stop came last and left the new one looking
    /// at empty lists with discovery off. The main page holds it while in
    /// Send mode.
    /// start_discovery goes out every time: for what already runs it is a
    /// no-op, and it starts a protocol switched on since.
    function startDiscovery() {
        engine.discoveryUsers = engine.discoveryUsers + 1
        return engine.command({ type: "start_discovery" }, engine._quiet)
    }
    /// Gives discovery back. The last one out stops it and forgets the
    /// peers, since nothing keeps them fresh then; a stop nobody asked
    /// for does nothing.
    function stopDiscovery() {
        if (engine.discoveryUsers <= 0) {
            return 0
        }
        engine.discoveryUsers = engine.discoveryUsers - 1
        if (engine.discoveryUsers > 0) {
            return 0
        }
        engine.localSendPeers.clear()
        engine.quickSharePeers.clear()
        return engine.command({ type: "stop_discovery" }, engine._quiet)
    }
    /// Answers an offer (F-C2). It leaves the queue here at once, so the
    /// same offer is never shown twice. Only an offer still waiting is
    /// answered: one the engine closed (timed out, withdrawn) or that was
    /// answered already gets nothing, whatever a stale dialog asks.
    /// Returns the command id, or 0 when nothing was sent.
    function answer(offerId, accept) {
        if (engine.offer(offerId) === null) {
            return 0
        }
        engine._removeOffer(offerId)
        return engine.command({ type: "answer", offer: offerId, accept: accept === true },
                              engine._quiet)
    }
    /// `target` is {protocol: "local_send", peer} and so on; `items` is
    /// [{kind: "file", path} | {kind: "text", text}].
    function send(target, items, callback) {
        return engine.command({ type: "send", target: target, items: items }, callback)
    }
    function receiveWormhole(code, callback) {
        return engine.command({ type: "receive_wormhole", code: code }, callback)
    }
    function cancel(transferId, callback) {
        return engine.command({ type: "cancel", transfer: transferId },
                              callback ? callback : engine._quiet)
    }
    function listBluetoothDevices(callback) {
        return engine.command({ type: "list_bluetooth_devices" }, callback)
    }

    // ---- Lookups --------------------------------------------------------

    /// The whole offer, as the consent dialog shows it, or null.
    function offer(offerId) {
        var o = engine._offerData[offerId]
        return o ? o : null
    }
    /// The first waiting offer, or null.
    function firstOffer() {
        return engine.offers.count > 0 ? engine.offer(engine.offers.get(0).offerId) : null
    }
    /// {code, qr} for a wormhole send, or null; `qr` is {size, rows} or
    /// null when the engine's was unusable.
    function wormholeCode(transferId) {
        var c = engine._codes[transferId]
        return c ? c : null
    }
    /// A copy of one transfer's row, or null.
    function transfer(transferId) {
        var i = engine._indexOf(engine.transfers, "transferId", transferId)
        if (i < 0) {
            return null
        }
        var row = engine.transfers.get(i)
        return {
            transferId: row.transferId, direction: row.direction, protocol: row.protocol,
            peer: row.peer, files: row.files, fileCount: row.fileCount, total: row.total,
            bytes: row.bytes, state: row.state, error: row.error, saved: row.saved,
            savedCount: row.savedCount
        }
    }
    function peerModel(protocol) {
        if (protocol === "local_send") {
            return engine.localSendPeers
        }
        if (protocol === "quick_share") {
            return engine.quickSharePeers
        }
        return null
    }
    function hasProtocol(protocol) {
        return engine.protocols.indexOf(protocol) >= 0
    }
    /// In this build and not switched off in the settings.
    function protocolEnabled(protocol) {
        if (!engine.hasProtocol(protocol)) {
            return false
        }
        var s = engine.settings
        if (protocol === "local_send") {
            return !(s.localsend && s.localsend.enabled === false)
        }
        if (protocol === "quick_share") {
            return !(s.quickshare && s.quickshare.enabled === false)
        }
        if (protocol === "wormhole") {
            return !(s.wormhole && s.wormhole.enabled === false)
        }
        if (protocol === "bluetooth") {
            return !(s.bluetooth && s.bluetooth.enabled === false)
        }
        return true
    }
    function clearTexts() {
        engine.texts.clear()
    }
    /// Takes finished transfers off the list.
    function clearFinished() {
        for (var i = engine.transfers.count - 1; i >= 0; i--) {
            if (engine.transfers.get(i).state !== "active") {
                engine.transfers.remove(i)
            }
        }
    }

    // ---- Words ----------------------------------------------------------

    function protocolName(protocol) {
        switch (protocol) {
        case "local_send": return "LocalSend"
        case "quick_share": return "Quick Share"
        case "wormhole": return "Magic Wormhole"
        case "bluetooth": return "Bluetooth"
        }
        return ""
    }
    function stateText(state) {
        switch (state) {
        //: A protocol's receiver is switched off.
        case "off": return qsTr("Off")
        //: A protocol's receiver is starting up.
        case "starting": return qsTr("Starting")
        //: A protocol's receiver is listening for offers.
        case "ready": return qsTr("Ready")
        //: A protocol's receiver could not start.
        case "failed": return qsTr("Failed")
        //: The protocol can only send from this phone (Bluetooth, Wormhole).
        case "send_only": return qsTr("Send only")
        }
        return ""
    }
    /// A translated sentence for an error, by its code; never the engine's
    /// English `message`, which pages show only as a detail line.
    function errorText(error) {
        var code = error && typeof error.code === "string" ? error.code : ""
        switch (code) {
        case "bad_command":
        case "bad_version":
            return qsTr("Sukkula could not understand its own request.")
        case "unavailable":
            return qsTr("Not available. Is it switched off in Settings?")
        case "not_found":
            return qsTr("It is gone. The other device may have left.")
        case "bad_settings":
            return qsTr("A setting could not be used.")
        case "bad_file":
            // Mostly Sailjail: only Downloads is granted (spec §2), and a
            // file shared from elsewhere may be out of reach.
            return qsTr("A file could not be read. Sukkula can read files in Downloads only.")
        case "too_large":
            return qsTr("Too large, or too many files.")
        case "refused":
            return qsTr("Declined.")
        case "peer_mismatch":
            return qsTr("The other device is not the one it claimed to be.")
        case "bad_code":
            return qsTr("That code does not work.")
        case "network":
            return qsTr("Network error or timeout.")
        case "storage":
            return qsTr("Could not save. Is the storage full?")
        }
        return qsTr("Something went wrong.")
    }

    // ---- Events ---------------------------------------------------------

    /// One event from the engine, as JSON. Anything unexpected is dropped.
    function handleEvent(json) {
        if (typeof json !== "string" || json.length > engine.maxEventChars) {
            return
        }
        var e
        try {
            e = JSON.parse(json)
        } catch (err) {
            return
        }
        if (!e || typeof e !== "object" || typeof e.type !== "string") {
            return
        }
        try {
            engine._dispatch(e)
        } catch (err2) {
            // A field of the wrong shape somewhere deep: the event is
            // dropped, and what it had already changed stays consistent
            // because every handler checks before it writes.
        }
    }

    function _dispatch(e) {
        switch (e.type) {
        case "fatal": engine._onFatal(e); break
        case "started": engine._onStarted(e); break
        case "reply": engine._onReply(e); break
        case "settings": engine._onSettings(e); break
        case "receiving": engine._onReceiving(e); break
        case "peer_found": engine._onPeerFound(e); break
        case "peer_lost": engine._onPeerLost(e); break
        case "offer_pending": engine._onOfferPending(e); break
        case "offer_closed": engine._onOfferClosed(e); break
        case "transfer_started": engine._onTransferStarted(e); break
        case "transfer_progress": engine._onTransferProgress(e); break
        case "transfer_finished": engine._onTransferFinished(e); break
        case "text_received": engine._onTextReceived(e); break
        case "wormhole_code": engine._onWormholeCode(e); break
        case "bluetooth_devices": engine._onBluetoothDevices(e); break
        default: break // A newer engine's event: not ours to guess at.
        }
    }

    function _onFatal(e) {
        var error = engine._error(e.error)
        engine.running = false
        engine.fatalCode = error.code
        engine.fatalDetail = error.message
    }

    function _onStarted(e) {
        engine.running = true
        engine.fatalCode = ""
        engine.fatalDetail = ""
        engine.version = engine._str(e.version, 32)
        var known = []
        if (Array.isArray(e.protocols)) {
            for (var i = 0; i < e.protocols.length && i < 8; i++) {
                var p = engine._protocol(e.protocols[i])
                if (p !== "" && known.indexOf(p) < 0) {
                    known.push(p)
                }
            }
        }
        engine.protocols = known
    }

    function _onReply(e) {
        var id = engine._key(e.id)
        var ok = e.ok === true
        var error = ok ? null : engine._error(e.error)
        var callback = id > 0 ? engine._callbacks[id] : undefined
        if (callback) {
            delete engine._callbacks[id]
            var at = engine._callbackOrder.indexOf(id)
            if (at >= 0) {
                engine._callbackOrder.splice(at, 1)
            }
            callback(ok, error, engine._key(e.transfer))
        } else if (!ok) {
            engine.failed(engine.errorText(error))
        }
    }

    function _onSettings(e) {
        if (!e.settings || typeof e.settings !== "object" || Array.isArray(e.settings)) {
            return
        }
        engine.settings = e.settings
        engine.settingsKnown = true
        engine.effectiveDeviceName = engine._str(e.effective_device_name, 128)
    }

    function _onReceiving(e) {
        var statuses = []
        if (Array.isArray(e.protocols)) {
            for (var i = 0; i < e.protocols.length && i < 8; i++) {
                var s = e.protocols[i]
                if (!s || typeof s !== "object") {
                    continue
                }
                var p = engine._protocol(s.protocol)
                var state = engine._oneOf(s.state, ["off", "starting", "ready", "failed", "send_only"])
                if (p === "" || state === "") {
                    continue
                }
                var error = s.error ? engine._error(s.error) : null
                statuses.push({
                    protocol: p,
                    state: state,
                    errorCode: error ? error.code : "",
                    errorDetail: error ? error.message : ""
                })
            }
        }
        engine.receiving = e.on === true
        engine.protocolStatuses = statuses
    }

    function _onPeerFound(e) {
        var peer = e.peer
        if (!peer || typeof peer !== "object") {
            return
        }
        var model = engine.peerModel(engine._protocol(peer.protocol))
        var id = engine._str(peer.id, 256)
        if (!model || id === "") {
            return
        }
        var row = {
            peerId: id,
            protocol: engine._protocol(peer.protocol),
            name: engine._str(peer.name, 128),
            deviceModel: engine._str(peer.model, 128),
            deviceType: engine._oneOf(peer.device_type, ["phone", "tablet", "computer", "unknown"])
        }
        var i = engine._indexOf(model, "peerId", id)
        if (i >= 0) {
            model.set(i, row)
        } else if (model.count < engine.maxPeers) {
            model.append(row)
        }
    }

    function _onPeerLost(e) {
        var id = engine._str(e.peer, 256)
        var models = [engine.localSendPeers, engine.quickSharePeers]
        for (var m = 0; m < models.length; m++) {
            var i = engine._indexOf(models[m], "peerId", id)
            if (i >= 0) {
                models[m].remove(i)
            }
        }
    }

    function _onOfferPending(e) {
        var o = e.offer
        if (!o || typeof o !== "object") {
            return
        }
        var id = engine._key(o.id)
        var protocol = engine._protocol(o.protocol)
        if (id < 0 || protocol === "" || engine._offerData[id]) {
            return
        }
        // The engine lets two wait (F-C3); a UI with more than a handful
        // stacked up is being flooded, and the rest time out on their own.
        if (engine.offers.count >= engine.maxOffers) {
            return
        }
        var files = []
        if (Array.isArray(o.files)) {
            for (var i = 0; i < o.files.length && i < engine.maxListedFiles; i++) {
                var f = o.files[i]
                if (f && typeof f === "object") {
                    files.push({ name: engine._str(f.name, 255), size: engine._count(f.size) })
                }
            }
        }
        var fileCount = Math.max(engine._count(o.file_count), files.length)
        var expiresIn = Math.min(engine._count(o.expires_in), engine.offerTimeoutSeconds)
        var view = {
            offerId: id,
            protocol: protocol,
            sender: engine._str(o.sender, 128),
            model: engine._str(o.model, 128),
            files: files,
            moreFiles: Math.max(engine._count(o.more_files), fileCount - files.length),
            fileCount: fileCount,
            totalBytes: engine._count(o.total_bytes),
            hasText: o.has_text === true,
            pin: engine._str(o.pin, 16),
            expiresAt: Date.now() + expiresIn * 1000
        }
        engine._offerData[id] = view
        engine.offers.append({
            offerId: id,
            protocol: protocol,
            sender: view.sender,
            fileCount: view.fileCount,
            totalBytes: view.totalBytes,
            expiresAt: view.expiresAt
        })
        engine.offerArrived(id)
    }

    function _onOfferClosed(e) {
        var id = engine._key(e.offer)
        if (id < 0) {
            return
        }
        engine._removeOffer(id)
        engine.offerClosed(id, engine._oneOf(e.reason,
            ["accepted", "declined", "timed_out", "withdrawn", "shutdown"]))
    }

    function _removeOffer(id) {
        delete engine._offerData[id]
        var i = engine._indexOf(engine.offers, "offerId", id)
        if (i >= 0) {
            engine.offers.remove(i)
        }
    }

    function _onTransferStarted(e) {
        var t = e.transfer
        if (!t || typeof t !== "object") {
            return
        }
        var id = engine._key(t.id)
        var direction = engine._oneOf(t.direction, ["incoming", "outgoing"])
        var protocol = engine._protocol(t.protocol)
        if (id < 0 || direction === "" || protocol === "") {
            return
        }
        var names = []
        if (Array.isArray(t.files)) {
            for (var i = 0; i < t.files.length && i < engine.maxListedFiles; i++) {
                var f = t.files[i]
                if (f && typeof f === "object") {
                    names.push(engine._str(f.name, 255))
                }
            }
        }
        var row = {
            transferId: id,
            direction: direction,
            protocol: protocol,
            peer: engine._str(t.peer, 128),
            files: names.join("\n"),
            fileCount: Math.max(engine._count(t.file_count), names.length),
            total: engine._count(t.total_bytes),
            bytes: 0,
            state: "active",
            error: "",
            saved: "",
            savedCount: 0
        }
        var at = engine._indexOf(engine.transfers, "transferId", id)
        if (at >= 0) {
            engine.transfers.set(at, row)
        } else {
            engine.transfers.insert(0, row)
            engine._trimTransfers()
        }
        engine._recount()
        engine.transferUpdated(id)
    }

    function _onTransferProgress(e) {
        var id = engine._key(e.transfer)
        var i = engine._indexOf(engine.transfers, "transferId", id)
        if (i < 0 || engine.transfers.get(i).state !== "active") {
            return
        }
        var total = engine._count(e.total)
        var bytes = Math.min(engine._count(e.bytes), total)
        engine.transfers.setProperty(i, "total", total)
        engine.transfers.setProperty(i, "bytes", bytes)
        engine._recount()
        engine.transferUpdated(id)
    }

    function _onTransferFinished(e) {
        var id = engine._key(e.transfer)
        var i = engine._indexOf(engine.transfers, "transferId", id)
        // A transfer ends once; a second ending is not news.
        if (i < 0 || engine.transfers.get(i).state !== "active") {
            return
        }
        var outcome = e.outcome && typeof e.outcome === "object" ? e.outcome : {}
        var state = "failed"
        if (outcome.result === "done") {
            state = "done"
        } else if (outcome.result === "cancelled") {
            state = "cancelled"
        }
        var saved = []
        if (Array.isArray(e.saved)) {
            for (var s = 0; s < e.saved.length && s < 500; s++) {
                var name = engine._str(e.saved[s], 255)
                if (name !== "") {
                    saved.push(name)
                }
            }
        }
        var row = engine.transfers.get(i)
        if (state === "done" && row.total > 0) {
            engine.transfers.setProperty(i, "bytes", row.total)
        }
        engine.transfers.setProperty(i, "state", state)
        engine.transfers.setProperty(i, "error", state === "failed" ? engine._error(outcome.error).code : "")
        engine.transfers.setProperty(i, "saved", saved.join("\n"))
        engine.transfers.setProperty(i, "savedCount", saved.length)
        engine._recount()
        engine.transferUpdated(id)
        engine.transferEnded(id, row.direction, state)
    }

    function _onTextReceived(e) {
        var text = engine._str(e.text, 65536)
        if (text === "") {
            return
        }
        engine.texts.insert(0, {
            transferId: engine._key(e.transfer),
            from: engine._str(e.from, 128),
            text: text,
            receivedAt: Date.now()
        })
        while (engine.texts.count > engine.maxTexts) {
            engine.texts.remove(engine.texts.count - 1)
        }
    }

    function _onWormholeCode(e) {
        var id = engine._key(e.transfer)
        var code = engine._str(e.code, 128)
        if (id < 0 || code === "") {
            return
        }
        engine._codes[id] = { code: code, qr: engine._qr(e.qr) }
        engine.wormholeCodeArrived(id)
    }

    function _onBluetoothDevices(e) {
        engine.bluetoothDevices.clear()
        if (!Array.isArray(e.devices)) {
            return
        }
        var mac = /^[0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){5}$/
        for (var i = 0; i < e.devices.length && engine.bluetoothDevices.count < engine.maxDevices; i++) {
            var d = e.devices[i]
            if (!d || typeof d !== "object" || typeof d.address !== "string" || !mac.test(d.address)) {
                continue
            }
            engine.bluetoothDevices.append({
                address: d.address.toUpperCase(),
                name: engine._str(d.name, 128)
            })
        }
    }

    // ---- Checks ---------------------------------------------------------

    function _str(v, max) {
        if (typeof v !== "string") {
            return ""
        }
        return v.length > max ? v.substring(0, max) : v
    }
    /// A size or count: a finite number at least 0, else 0.
    function _count(v) {
        return typeof v === "number" && isFinite(v) && v >= 0 ? Math.floor(v) : 0
    }
    /// An id: a whole number at least 0, else -1.
    function _key(v) {
        return typeof v === "number" && isFinite(v) && v >= 0 && Math.floor(v) === v ? v : -1
    }
    function _oneOf(v, allowed) {
        return typeof v === "string" && allowed.indexOf(v) >= 0 ? v : ""
    }
    function _protocol(v) {
        return engine._oneOf(v, ["local_send", "quick_share", "wormhole", "bluetooth"])
    }
    function _error(v) {
        var codes = ["bad_command", "bad_version", "unavailable", "not_found", "bad_settings",
                     "bad_file", "too_large", "refused", "peer_mismatch", "bad_code",
                     "network", "storage", "internal"]
        if (!v || typeof v !== "object") {
            return { code: "internal", message: "" }
        }
        var code = engine._oneOf(v.code, codes)
        return { code: code === "" ? "internal" : code, message: engine._str(v.message, 500) }
    }
    /// What a negative sukkula_command() return means (sukkula.h).
    function _bridgeError(rc) {
        if (rc === -3) {
            return { code: "too_large", message: "command over 64 KiB" }
        }
        if (rc === -5) {
            // SUKKULA_ERR_BUSY: 64 commands await their replies.
            return { code: "too_large", message: "too many commands waiting" }
        }
        if (rc === -1) {
            return { code: "unavailable", message: "the engine is not running" }
        }
        return { code: "internal", message: "command refused: " + rc }
    }
    /// A QR code of `size` rows of `size` characters, `0` or `1`, or null.
    /// 177 modules is version 40, the largest there is.
    function _qr(v) {
        if (!v || typeof v !== "object" || !Array.isArray(v.rows)) {
            return null
        }
        var size = engine._key(v.size)
        if (size < 21 || size > 177 || v.rows.length !== size) {
            return null
        }
        var bits = /^[01]+$/
        for (var i = 0; i < size; i++) {
            var row = v.rows[i]
            if (typeof row !== "string" || row.length !== size || !bits.test(row)) {
                return null
            }
        }
        return { size: size, rows: v.rows.slice(0) }
    }

    function _indexOf(model, role, value) {
        for (var i = 0; i < model.count; i++) {
            if (model.get(i)[role] === value) {
                return i
            }
        }
        return -1
    }

    function _trimTransfers() {
        // Oldest finished ones go first; a list full of active transfers
        // is left alone, since each of those can still be cancelled.
        for (var i = engine.transfers.count - 1;
             i >= 0 && engine.transfers.count > engine.maxTransfers; i--) {
            if (engine.transfers.get(i).state !== "active") {
                engine.transfers.remove(i)
            }
        }
    }

    function _recount() {
        var active = 0
        var bytes = 0
        var total = 0
        for (var i = 0; i < engine.transfers.count; i++) {
            var t = engine.transfers.get(i)
            if (t.state === "active") {
                active++
                bytes += t.bytes
                total += t.total
            }
        }
        engine.activeTransfers = active
        engine.activeBytes = bytes
        engine.activeTotal = total
    }
}
