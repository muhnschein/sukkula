// SPDX-License-Identifier: GPL-3.0-or-later
.pragma library

// Engine events as crates/sukkula-engine/src/api.rs serialises them, for
// the QML tests. Peer-supplied strings are spelled with EVIL in them, which
// is what the runner's plain-text check looks for (see runner.cpp).

// A sender name no UI should ever render as markup, with a "%2" in it so
// a string built with .arg() shows whether it was substituted again.
var EVIL_NAME = "<b>EVIL</b><img src=x> Pekka %2"
var EVIL_MODEL = "<i>EVIL</i> Pixel 9"
var EVIL_FILE = "<u>EVIL<u> holiday.jpg"
var EVIL_TEXT = "<a href=\"https://evil.example\">EVIL link</a> & <script>x</script>"

function json(o) {
    return JSON.stringify(o)
}

function receiving(on, failed) {
    return json({
        type: "receiving",
        on: on,
        protocols: [
            failed ? { protocol: "local_send", state: "failed",
                       error: { code: "network", message: "port 53317 in use" } }
                   : { protocol: "local_send", state: on ? "ready" : "off" },
            { protocol: "quick_share", state: on ? "ready" : "off" },
            { protocol: "wormhole", state: "send_only" },
            { protocol: "bluetooth", state: "send_only" }
        ]
    })
}

function offer(id, overrides) {
    var o = {
        id: id,
        protocol: "quick_share",
        sender: EVIL_NAME,
        model: EVIL_MODEL,
        files: [
            { name: EVIL_FILE, size: 2500000 },
            { name: "notes.txt", size: 1200 }
        ],
        more_files: 3,
        file_count: 5,
        total_bytes: 2600000,
        has_text: true,
        pin: "4821",
        expires_in: 60
    }
    for (var k in overrides) {
        o[k] = overrides[k]
    }
    return json({ type: "offer_pending", offer: o })
}

function offerClosed(id, reason) {
    return json({ type: "offer_closed", offer: id, reason: reason })
}

function transferStarted(id, direction, overrides) {
    var t = {
        id: id,
        direction: direction,
        protocol: "local_send",
        peer: EVIL_NAME,
        files: [{ name: EVIL_FILE, size: 1000 }, { name: "b.pdf", size: 1000 }],
        file_count: 2,
        total_bytes: 2000
    }
    for (var k in overrides) {
        t[k] = overrides[k]
    }
    return json({ type: "transfer_started", transfer: t })
}

function progress(id, bytes, total) {
    return json({ type: "transfer_progress", transfer: id, bytes: bytes, total: total })
}

function finished(id, result, saved, code) {
    var outcome = { result: result }
    if (result === "failed") {
        outcome.error = { code: code ? code : "network", message: "timeout" }
    }
    var e = { type: "transfer_finished", transfer: id, outcome: outcome }
    if (saved) {
        e.saved = saved
    }
    return json(e)
}

function textReceived(id, text) {
    return json({ type: "text_received", transfer: id, from: EVIL_NAME, text: text ? text : EVIL_TEXT })
}

function peerFound(id, protocol, name) {
    return json({ type: "peer_found", peer: { id: id, protocol: protocol, name: name ? name : EVIL_NAME,
                                              model: EVIL_MODEL, device_type: "phone" } })
}

function peerLost(id) {
    return json({ type: "peer_lost", peer: id })
}

// A 21x21 pattern: finder-like corners and a checkerboard.
function qr21() {
    var rows = []
    for (var y = 0; y < 21; y++) {
        var row = ""
        for (var x = 0; x < 21; x++) {
            row += ((x < 7 && y < 7) || (x > 13 && y < 7) || (x < 7 && y > 13) || (x + y) % 2 === 0) ? "1" : "0"
        }
        rows.push(row)
    }
    return { size: 21, rows: rows }
}

function wormholeCode(id, code, qr) {
    return json({ type: "wormhole_code", transfer: id, code: code, qr: qr ? qr : qr21() })
}

function bluetoothDevices(devices) {
    return json({ type: "bluetooth_devices", devices: devices })
}

function settings(overrides) {
    var s = {
        device_name: "",
        localsend: { enabled: true, pin: null },
        quickshare: { enabled: true, visibility: "everyone", ble_nudge: true },
        wormhole: { enabled: true, mailbox_url: null, relay_url: null },
        bluetooth: { enabled: true },
        logging: false
    }
    for (var k in overrides) {
        s[k] = overrides[k]
    }
    return json({ type: "settings", settings: s, effective_device_name: "Jolla Phone" })
}

function reply(id, ok, code, transfer) {
    var r = { type: "reply", id: id, ok: ok }
    if (!ok) {
        r.error = { code: code ? code : "internal", message: "detail for logs" }
    }
    if (transfer !== undefined) {
        r.transfer = transfer
    }
    return json(r)
}
