// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Send mode (F-C6): every way of sending on one radar.
 *
 * At the foot, this phone, holding what is to be sent: tap it to choose
 * files, and again to add more; the cross beside it clears the lot. Until
 * something is chosen that is all there is. Then the peers nearby come
 * onto the rings -- LocalSend and Quick Share as discovery finds them
 * (F-LS1, F-QS1), Bluetooth's paired devices (F-BT1), each with its
 * protocol's badge -- and the cloud above them, which holds the internet
 * protocols: tapping it shows Magic Wormhole's tile (F-MW1) and croc's.
 * Discovery runs from the start, so the peers are there as soon as the
 * files are.
 *
 * Tapping a peer or a tile sends to it. While a send runs, only its peer
 * stays, a line runs to it -- through the cloud for the internet
 * protocols -- and the peer and the centre fill up as the bytes go.
 *
 * Peer names are the peers' own (after S2) and are shown only in
 * PeerBubble's plain-text label.
 */
Radar {
    id: view

    property QtObject engine
    /// What to send: a Payload.
    property QtObject payload
    /// Where to say what went wrong: a Banner.
    property Item banner
    /// Discovery is running.
    property bool discovering: false
    /// This is the tab on screen.
    property bool current: true
    /// How long an ended send stays on screen, in ms.
    property int linger: 4000
    property bool alive: true
    /// The internet protocols' tiles are out.
    property bool moreOpen: false

    /// The send on screen, or null: {key, protocol, name, slot, internet,
    /// transferId}. `slot` is where its peer was (-1: none).
    property var outgoing: null
    /// The engine's row for the send's transfer, or null.
    property var outgoingTransfer: null
    property string outgoingCode: ""
    /// Waiting for the reply to the send command.
    property bool sending: false
    /// The payload's revision when the send on screen took it.
    property int sentRevision: -1

    readonly property bool hasPayload: view.payload.itemCount > 0
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
    /// An internet send whose receiver has not come yet: its tile shows
    /// the code instead of a peer.
    readonly property bool codeWaiting: view.hasOutgoing && view.outgoing.internet
        && (view.outgoingState === "starting" || (view.outgoingState === "active"
            && (view.outgoingTransfer === null || view.outgoingTransfer.bytes <= 0)))

    readonly property bool wormholeOn: view.engine.protocolEnabled("wormhole")
    readonly property bool crocOn: view.engine.protocolEnabled("croc")
    readonly property bool internetOn: view.wormholeOn || view.crocOn
    /// Something that puts peers on the rings is switched on.
    readonly property bool nearbyOn: view.engine.protocolEnabled("local_send")
                                     || view.engine.protocolEnabled("quick_share")
                                     || view.engine.protocolEnabled("bluetooth")
    readonly property bool anyOn: view.internetOn || view.nearbyOn
    /// Peers are shown: there is something to send them.
    readonly property bool showPeers: view.hasPayload && !view.hasOutgoing
    /// Where the send's line ends: the tile of its protocol.
    readonly property real outgoingTileX: view.hasOutgoing && view.outgoing.protocol === "croc"
                                          ? view.rightTileX : view.leftTileX

    pulsing: view.discovering && view.showPeers && view.current
    faint: view.hasOutgoing

    onSlotSpotsChanged: view.rebalance()
    onHasPayloadChanged: {
        if (!view.hasPayload) {
            view.moreOpen = false
        }
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

    function rebalance() {
        var all = view.peers()
        var keys = []
        for (var i = 0; i < all.length; i++) {
            keys.push(all[i].key)
        }
        view.place(keys)
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
        onCodeArrived: {
            if (view.hasOutgoing && transferId === view.outgoing.transferId) {
                view.refreshOutgoing()
            }
        }
    }

    // ---- Sending --------------------------------------------------------

    /// A peer (or a tile) was tapped: `peer` as peers() makes them, `slot`
    /// where it sits.
    function choose(peer, slot) {
        if (view.hasOutgoing || view.sending || !peer) {
            return
        }
        if (!view.hasPayload) {
            view.pickFiles()
            return
        }
        if (peer.protocol === "wormhole" && view.payload.itemCount !== 1) {
            //: Send screen: Magic Wormhole tapped with more than one item chosen.
            view.banner.show(qsTr("Magic Wormhole sends one file or one text at a time."))
            return
        }
        if (peer.protocol === "croc" && view.payload.hasText && view.payload.itemCount !== 1) {
            //: Send screen: croc tapped with a text and something else chosen.
            view.banner.show(qsTr("croc sends files, or one text on its own."))
            return
        }
        if (peer.protocol === "bluetooth" && view.payload.hasText) {
            //: Send screen: a Bluetooth device tapped with a text chosen.
            view.banner.show(qsTr("Bluetooth sends files only."))
            return
        }
        view.sendTo(peer, slot)
    }

    /// Magic Wormhole's or croc's tile was tapped.
    function chooseTile(protocol) {
        if (view.hasOutgoing) {
            if (view.outgoing.protocol === protocol && view.outgoing.transferId >= 0 && view.outgoingCode !== "") {
                pageStack.push(Qt.resolvedUrl("../pages/WormholeCodePage.qml"),
                               { engine: view.engine, transferId: view.outgoing.transferId,
                                 protocol: protocol })
            } else if (view.outgoingEnded) {
                view.dismiss()
            }
            return
        }
        view.choose({ key: protocol, protocol: protocol, name: "", target: { protocol: protocol } }, -1)
    }

    function sendTo(peer, slot) {
        view.sending = true
        view.outgoing = {
            key: peer.key, protocol: peer.protocol, name: peer.name, slot: slot,
            internet: peer.protocol === "wormhole" || peer.protocol === "croc", transferId: -1
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
                    internet: f.internet, transferId: transfer
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
        var c = view.engine.sendCode(view.outgoing.transferId)
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

    /// Files to add, from the file browser: several at once where Silica
    /// has the dialog for it, one at a time where it does not.
    function pickFiles() {
        if (view.hasOutgoing) {
            view.dismiss()
            return
        }
        if (view.payload.files.length >= view.payload.maxFiles) {
            //: Send screen: the most files one send can carry are chosen already.
            view.banner.show(qsTr("That is as many files as one send can take."))
            return
        }
        var url = Qt.resolvedUrl("../pages/FilePicker.qml")
        if (Qt.createComponent(url).status !== Component.Ready) {
            url = Qt.resolvedUrl("../pages/SingleFilePicker.qml")
        }
        var picker = pageStack.push(url)
        if (picker) {
            picker.picked.connect(view.picked)
        }
    }

    /// Absolute paths from the picker.
    function picked(paths) {
        var list = Array.isArray(paths) ? paths : [paths]
        for (var i = 0; i < list.length; i++) {
            view.payload.addFile(list[i])
        }
    }

    function clearPayload() {
        if (!view.hasOutgoing) {
            view.payload.clear()
        }
    }

    function showAll() {
        pageStack.push(Qt.resolvedUrl("../pages/PeerListPage.qml"), { engine: view.engine, view: view })
    }

    Component.onCompleted: view.rebalance()
    Component.onDestruction: view.alive = false

    // ---- The picture ----------------------------------------------------

    // The internet: what Magic Wormhole and croc go through.
    CloudButton {
        id: cloud
        objectName: "cloud"
        x: view.cloudX - width / 2
        y: view.cloudY - view.cloudHeight / 2
        width: view.cloudWidth
        cloudHeight: view.cloudHeight
        labelHeight: view.summaryHeight
        labelWidth: view.width - 2 * view.margin
        visible: view.internetOn && (view.showPeers || (view.hasOutgoing && view.outgoing.internet))
        faint: view.hasOutgoing && !view.outgoing.internet
        //: Send screen, under the cloud: tapping it shows Magic Wormhole and croc.
        label: !view.hasOutgoing && !view.moreOpen ? qsTr("More options") : ""
        onClicked: {
            if (!view.hasOutgoing) {
                view.moreOpen = !view.moreOpen
            }
        }
    }

    // The way a send goes: straight to a peer nearby, or up through the
    // cloud to the internet's receiver.
    Segment {
        objectName: "sendLine"
        visible: view.hasOutgoing && !view.outgoing.internet
        x1: view.ox
        y1: view.oy
        x2: view.hasOutgoing ? view.spotX(view.outgoing.slot) : view.ox
        y2: view.hasOutgoing ? view.spotY(view.outgoing.slot) : view.oy
        startGap: view.originSize / 2
        endGap: view.avatar / 2
    }
    Segment {
        objectName: "internetLine"
        visible: view.hasOutgoing && view.outgoing.internet
        x1: view.ox
        y1: view.oy
        x2: view.cloudX
        y2: view.cloudY
        startGap: view.originSize / 2
    }
    Segment {
        visible: view.hasOutgoing && view.outgoing.internet
        x1: view.cloudX
        y1: view.cloudY
        x2: view.outgoingTileX
        // To the tile's edge while it shows the code, to the avatar's
        // once the receiver has come.
        y2: view.codeWaiting ? view.tilesY + view.tileHeight : view.tileMidY
        endGap: view.codeWaiting ? 0 : view.avatar / 2
    }

    CodeTile {
        objectName: "wormholeTile"
        x: view.margin
        y: view.tilesY
        width: view.tileWidth
        height: view.tileHeight
        visible: view.wormholeOn && (view.hasOutgoing ? view.codeWaiting && view.outgoing.protocol === "wormhole"
                                                      : view.moreOpen && view.hasPayload)
        title: "Magic Wormhole"
        //: Send screen: under Magic Wormhole's name on its tile.
        hint: qsTr("Send with a code")
        starting: view.codeWaiting
        code: view.codeWaiting ? view.outgoingCode : ""
        onClicked: view.chooseTile("wormhole")
    }

    CodeTile {
        objectName: "crocTile"
        x: view.width - view.margin - view.tileWidth
        y: view.tilesY
        width: view.tileWidth
        height: view.tileHeight
        visible: view.crocOn && (view.hasOutgoing ? view.codeWaiting && view.outgoing.protocol === "croc"
                                                  : view.moreOpen && view.hasPayload)
        title: "croc"
        //: Send screen: under croc's name on its tile.
        hint: qsTr("Send with a code")
        starting: view.codeWaiting
        code: view.codeWaiting ? view.outgoingCode : ""
        onClicked: view.chooseTile("croc")
    }

    // The peers, in their slots, once there is something to send them.
    // Hidden while a send is on screen: its peer is drawn on its own
    // below, and stays even if discovery loses it.
    Repeater {
        model: view.engine.protocolEnabled("local_send") ? view.engine.localSendPeers : null
        delegate: PeerBubble {
            readonly property int slot: view.slots.indexOf("local_send:" + model.peerId)
            objectName: "peerBubble"
            size: view.avatar
            x: view.spotX(slot) - width / 2
            y: view.spotY(slot) - height / 2
            visible: slot >= 0 && view.showPeers
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
            visible: slot >= 0 && view.showPeers
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
            visible: slot >= 0 && view.showPeers
            name: model.name.length > 0 ? model.name : model.address
            protocol: "bluetooth"
            onClicked: view.choose(view.bluetoothPeer(model), slot)
        }
    }

    // More peers than slots: all of them, as a list.
    Rectangle {
        id: more
        objectName: "morePeers"
        x: view.moreX - width / 2
        y: view.oy - height / 2
        width: view.avatar * 0.8
        height: width
        radius: width / 2
        visible: view.overflow > 0 && view.showPeers
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

    // What was chosen, off again.
    IconButton {
        objectName: "clearPayload"
        x: view.lessX - width / 2
        y: view.oy - height / 2
        visible: view.hasPayload && !view.hasOutgoing
        icon.source: "image://theme/icon-m-clear"
        onClicked: view.clearPayload()
    }

    // The send's peer: in its slot, or on its tile once the receiver has
    // come.
    PeerBubble {
        id: target
        objectName: "outgoingBubble"
        size: view.avatar
        visible: view.hasOutgoing && !view.codeWaiting
        x: (view.hasOutgoing && view.outgoing.internet ? view.outgoingTileX
                                                       : view.spotX(view.hasOutgoing ? view.outgoing.slot : 0)) - width / 2
        y: (view.hasOutgoing && view.outgoing.internet ? view.tileMidY
                                                       : view.spotY(view.hasOutgoing ? view.outgoing.slot : 0)) - height / 2
        name: view.hasOutgoing ? view.outgoing.name : ""
        protocol: view.hasOutgoing && !view.outgoing.internet ? view.outgoing.protocol : ""
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

        // The percentage, and beside it the way to stop: on the side
        // away from the peer.
        Row {
            anchors {
                left: outgoingInfo.leftOfPeer ? undefined : parent.left
                right: outgoingInfo.leftOfPeer ? parent.right : undefined
            }
            layoutDirection: outgoingInfo.leftOfPeer ? Qt.RightToLeft : Qt.LeftToRight
            spacing: Theme.paddingSmall

            Label {
                objectName: "outgoingPercent"
                anchors.verticalCenter: parent.verticalCenter
                text: Math.floor(100 * Math.max(0, view.outgoingProgress)) + "%"
                textFormat: Text.PlainText
                visible: view.outgoingState === "active" || view.outgoingState === "done"
                font.pixelSize: Theme.fontSizeExtraLarge
                color: Theme.highlightColor
            }
            IconButton {
                objectName: "cancelSend"
                anchors.verticalCenter: parent.verticalCenter
                visible: view.outgoingState === "active"
                icon.source: "image://theme/icon-m-clear"
                onClicked: view.cancelSend()
            }
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
        onClicked: view.pickFiles()

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

        Glyph {
            objectName: "originGlyph"
            anchors.centerIn: parent
            width: parent.width * 0.6
            height: width
            kind: !view.hasPayload && !view.hasOutgoing ? "add"
                  : view.payload.files.length > 0 || !view.hasPayload ? "file" : "text"
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
        color: view.hasPayload ? Theme.secondaryColor : Theme.secondaryHighlightColor
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

    // Nobody to send to yet, or nothing switched on.
    Label {
        objectName: "radarHint"
        x: view.margin
        width: view.width - 2 * view.margin
        y: view.oy - view.outer * 0.55 - height / 2
        visible: !view.hasOutgoing && (!view.anyOn || (view.hasPayload && view.nearbyOn
                                                       && view.slots.join("") === ""))
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
