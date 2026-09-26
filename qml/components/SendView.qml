// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Send mode (F-C6): every way of sending on one screen.
 *
 * At the foot, the centre of a radar: this phone, holding what is to be
 * sent (tap it to choose). On the rings, the peers nearby -- LocalSend and
 * Quick Share as discovery finds them (F-LS1, F-QS1), Bluetooth's paired
 * devices (F-BT1) -- each with its protocol's badge. Above, the cloud the
 * internet protocols go through, and Magic Wormhole's tile (F-MW1); the
 * tile beside it is kept free for croc.
 *
 * Tapping a peer sends to it; with nothing chosen yet, the file picker
 * comes first. While a send runs, only its peer stays, a line runs to it
 * -- through the cloud for Magic Wormhole -- and the peer and the centre
 * fill up as the bytes go.
 *
 * Peer names are the peers' own (after S2) and are shown only in
 * PeerBubble's plain-text label.
 */
Item {
    id: view

    property QtObject engine
    /// What to send: a Payload.
    property QtObject payload
    /// Where to say what went wrong: a Banner.
    property Item banner
    /// Discovery is running: the rings pulse.
    property bool discovering: false
    /// How long an ended send stays on screen, in ms.
    property int linger: 4000
    property bool alive: true
    /// The page stack the view's pages go on: its page's.
    property var pageStack: null

    /// The send on screen, or null: {key, protocol, name, slot,
    /// wormhole, transferId}. `slot` is where its peer was (-1: none).
    property var outgoing: null
    /// The engine's row for the send's transfer, or null.
    property var outgoingTransfer: null
    property string outgoingCode: ""
    /// Waiting for the reply to the send command.
    property bool sending: false
    /// A peer tapped with nothing to send: the file picker is up for it.
    property var pendingPeer: null
    /// The payload's revision when the send on screen took it.
    property int sentRevision: -1

    /// Slot index -> the key of the peer placed there, or "".
    property var slots: []
    /// Peers with no slot free.
    property int overflow: 0

    readonly property bool hasOutgoing: view.outgoing !== null
    readonly property string outgoingState: !view.hasOutgoing ? ""
        : view.outgoingTransfer !== null ? view.outgoingTransfer.state
        : view.outgoing.transferId >= 0 ? "active" : "starting"
    readonly property bool outgoingEnded: view.outgoingState === "done" || view.outgoingState === "failed"
                                       || view.outgoingState === "cancelled"
    readonly property real outgoingProgress: {
        if (!view.hasOutgoing) {
            return -1
        }
        if (view.outgoingState === "done") {
            return 1
        }
        var t = view.outgoingTransfer
        return t && t.total > 0 ? Math.min(1, t.bytes / t.total) : 0
    }
    /// A wormhole send whose receiver has not come yet: its tile shows the
    /// code instead of a peer.
    readonly property bool wormholeWaiting: view.hasOutgoing && view.outgoing.wormhole
        && (view.outgoingState === "starting" || (view.outgoingState === "active"
            && (view.outgoingTransfer === null || view.outgoingTransfer.bytes <= 0)))

    readonly property bool wormholeOn: view.engine.protocolEnabled("wormhole")
    /// Something that puts peers on the rings is switched on.
    readonly property bool nearbyOn: view.engine.protocolEnabled("local_send")
                                     || view.engine.protocolEnabled("quick_share")
                                     || view.engine.protocolEnabled("bluetooth")
    readonly property bool anyOn: view.wormholeOn || view.nearbyOn
    readonly property bool scanning: view.discovering && !view.hasOutgoing && view.visible

    // ---- Geometry -------------------------------------------------------

    readonly property real margin: Theme.horizontalPageMargin
    readonly property real avatar: Theme.itemSizeMedium * 0.9
    readonly property real originSize: Theme.itemSizeMedium
    readonly property real tileWidth: (view.width - 3 * view.margin) / 2
    readonly property real tileHeight: Theme.itemSizeLarge
    readonly property real tilesBottom: Theme.paddingLarge + view.tileHeight
    readonly property real cloudWidth: Math.min(view.width * 0.42, Theme.itemSizeExtraLarge * 2)
    readonly property real cloudHeight: view.cloudWidth * 0.6
    readonly property real summaryHeight: Theme.fontSizeExtraSmall * 1.5
    /// The radar's centre: this phone.
    readonly property real ox: view.width / 2
    readonly property real oy: view.height - view.originSize / 2 - view.summaryHeight - Theme.paddingMedium
    /// The outer ring's half-width and half-height.
    readonly property real rx: Math.max(view.avatar, view.width / 2 - view.margin / 2)
    readonly property real ry: {
        var room = view.oy - view.tilesBottom - view.cloudHeight - view.avatar / 2
                   - view.summaryHeight - 2 * Theme.paddingLarge
        return Math.max(view.avatar, Math.min(view.rx * 1.6, room))
    }
    readonly property real cloudX: view.ox
    readonly property real cloudY: (view.tilesBottom + view.oy - view.ry - view.avatar * 0.6) / 2

    /// The rings, as fractions of the outer one.
    readonly property var rings: [0.24, 0.46, 0.72, 1]
    /// Where peers go, on the outer two rings, in the order they are
    /// filled: [ring, degrees]. Seven, so that no avatar or name covers
    /// another in portrait; more peers than that are behind "+N".
    readonly property var slotSpots: [[1, 90], [0.72, 112], [0.72, 68], [1, 138], [1, 42],
                                      [0.72, 154], [0.72, 26]]

    function spotX(slot) {
        var s = view.slotSpots[slot >= 0 && slot < view.slotSpots.length ? slot : 0]
        var x = view.ox + view.rx * s[0] * Math.cos(s[1] * Math.PI / 180)
        var edge = view.avatar / 2 + Theme.paddingSmall
        return Math.max(edge, Math.min(view.width - edge, x))
    }
    function spotY(slot) {
        var s = view.slotSpots[slot >= 0 && slot < view.slotSpots.length ? slot : 0]
        return view.oy - view.ry * s[0] * Math.sin(s[1] * Math.PI / 180)
    }

    // ---- Peers ----------------------------------------------------------

    function lanPeer(row) {
        return {
            key: row.protocol + ":" + row.peerId,
            protocol: row.protocol,
            name: row.name,
            target: { protocol: row.protocol, peer: row.peerId }
        }
    }
    function bluetoothPeer(row) {
        return {
            key: "bluetooth:" + row.address,
            protocol: "bluetooth",
            name: row.name.length > 0 ? row.name : row.address,
            target: { protocol: "bluetooth", address: row.address }
        }
    }

    /// Every peer a send can go to now, in the order they get slots.
    function peers() {
        var out = []
        var lan = ["local_send", "quick_share"]
        for (var p = 0; p < lan.length; p++) {
            var model = view.engine.peerModel(lan[p])
            if (!view.engine.protocolEnabled(lan[p]) || !model) {
                continue
            }
            for (var i = 0; i < model.count; i++) {
                out.push(view.lanPeer(model.get(i)))
            }
        }
        if (view.engine.protocolEnabled("bluetooth")) {
            var devices = view.engine.bluetoothDevices
            for (var d = 0; d < devices.count; d++) {
                out.push(view.bluetoothPeer(devices.get(d)))
            }
        }
        return out
    }

    /// Gives each peer a slot, keeping every peer's slot while it stays:
    /// nobody jumps when somebody else comes or goes.
    function rebalance() {
        var all = view.peers()
        var present = {}
        for (var i = 0; i < all.length; i++) {
            present[all[i].key] = true
        }
        var next = []
        var placed = {}
        for (var s = 0; s < view.slotSpots.length; s++) {
            var key = s < view.slots.length ? view.slots[s] : ""
            if (key !== "" && present[key] === true) {
                next.push(key)
                placed[key] = true
            } else {
                next.push("")
            }
        }
        var left = 0
        for (var k = 0; k < all.length; k++) {
            if (placed[all[k].key] === true) {
                continue
            }
            var free = next.indexOf("")
            if (free < 0) {
                left++
                continue
            }
            next[free] = all[k].key
            placed[all[k].key] = true
        }
        if (JSON.stringify(next) !== JSON.stringify(view.slots)) {
            view.slots = next
        }
        view.overflow = left
    }

    Connections {
        target: view.engine.localSendPeers
        // Qt 5.6 handler syntax.
        onCountChanged: view.rebalance()
    }
    Connections {
        target: view.engine.quickSharePeers
        onCountChanged: view.rebalance()
    }
    Connections {
        target: view.engine.bluetoothDevices
        onCountChanged: view.rebalance()
    }
    Connections {
        target: view.engine
        onSettingsChanged: view.rebalance()
        onTransferUpdated: {
            if (view.hasOutgoing && transferId === view.outgoing.transferId) {
                view.refreshOutgoing()
            }
        }
        onWormholeCodeArrived: {
            if (view.hasOutgoing && transferId === view.outgoing.transferId) {
                view.refreshOutgoing()
            }
        }
    }

    // ---- Sending --------------------------------------------------------

    /// A peer (or the wormhole tile) was tapped: `peer` as peers() makes
    /// them, `slot` where it sits.
    function choose(peer, slot) {
        if (view.hasOutgoing || view.sending || !peer) {
            return
        }
        if (view.payload.itemCount === 0) {
            view.pendingPeer = { peer: peer, slot: slot }
            view.pickFile()
            return
        }
        if (peer.protocol === "wormhole" && view.payload.itemCount !== 1) {
            //: Send screen: Magic Wormhole tapped with more than one item chosen.
            view.banner.show(qsTr("Magic Wormhole sends one file or one text at a time."))
            return
        }
        if (peer.protocol === "bluetooth" && view.payload.hasText) {
            //: Send screen: a Bluetooth device tapped with a text chosen.
            view.banner.show(qsTr("Bluetooth sends files only."))
            return
        }
        view.sendTo(peer, slot)
    }

    function chooseWormhole() {
        if (view.hasOutgoing) {
            if (view.outgoing.wormhole && view.outgoing.transferId >= 0 && view.outgoingCode !== "") {
                view.pageStack.push(Qt.resolvedUrl("../pages/WormholeCodePage.qml"),
                               { engine: view.engine, transferId: view.outgoing.transferId })
            } else if (view.outgoingEnded) {
                view.dismiss()
            }
            return
        }
        view.choose({ key: "wormhole", protocol: "wormhole", name: "", target: { protocol: "wormhole" } }, -1)
    }

    function sendTo(peer, slot) {
        view.sending = true
        view.outgoing = {
            key: peer.key, protocol: peer.protocol, name: peer.name, slot: slot,
            wormhole: peer.protocol === "wormhole", transferId: -1
        }
        view.outgoingTransfer = null
        view.outgoingCode = ""
        view.sentRevision = view.payload.revision
        var self = view
        view.engine.send(peer.target, view.payload.items(), function (ok, error, transfer) {
            if (self.alive !== true) {
                return
            }
            self.sending = false
            if (!ok) {
                self.outgoing = null
                self.banner.show(self.engine.errorText(error))
                return
            }
            var f = self.outgoing
            if (f) {
                self.outgoing = {
                    key: f.key, protocol: f.protocol, name: f.name, slot: f.slot,
                    wormhole: f.wormhole, transferId: transfer
                }
                self.refreshOutgoing()
            }
        })
    }

    function refreshOutgoing() {
        if (!view.hasOutgoing || view.outgoing.transferId < 0) {
            return
        }
        view.outgoingTransfer = view.engine.transfer(view.outgoing.transferId)
        var c = view.engine.wormholeCode(view.outgoing.transferId)
        view.outgoingCode = c ? c.code : ""
    }

    onOutgoingEndedChanged: {
        if (view.outgoingEnded) {
            if (view.outgoingState === "done" && view.payload.revision === view.sentRevision) {
                // Sent: what was sent is done with. A failed send keeps it
                // for another try, and a share that came meanwhile stays.
                view.payload.clear()
            }
            lingering.restart()
        }
    }

    /// Back to the radar after a send has ended.
    function dismiss() {
        if (!view.outgoingEnded) {
            return
        }
        lingering.stop()
        view.outgoing = null
        view.outgoingTransfer = null
        view.outgoingCode = ""
    }

    function cancelSend() {
        if (view.hasOutgoing && view.outgoing.transferId >= 0 && view.outgoingState === "active") {
            view.engine.cancel(view.outgoing.transferId)
        }
    }

    Timer {
        id: lingering
        interval: view.linger
        onTriggered: view.dismiss()
    }

    function pickFile() {
        var picker = view.pageStack.push(Qt.resolvedUrl("../pages/FilePicker.qml"))
        if (picker) {
            picker.picked.connect(view.picked)
        } else {
            view.pendingPeer = null
        }
    }

    function picked(path) {
        var ok = view.payload.addFile(path)
        var pending = view.pendingPeer
        view.pendingPeer = null
        if (ok && pending) {
            view.choose(pending.peer, pending.slot)
        }
    }

    /// The main page is on top again: a picker closed without a file
    /// takes its pending peer with it.
    function returned() {
        view.pendingPeer = null
    }

    function editPayload() {
        if (view.hasOutgoing) {
            view.dismiss()
            return
        }
        view.pageStack.push(Qt.resolvedUrl("../pages/PayloadPage.qml"), { payload: view.payload })
    }

    function showAll() {
        view.pageStack.push(Qt.resolvedUrl("../pages/PeerListPage.qml"), { engine: view.engine, view: view })
    }

    Component.onCompleted: view.rebalance()
    Component.onDestruction: view.alive = false

    clip: true

    // ---- The picture ----------------------------------------------------

    // The rings, pulsing outwards while discovery looks.
    Repeater {
        model: view.rings
        delegate: Rectangle {
            id: ring
            readonly property real f: modelData
            x: view.ox - view.rx * ring.f
            y: view.oy - view.rx * ring.f
            width: 2 * view.rx * ring.f
            height: width
            radius: width / 2
            color: "transparent"
            border.width: 2
            border.color: view.hasOutgoing ? Theme.rgba(Theme.secondaryColor, 0.35) : Theme.primaryColor
            opacity: 0.6
            transform: Scale {
                origin.x: ring.width / 2
                origin.y: ring.height / 2
                yScale: view.ry / view.rx
            }

            SequentialAnimation on opacity {
                running: view.scanning
                loops: Animation.Infinite
                alwaysRunToEnd: true
                PauseAnimation { duration: 250 * index }
                NumberAnimation { to: 1; duration: 350; easing.type: Easing.OutQuad }
                NumberAnimation { to: 0.6; duration: 700; easing.type: Easing.InQuad }
                PauseAnimation { duration: 250 * (3 - index) + 600 }
            }
        }
    }

    // The internet: what Magic Wormhole goes through.
    Glyph {
        objectName: "cloud"
        x: view.cloudX - width / 2
        y: view.cloudY - height / 2
        width: view.cloudWidth
        height: view.cloudHeight
        visible: view.wormholeOn
        kind: "cloud"
        color: view.hasOutgoing && !view.outgoing.wormhole ? Theme.rgba(Theme.secondaryColor, 0.35)
                                                     : Theme.primaryColor
        lineWidth: Math.max(2, Theme.paddingSmall / 2)
    }

    // The way a send goes: straight to a peer nearby, or up through the
    // cloud to the wormhole's receiver.
    Segment {
        objectName: "sendLine"
        visible: view.hasOutgoing && !view.outgoing.wormhole
        x1: view.ox
        y1: view.oy
        x2: view.hasOutgoing ? view.spotX(view.outgoing.slot) : view.ox
        y2: view.hasOutgoing ? view.spotY(view.outgoing.slot) : view.oy
        startGap: view.originSize / 2
        endGap: view.avatar / 2
    }
    Segment {
        objectName: "wormholeLine"
        visible: view.hasOutgoing && view.outgoing.wormhole
        x1: view.ox
        y1: view.oy
        x2: view.cloudX
        y2: view.cloudY
        startGap: view.originSize / 2
    }
    Segment {
        visible: view.hasOutgoing && view.outgoing.wormhole
        x1: view.cloudX
        y1: view.cloudY
        x2: wormholeTile.x + wormholeTile.width / 2
        // To the tile's edge while it shows the code, to the avatar's
        // once the receiver has come.
        y2: view.wormholeWaiting ? wormholeTile.y + wormholeTile.height : wormholeTile.y + wormholeTile.height / 2
        endGap: view.wormholeWaiting ? 0 : view.avatar / 2
    }

    WormholeTile {
        id: wormholeTile
        objectName: "wormholeTile"
        x: view.margin
        y: Theme.paddingLarge
        width: view.tileWidth
        height: view.tileHeight
        visible: view.wormholeOn && (!view.hasOutgoing || view.wormholeWaiting)
        starting: view.wormholeWaiting
        code: view.wormholeWaiting ? view.outgoingCode : ""
        onClicked: view.chooseWormhole()
    }

    // Kept free for croc: the top right-hand tile of the sketch.
    Item {
        objectName: "crocSlot"
        x: view.width - view.margin - view.tileWidth
        y: Theme.paddingLarge
        width: view.tileWidth
        height: view.tileHeight
        visible: false
    }

    // The peers, in their slots. Hidden while a send is on screen: its
    // peer is drawn on its own below, and stays even if discovery loses it.
    Repeater {
        model: view.engine.protocolEnabled("local_send") ? view.engine.localSendPeers : null
        delegate: PeerBubble {
            readonly property int slot: view.slots.indexOf("local_send:" + model.peerId)
            objectName: "peerBubble"
            size: view.avatar
            x: view.spotX(slot) - width / 2
            y: view.spotY(slot) - height / 2
            visible: slot >= 0 && !view.hasOutgoing
            name: model.name
            protocol: "local_send"
            onClicked: view.choose(view.lanPeer(model), slot)
        }
    }
    Repeater {
        model: view.engine.protocolEnabled("quick_share") ? view.engine.quickSharePeers : null
        delegate: PeerBubble {
            readonly property int slot: view.slots.indexOf("quick_share:" + model.peerId)
            objectName: "peerBubble"
            size: view.avatar
            x: view.spotX(slot) - width / 2
            y: view.spotY(slot) - height / 2
            visible: slot >= 0 && !view.hasOutgoing
            name: model.name
            protocol: "quick_share"
            onClicked: view.choose(view.lanPeer(model), slot)
        }
    }
    Repeater {
        model: view.engine.protocolEnabled("bluetooth") ? view.engine.bluetoothDevices : null
        delegate: PeerBubble {
            readonly property int slot: view.slots.indexOf("bluetooth:" + model.address)
            objectName: "peerBubble"
            size: view.avatar
            x: view.spotX(slot) - width / 2
            y: view.spotY(slot) - height / 2
            visible: slot >= 0 && !view.hasOutgoing
            name: model.name.length > 0 ? model.name : model.address
            protocol: "bluetooth"
            onClicked: view.choose(view.bluetoothPeer(model), slot)
        }
    }

    // More peers than slots: all of them, as a list.
    Rectangle {
        id: more
        objectName: "morePeers"
        x: view.ox + view.rx * 0.72 - width / 2
        y: view.oy - height / 2
        width: view.avatar * 0.8
        height: width
        radius: width / 2
        visible: view.overflow > 0 && !view.hasOutgoing
        color: Theme.rgba(Theme.highlightBackgroundColor, moreArea.pressed ? 0.3 : 0.1)
        border.width: 2
        border.color: moreArea.pressed ? Theme.highlightColor : Theme.primaryColor

        signal clicked()
        onClicked: view.showAll()

        Label {
            anchors.centerIn: parent
            text: "+" + view.overflow
            textFormat: Text.PlainText
            font.pixelSize: Theme.fontSizeSmall
            color: moreArea.pressed ? Theme.highlightColor : Theme.primaryColor
        }
        MouseArea {
            id: moreArea
            anchors.fill: parent
            onClicked: more.clicked()
        }
    }

    // The send's peer: in its slot, or where the wormhole tile was once
    // the receiver has come.
    PeerBubble {
        id: target
        objectName: "outgoingBubble"
        size: view.avatar
        visible: view.hasOutgoing && !view.wormholeWaiting
        x: (view.hasOutgoing && view.outgoing.wormhole ? wormholeTile.x + wormholeTile.width / 2
                                                 : view.spotX(view.hasOutgoing ? view.outgoing.slot : 0)) - width / 2
        y: (view.hasOutgoing && view.outgoing.wormhole ? wormholeTile.y + wormholeTile.height / 2
                                                 : view.spotY(view.hasOutgoing ? view.outgoing.slot : 0)) - height / 2
        name: view.hasOutgoing ? view.outgoing.name : ""
        protocol: view.hasOutgoing && !view.outgoing.wormhole ? view.outgoing.protocol : ""
        active: true
        progress: view.outgoingProgress
        onClicked: view.dismiss()
    }

    // How far, next to the peer: the percentage big, then what is going on.
    Column {
        id: outgoingInfo
        readonly property bool leftOfPeer: target.x + target.width / 2 > view.width / 2
        x: outgoingInfo.leftOfPeer ? target.x - width - Theme.paddingLarge
                                : target.x + target.width + Theme.paddingLarge
        y: target.y
        width: Math.max(0, outgoingInfo.leftOfPeer ? target.x - Theme.paddingLarge - view.margin
                                                : view.width - target.x - target.width - Theme.paddingLarge - view.margin)
        visible: target.visible
        spacing: Theme.paddingSmall / 2

        Label {
            objectName: "outgoingPercent"
            width: parent.width
            horizontalAlignment: outgoingInfo.leftOfPeer ? Text.AlignRight : Text.AlignLeft
            text: Math.floor(100 * Math.max(0, view.outgoingProgress)) + "%"
            textFormat: Text.PlainText
            visible: view.outgoingState === "active" || view.outgoingState === "done"
            font.pixelSize: Theme.fontSizeExtraLarge
            color: Theme.highlightColor
        }
        Label {
            objectName: "outgoingStatus"
            width: parent.width
            horizontalAlignment: outgoingInfo.leftOfPeer ? Text.AlignRight : Text.AlignLeft
            text: view.statusText()
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeExtraSmall
            color: view.outgoingState === "failed" ? Theme.errorColor : Theme.secondaryHighlightColor
        }
        IconButton {
            objectName: "cancelSend"
            visible: view.outgoingState === "active"
            icon.source: "image://theme/icon-m-clear"
            onClicked: view.cancelSend()
        }
    }

    function statusText() {
        var t = view.outgoingTransfer
        switch (view.outgoingState) {
        case "starting":
            //: Send screen: a send was asked for, the engine has not answered yet.
            return qsTr("Connecting…")
        case "active":
            if (t && t.bytes > 0) {
                //: Progress of a send: %1 bytes so far, %2 bytes in all, both formatted.
                return qsTr("%1 of %2").arg(Format.formatFileSize(t.bytes)).arg(Format.formatFileSize(t.total))
            }
            //: Send screen: the peer has been asked and has not answered yet.
            return qsTr("Waiting for an answer…")
        case "done":
            //: A send arrived.
            return qsTr("Sent")
        case "cancelled":
            //: A send was stopped by one of the two sides.
            return qsTr("Cancelled")
        case "failed":
            //: A send failed; %1 says why.
            return qsTr("Failed: %1").arg(view.engine.errorText({ code: t ? t.error : "" }))
        }
        return ""
    }

    // This phone, with what it is sending.
    Item {
        id: origin
        objectName: "origin"
        x: view.ox - width / 2
        y: view.oy - height / 2
        width: view.originSize
        height: width

        signal clicked()
        onClicked: view.editPayload()

        readonly property bool empty: view.payload.itemCount === 0
        readonly property color ink: originArea.pressed || view.hasOutgoing ? Theme.highlightColor
                                                                        : Theme.primaryColor

        Rectangle {
            anchors.fill: parent
            radius: width / 2
            color: Theme.rgba(Theme.highlightBackgroundColor, originArea.pressed ? 0.3 : 0.15)
            border.width: Math.max(2, Math.round(width / 30))
            border.color: origin.ink
        }

        // Fills with the send, like its peer.
        Item {
            anchors {
                left: parent.left
                right: parent.right
                bottom: parent.bottom
            }
            height: parent.height * Math.max(0, view.outgoingProgress)
            clip: true
            visible: view.hasOutgoing

            Rectangle {
                y: parent.height - origin.height
                width: origin.width
                height: origin.height
                radius: width / 2
                color: Theme.highlightColor
                opacity: 0.8
            }
        }

        Image {
            anchors.centerIn: parent
            visible: origin.empty && !view.hasOutgoing
            source: "image://theme/icon-m-add?" + origin.ink
        }

        Glyph {
            anchors.centerIn: parent
            width: parent.width * 0.6
            height: width
            visible: !origin.empty || view.hasOutgoing
            kind: view.payload.files.length > 0 || origin.empty ? "file" : "text"
            color: origin.ink
        }

        // How many things, when more than one.
        Rectangle {
            anchors {
                right: parent.right
                top: parent.top
                rightMargin: -width * 0.2
                topMargin: -height * 0.2
            }
            width: Math.max(height, countLabel.implicitWidth + Theme.paddingSmall)
            height: Theme.iconSizeSmall
            radius: height / 2
            visible: view.payload.itemCount > 1 && !view.hasOutgoing
            color: Theme.highlightBackgroundColor

            Label {
                id: countLabel
                objectName: "payloadCount"
                anchors.centerIn: parent
                text: String(view.payload.itemCount)
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.primaryColor
            }
        }

        MouseArea {
            id: originArea
            anchors.fill: parent
            onClicked: origin.clicked()
        }
    }

    Label {
        objectName: "payloadSummary"
        anchors {
            top: origin.bottom
            topMargin: Theme.paddingSmall / 2
            horizontalCenter: origin.horizontalCenter
        }
        width: view.width - 2 * view.margin
        visible: !view.hasOutgoing
        horizontalAlignment: Text.AlignHCenter
        text: view.summary()
        textFormat: Text.PlainText
        truncationMode: TruncationMode.Fade
        font.pixelSize: Theme.fontSizeExtraSmall
        color: origin.empty ? Theme.secondaryHighlightColor : Theme.secondaryColor
    }

    function summary() {
        var p = view.payload
        if (p.itemCount === 0) {
            //: Send screen, under the centre of the radar with nothing chosen yet.
            return qsTr("Tap to choose what to send")
        }
        if (p.itemCount === 1 && p.files.length === 1) {
            return p.files[0].name
        }
        if (p.files.length === 0) {
            //: Send screen, under the centre of the radar: only texts are chosen.
            return qsTr("%n text(s)", "", p.itemCount)
        }
        //: Send screen, under the centre of the radar: files, or files and texts.
        return qsTr("%n item(s)", "", p.itemCount)
    }

    // Nothing to send to yet, or nothing switched on.
    Label {
        objectName: "radarHint"
        x: view.margin
        width: view.width - 2 * view.margin
        y: view.oy - view.ry * 0.55 - height / 2
        visible: !view.hasOutgoing && (!view.anyOn || (view.nearbyOn && view.slots.join("") === ""))
        horizontalAlignment: Text.AlignHCenter
        text: !view.anyOn
              //: Send screen when every protocol is off.
              ? qsTr("Every way of sending is switched off in Settings.")
              : view.discovering
                //: Send screen: discovery is running and has found nobody yet.
                ? qsTr("Looking for devices nearby…")
                //: Send screen: nobody found and discovery is not running.
                : qsTr("No devices nearby.")
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        font.pixelSize: Theme.fontSizeSmall
        color: Theme.secondaryHighlightColor
    }
}
