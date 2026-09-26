// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Receive mode (F-C1): Send mode's radar, the other way round.
 *
 * At the foot, this phone, with the name others see. The rings pulse
 * while it is visible. A device that offers something comes onto the
 * rings with its protocol's badge while the consent dialog asks (F-C2);
 * once accepted, a line runs from it to this phone and both fill up as
 * the bytes come (F-C5). Above, the cloud: tapping it shows the tiles to
 * receive with a code, over Magic Wormhole (F-MW2) or croc, and what
 * comes that way comes through the cloud from its tile.
 *
 * What was received, and what was sent, is on the History page.
 *
 * Sender names are the peers' own (after S2) and are shown only in
 * PeerBubble's plain-text label.
 */
Radar {
    id: view

    property QtObject engine
    /// Where to say what went wrong: a Banner.
    property Item banner
    /// This is the tab on screen.
    property bool current: true
    /// How long an ended transfer stays on screen, in ms.
    property int linger: 4000
    property bool alive: true
    /// The code tiles are out.
    property bool moreOpen: false

    /// Ended incoming transfers still on screen: transferId -> when.
    property var ended: ({})
    /// Offers that have left the rings, for their transfer to take their
    /// slot if they were accepted: [{protocol, sender, slot, at}]. The
    /// engine takes an offer off its list before it says how it closed,
    /// so each one gone is kept for a while, whatever its answer.
    property var handover: []
    /// What each offer on screen is, for the handover: offerId ->
    /// {protocol, sender}.
    property var offerInfo: ({})
    /// The transfer whose progress is spelt out, or -1.
    property int focusId: -1
    /// Incoming transfers running, and their bytes.
    property int activeCount: 0
    property real activeBytes: 0
    property real activeTotal: 0
    /// Something is on the rings or the tiles.
    property bool busy: false
    /// Goes up with every rebalance(): what reads the transfers reads it.
    property int tick: 0

    readonly property bool receiving: view.engine.receiving
    readonly property var readyProtocols: {
        var out = []
        var s = view.engine.protocolStatuses
        for (var i = 0; i < s.length; i++) {
            if (s[i].state === "ready") {
                out.push(s[i].protocol)
            }
        }
        return out
    }
    readonly property var failedProtocols: {
        var out = []
        var s = view.engine.protocolStatuses
        for (var i = 0; i < s.length; i++) {
            if (s[i].state === "failed") {
                out.push(s[i])
            }
        }
        return out
    }
    readonly property bool wormholeOn: view.engine.protocolEnabled("wormhole")
    readonly property bool crocOn: view.engine.protocolEnabled("croc")
    readonly property bool internetOn: view.wormholeOn || view.crocOn
    readonly property real progress: view.activeTotal > 0 ? Math.min(1, view.activeBytes / view.activeTotal)
                                                          : (view.activeCount > 0 ? 0 : -1)
    readonly property var focused: (view.tick, view.focusId >= 0 ? view.engine.transfer(view.focusId) : null)
    /// An internet transfer or offer is on screen: the tiles give way.
    property bool internetBusy: false

    pulsing: view.current && view.receiving && view.readyProtocols.length > 0 && view.activeCount === 0
    faint: view.activeCount > 0
    /// Left of this phone: how the transfer in focus is doing.
    readonly property real infoWidth: view.ox - view.originSize / 2 - Theme.paddingLarge - view.margin
    readonly property real infoHeight: Theme.fontSizeExtraLarge * 1.3 + Theme.fontSizeExtraSmall * 4
                                       + Theme.iconSizeMedium + Theme.paddingSmall * 3
    reserved: [[0, view.oy + view.originSize / 2 - view.infoHeight,
                view.ox - view.originSize / 2, view.oy + view.originSize / 2 + view.summaryHeight]]

    function internet(protocol) {
        return protocol === "wormhole" || protocol === "croc"
    }
    function tileX(protocol) {
        return protocol === "croc" ? view.rightTileX : view.leftTileX
    }

    /// An incoming transfer is on screen: running, or ended a moment ago.
    function shows(direction, state, transferId) {
        return direction === "incoming" && (state === "active" || view.ended[transferId] !== undefined)
    }

    onSlotSpotsChanged: view.rebalance()

    /// Who is where: the offers and the incoming transfers on screen get
    /// slots; a transfer takes its offer's.
    function rebalance() {
        var keys = []
        var wanted = {}
        var offers = []
        var info = {}
        var busy = false
        var internetBusy = false
        for (var i = 0; i < view.engine.offers.count; i++) {
            var o = view.engine.offers.get(i)
            info[o.offerId] = { protocol: o.protocol, sender: o.sender }
            busy = true
            if (view.internet(o.protocol)) {
                internetBusy = true
                continue
            }
            offers.push(o)
        }
        var superseded = {}
        var now = Date.now()
        var handover = []
        for (var h = 0; h < view.handover.length; h++) {
            if (now - view.handover[h].at < 10000) {
                handover.push(view.handover[h])
            }
        }
        for (var g = 0; g < view.slots.length; g++) {
            var was = view.slots[g]
            if (was.indexOf("offer:") !== 0) {
                continue
            }
            var gone = Number(was.substring(6))
            if (info[gone] === undefined && view.offerInfo[gone] !== undefined) {
                handover.push({ protocol: view.offerInfo[gone].protocol, sender: view.offerInfo[gone].sender,
                                slot: g, at: now })
            }
        }
        var active = 0
        var bytes = 0
        var total = 0
        var newest = -1
        var focusShown = false
        for (var t = 0; t < view.engine.transfers.count; t++) {
            var row = view.engine.transfers.get(t)
            if (!view.shows(row.direction, row.state, row.transferId)) {
                continue
            }
            busy = true
            if (row.state === "active") {
                active++
                bytes += row.bytes
                total += row.total
                if (newest < 0) {
                    newest = row.transferId
                }
            }
            if (row.transferId === view.focusId) {
                focusShown = true
            }
            if (view.internet(row.protocol)) {
                internetBusy = true
                continue
            }
            var key = "transfer:" + row.transferId
            keys.push(key)
            if (view.slots.indexOf(key) >= 0) {
                continue
            }
            // Its offer, still on screen, or just accepted.
            for (var k = 0; k < offers.length; k++) {
                if (offers[k].protocol === row.protocol && offers[k].sender === row.peer
                        && superseded[offers[k].offerId] !== true) {
                    superseded[offers[k].offerId] = true
                    wanted[key] = view.slots.indexOf("offer:" + offers[k].offerId)
                    break
                }
            }
            if (wanted[key] === undefined) {
                for (var m = 0; m < handover.length; m++) {
                    if (handover[m].protocol === row.protocol && handover[m].sender === row.peer) {
                        wanted[key] = handover[m].slot
                        handover.splice(m, 1)
                        break
                    }
                }
            }
        }
        for (var n = 0; n < offers.length; n++) {
            if (superseded[offers[n].offerId] !== true) {
                keys.push("offer:" + offers[n].offerId)
            }
        }
        view.offerInfo = info
        view.handover = handover
        view.activeCount = active
        view.activeBytes = bytes
        view.activeTotal = total
        view.busy = busy
        view.internetBusy = internetBusy
        if (!focusShown) {
            view.focusId = newest
        }
        if (internetBusy) {
            view.moreOpen = false
        }
        view.place(keys, wanted)
        view.tick++
    }

    Connections {
        target: view.engine.offers
        // Qt 5.6 handler syntax.
        onCountChanged: view.rebalance()
    }
    Connections {
        target: view.engine.transfers
        onCountChanged: view.rebalance()
    }
    Connections {
        target: view.engine
        onSettingsChanged: view.rebalance()
        onTransferUpdated: {
            var t = view.engine.transfer(transferId)
            if (t && t.direction === "incoming" && t.state === "active"
                    && (view.focusId < 0 || view.engine.transfer(view.focusId) === null)) {
                view.focusId = transferId
            }
            // Ended: kept on screen from this very update, so that its
            // last state is what is spelt out.
            if (t && t.direction === "incoming" && t.state !== "active") {
                view.ending(transferId)
            } else {
                view.rebalance()
            }
        }
        onTransferEnded: {
            if (direction === "incoming") {
                view.ending(transferId)
            }
        }
    }

    /// An incoming transfer has ended: it stays for `linger`.
    function ending(transferId) {
        if (view.ended[transferId] !== undefined) {
            return
        }
        var next = {}
        for (var id in view.ended) {
            next[id] = view.ended[id]
        }
        next[transferId] = Date.now()
        view.ended = next
        view.rebalance()
        prune.restart()
    }

    /// An ended transfer leaves the screen after `linger`, or when tapped.
    function dismiss(transferId) {
        if (view.ended[transferId] === undefined) {
            return
        }
        var next = {}
        for (var id in view.ended) {
            if (Number(id) !== transferId) {
                next[id] = view.ended[id]
            }
        }
        view.ended = next
        view.rebalance()
    }

    Timer {
        id: prune
        interval: Math.max(10, Math.min(250, view.linger / 2))
        repeat: true
        onTriggered: {
            var now = Date.now()
            var next = {}
            var left = 0
            var changed = false
            for (var id in view.ended) {
                if (now - view.ended[id] < view.linger) {
                    next[id] = view.ended[id]
                    left++
                } else {
                    changed = true
                }
            }
            if (changed) {
                view.ended = next
                view.rebalance()
            }
            if (left === 0) {
                prune.stop()
            }
        }
    }

    function tapped(transferId, state) {
        if (state === "active") {
            view.focusId = transferId
        } else {
            view.dismiss(transferId)
        }
    }

    function receiveWithCode(protocol) {
        if (protocol === "wormhole") {
            pageStack.push(Qt.resolvedUrl("../pages/WormholeReceivePage.qml"), { engine: view.engine })
        }
    }

    function cancelFocused() {
        if (view.focused && view.focused.state === "active") {
            view.engine.cancel(view.focusId)
        }
    }

    Component.onCompleted: view.rebalance()
    Component.onDestruction: view.alive = false

    // ---- The picture ----------------------------------------------------

    CloudButton {
        objectName: "cloud"
        x: view.cloudX - width / 2
        y: view.cloudY - view.cloudHeight / 2
        width: view.cloudWidth
        cloudHeight: view.cloudHeight
        labelHeight: view.summaryHeight
        labelWidth: view.width - 2 * view.margin
        visible: view.internetOn && view.engine.running
        faint: view.busy && !view.internetBusy
        //: Receive screen, under the cloud: tapping it shows Magic Wormhole and croc.
        label: !view.moreOpen && !view.internetBusy ? qsTr("Receive with a code") : ""
        onClicked: {
            if (!view.internetBusy) {
                view.moreOpen = !view.moreOpen
            }
        }
    }

    CodeTile {
        objectName: "wormholeTile"
        x: view.margin
        y: view.tilesY
        width: view.tileWidth
        height: view.tileHeight
        visible: view.wormholeOn && view.moreOpen && !view.internetBusy
        title: "Magic Wormhole"
        //: Receive screen: under Magic Wormhole's name on its tile.
        hint: qsTr("Type the code")
        onClicked: view.receiveWithCode("wormhole")
    }

    CodeTile {
        objectName: "crocTile"
        x: view.width - view.margin - view.tileWidth
        y: view.tilesY
        width: view.tileWidth
        height: view.tileHeight
        visible: view.crocOn && view.moreOpen && !view.internetBusy
        title: "croc"
        //: Receive screen: under croc's name on its tile.
        hint: qsTr("Type the code")
        onClicked: view.receiveWithCode("croc")
    }

    // Offers waiting for an answer: nearby in their slots, from the
    // internet on their protocol's tile.
    Repeater {
        model: view.engine.offers
        delegate: PeerBubble {
            readonly property bool fromInternet: view.internet(model.protocol)
            readonly property int slot: view.slots.indexOf("offer:" + model.offerId)
            objectName: "offerBubble"
            size: view.avatar
            x: (fromInternet ? view.tileX(model.protocol) : view.spotX(slot)) - width / 2
            y: (fromInternet ? view.tileMidY : view.spotY(slot)) - height / 2
            visible: fromInternet || slot >= 0
            name: model.sender
            protocol: fromInternet ? "" : model.protocol
            active: true
        }
    }

    // Incoming transfers: a line to this phone, and the sender filling up.
    Repeater {
        model: view.engine.transfers
        delegate: Item {
            id: incoming
            readonly property bool fromInternet: view.internet(model.protocol)
            readonly property int slot: view.slots.indexOf("transfer:" + model.transferId)
            readonly property bool shown: view.shows(model.direction, model.state, model.transferId)
                                          && (fromInternet || slot >= 0)
            readonly property real px: fromInternet ? view.tileX(model.protocol) : view.spotX(slot)
            readonly property real py: fromInternet ? view.tileMidY : view.spotY(slot)
            anchors.fill: parent
            visible: incoming.shown

            Segment {
                visible: !incoming.fromInternet
                x1: incoming.px
                y1: incoming.py
                x2: view.ox
                y2: view.oy
                startGap: view.avatar / 2
                endGap: view.originSize / 2
            }
            Segment {
                visible: incoming.fromInternet
                x1: incoming.px
                y1: incoming.py
                x2: view.cloudX
                y2: view.cloudY
                startGap: view.avatar / 2
            }
            Segment {
                visible: incoming.fromInternet
                x1: view.cloudX
                y1: view.cloudY
                x2: view.ox
                y2: view.oy
                endGap: view.originSize / 2
            }
            PeerBubble {
                objectName: "incomingBubble"
                size: view.avatar
                x: incoming.px - width / 2
                y: incoming.py - height / 2
                name: model.peer
                protocol: incoming.fromInternet ? "" : model.protocol
                active: true
                progress: model.state === "done" ? 1 : (model.total > 0 ? model.bytes / model.total : 0)
                onClicked: view.tapped(model.transferId, model.state)
            }
        }
    }

    // How far, left of this phone: who, the percentage big, then what is
    // going on.
    Column {
        id: info
        readonly property var t: view.focused
        x: view.margin
        y: view.oy + view.originSize / 2 - height
        width: view.infoWidth
        visible: info.t !== null && view.shows(info.t.direction, info.t.state, view.focusId)
        spacing: Theme.paddingSmall / 2

        Label {
            objectName: "incomingFrom"
            width: parent.width
            horizontalAlignment: Text.AlignRight
            visible: text.length > 0
            text: info.t !== null ? info.t.peer : ""
            textFormat: Text.PlainText
            truncationMode: TruncationMode.Fade
            font.pixelSize: Theme.fontSizeExtraSmall
            color: Theme.highlightColor
        }
        Label {
            objectName: "incomingPercent"
            width: parent.width
            horizontalAlignment: Text.AlignRight
            text: info.t === null ? ""
                  : Math.floor(100 * (info.t.state === "done" ? 1
                                      : info.t.total > 0 ? Math.min(1, info.t.bytes / info.t.total) : 0)) + "%"
            textFormat: Text.PlainText
            visible: info.t !== null && (info.t.state === "active" || info.t.state === "done")
            font.pixelSize: Theme.fontSizeExtraLarge
            color: Theme.highlightColor
        }
        Label {
            objectName: "incomingStatus"
            width: parent.width
            horizontalAlignment: Text.AlignRight
            text: view.statusText(info.t)
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            maximumLineCount: 3
            font.pixelSize: Theme.fontSizeExtraSmall
            color: info.t !== null && info.t.state === "failed" ? Theme.errorColor : Theme.secondaryHighlightColor
        }
        IconButton {
            objectName: "cancelIncoming"
            anchors.right: parent.right
            visible: info.t !== null && info.t.state === "active"
            icon.source: "image://theme/icon-m-clear"
            onClicked: view.cancelFocused()
        }
    }

    function statusText(t) {
        if (!t) {
            return ""
        }
        switch (t.state) {
        case "active":
            //: Progress of a transfer: %1 bytes so far, %2 bytes in all, both formatted.
            return qsTr("%1 of %2").arg(Format.formatFileSize(t.bytes)).arg(Format.formatFileSize(t.total))
        case "done":
            return t.savedCount > 0
                   //: Receive screen: files arrived.
                   ? qsTr("Saved in Downloads/Sukkula")
                   //: Receive screen: a text arrived; it is on the History page.
                   : qsTr("Received. It is in History.")
        case "cancelled":
            //: A transfer was stopped by one of the two sides.
            return qsTr("Cancelled")
        case "failed":
            //: A transfer failed; %1 says why.
            return qsTr("Failed: %1").arg(view.engine.errorText({ code: t.error }))
        }
        return ""
    }

    // This phone, filling up with what comes.
    Item {
        id: origin
        objectName: "receiveOrigin"
        x: view.ox - width / 2
        y: view.oy - height / 2
        width: view.originSize
        height: width

        readonly property color ink: view.activeCount > 0 ? Theme.highlightColor : Theme.primaryColor

        Rectangle {
            anchors.fill: parent
            radius: width / 2
            color: Theme.rgba(Theme.highlightBackgroundColor, 0.15)
            border.width: Math.max(2, Math.round(width / 30))
            border.color: origin.ink
        }

        Item {
            objectName: "receiveFill"
            anchors {
                left: parent.left
                right: parent.right
                bottom: parent.bottom
            }
            height: parent.height * Math.max(0, view.progress)
            clip: true
            visible: view.activeCount > 0

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
            anchors.centerIn: parent
            width: parent.width * 0.6
            height: width
            kind: "phone"
            color: origin.ink
        }
    }

    Label {
        objectName: "deviceNameLabel"
        anchors {
            top: origin.bottom
            topMargin: Theme.paddingSmall / 2
            horizontalCenter: origin.horizontalCenter
        }
        width: view.width - 2 * view.margin
        horizontalAlignment: Text.AlignHCenter
        text: view.engine.effectiveDeviceName
        textFormat: Text.PlainText
        truncationMode: TruncationMode.Fade
        font.pixelSize: Theme.fontSizeExtraSmall
        color: Theme.secondaryHighlightColor
    }

    // What receiving is doing, while nothing is coming.
    Column {
        objectName: "receiveStatus"
        x: view.margin
        width: view.width - 2 * view.margin
        y: view.oy - view.outer * 0.55 - height / 2
        visible: !view.busy
        spacing: Theme.paddingSmall

        Label {
            objectName: "receiveState"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: !view.receiving
                  //: Receive screen: Receive mode was asked for and is not on yet.
                  ? qsTr("Switching on…")
                  : view.readyProtocols.length > 0
                    //: Receive screen with nothing coming yet.
                    ? qsTr("Waiting for offers. Nothing is saved until you accept it.")
                    : view.failedProtocols.length > 0
                      //: Receive screen: no protocol could start.
                      ? qsTr("Nobody nearby can see this phone.")
                      //: Receive screen: the protocols are starting.
                      : qsTr("Starting…")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryHighlightColor
        }
        Label {
            objectName: "visibleVia"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            visible: view.receiving && view.readyProtocols.length > 0
            text: {
                var names = []
                for (var i = 0; i < view.readyProtocols.length; i++) {
                    names.push(view.engine.protocolName(view.readyProtocols[i]))
                }
                //: Receive screen: the protocols this phone can be found over; %1 lists them.
                return qsTr("Visible over %1").arg(names.join(", "))
            }
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeExtraSmall
            color: Theme.secondaryColor
        }
        Repeater {
            model: view.receiving ? view.failedProtocols : []
            delegate: Label {
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                //: A protocol could not start; %1 is its name, %2 why.
                text: qsTr("%1 could not start: %2").arg(view.engine.protocolName(modelData.protocol))
                                                  .arg(view.engine.errorText({ code: modelData.errorCode }))
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.errorColor
            }
        }
    }
}
