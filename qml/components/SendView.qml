// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The Send tab (F-C6): what to send, then who to.
 *
 * With nothing chosen, four tiles open the platform's pickers -- photos,
 * videos, documents, any file -- and a line at the foot says who is
 * nearby, since discovery runs from the start. Files shared from another
 * app arrive here already chosen.
 *
 * With files chosen, a row says what they are, with + to add more and a
 * cross to clear them (with a remorse to undo). Under "Nearby", every
 * device discovery found (F-LS1, F-QS1) and every paired Bluetooth device
 * (F-BT1), one row each: a device found over both Quick Share and
 * LocalSend, by the same name, is one row, which is a guess, so its menu
 * says which way to send. The protocol is only ever in the grey line.
 * Under "Far away", sending with a code, on a page of its own (F-MW1,
 * F-CR1).
 *
 * Tapping a device sends to it at once. Its row then shows how far the
 * send has got, with a cross to stop it, and the other rows wait. After
 * the send the files stay chosen, so they can go to another device too.
 *
 * Peer names are the peers' own (after S2) and are shown only in
 * IconListItem's and ProgressRow's plain-text labels.
 */
Item {
    id: view

    property QtObject engine
    /// What to send: a Payload.
    property QtObject payload
    /// Where to say what went wrong: a Banner.
    property Item banner
    /// The page's RemorsePopup, for clearing what is chosen.
    property Item remorse
    /// Discovery is running.
    property bool discovering: false
    /// This is the tab on screen.
    property bool current: true
    /// Room at the top for the tabs.
    property real topInset: 0
    /// How long an ended send stays in its row, in ms.
    property int linger: 4000
    property bool alive: true

    /// Every device a send can go to, one per name: [{key, name,
    /// deviceType, peers: [{protocol, target, peerId, deviceModel,
    /// deviceType, address, fingerprint}]}]. LAN devices by name first,
    /// then the paired Bluetooth devices no LAN device shares a name with.
    property var devices: []

    /// The send on screen, or null: {key, protocol, name, transferId}.
    /// `key` is its device's, or "code" for a send with a code.
    property var outgoing: null
    /// The engine's row for the send's transfer, or null.
    property var outgoingTransfer: null
    /// The code of a send with a code, once the engine has it.
    property string outgoingCode: ""
    /// The QR code for it: {size, rows}, or null.
    property var outgoingQr: null
    /// Waiting for the reply to the send command.
    property bool sending: false
    /// A send with a code was given up before its transfer was known: it
    /// is cancelled as soon as it is.
    property bool cancelWhenKnown: false
    /// The pickers (Pickers.qml), made on first use.
    property QtObject pickers: null

    readonly property bool hasPayload: view.payload.itemCount > 0
    readonly property bool hasOutgoing: view.outgoing !== null
    readonly property string outgoingState: !view.hasOutgoing ? ""
        : view.outgoingTransfer !== null ? view.outgoingTransfer.state
        : view.outgoing.transferId >= 0 ? "active" : "starting"
    readonly property bool outgoingEnded: view.outgoingState === "done" || view.outgoingState === "failed"
                                       || view.outgoingState === "cancelled"
    /// A send with a code whose receiver has not come yet.
    readonly property bool codeWaiting: view.hasOutgoing && view.outgoing.key === "code"
        && (view.outgoingState === "starting" || (view.outgoingState === "active"
            && (view.outgoingTransfer === null || view.outgoingTransfer.bytes <= 0)))

    readonly property bool wormholeOn: view.engine.protocolEnabled("wormhole")
    readonly property bool crocOn: view.engine.protocolEnabled("croc")
    readonly property bool codeOn: view.wormholeOn || view.crocOn
    /// Something that finds devices nearby is switched on.
    readonly property bool nearbyOn: view.engine.protocolEnabled("local_send")
                                     || view.engine.protocolEnabled("quick_share")
                                     || view.engine.protocolEnabled("bluetooth")
    /// How many devices discovery found, not counting the paired ones.
    readonly property int foundCount: {
        var n = 0
        for (var i = 0; i < view.devices.length; i++) {
            if (view.devices[i].found) {
                n++
            }
        }
        return n
    }
    /// The files' kind in common, for the icon and the words.
    readonly property string payloadKind: view.engine.commonKind(view.payload.names(), view.payload.itemCount)

    implicitHeight: column.height

    Component.onCompleted: view.rebuild()
    Component.onDestruction: view.alive = false

    // ---- Devices --------------------------------------------------------

    function rebuild() {
        var byName = {}
        var lan = []
        var lanProtocols = ["quick_share", "local_send"]
        for (var p = 0; p < lanProtocols.length; p++) {
            var protocol = lanProtocols[p]
            var model = view.engine.peerModel(protocol)
            if (!model || !view.engine.protocolEnabled(protocol)) {
                continue
            }
            for (var i = 0; i < model.count; i++) {
                var row = model.get(i)
                var key = String(row.name).toLowerCase()
                var device = byName[key]
                if (!device) {
                    device = { key: "device:" + key, name: row.name, deviceType: "unknown", found: true,
                               peers: [] }
                    byName[key] = device
                    lan.push(device)
                }
                if (device.deviceType === "unknown") {
                    device.deviceType = row.deviceType
                }
                device.peers.push({
                    protocol: protocol, target: { protocol: protocol, peer: row.peerId },
                    peerId: row.peerId, deviceModel: row.deviceModel, deviceType: row.deviceType,
                    address: row.address, fingerprint: row.fingerprint
                })
            }
        }
        var paired = []
        if (view.engine.protocolEnabled("bluetooth")) {
            var devices = view.engine.bluetoothDevices
            for (var d = 0; d < devices.count; d++) {
                var bt = devices.get(d)
                var name = bt.name.length > 0 ? bt.name : bt.address
                var btKey = String(name).toLowerCase()
                var peer = {
                    protocol: "bluetooth", target: { protocol: "bluetooth", address: bt.address },
                    peerId: "", deviceModel: "", deviceType: "unknown", address: bt.address, fingerprint: ""
                }
                if (byName[btKey]) {
                    byName[btKey].peers.push(peer)
                    continue
                }
                var one = { key: "device:" + btKey, name: name, deviceType: "unknown", found: false,
                            peers: [peer] }
                byName[btKey] = one
                paired.push(one)
            }
        }
        var byLowerName = function (a, b) {
            var x = a.name.toLowerCase()
            var y = b.name.toLowerCase()
            return x < y ? -1 : x > y ? 1 : 0
        }
        lan.sort(byLowerName)
        paired.sort(byLowerName)
        view.devices = lan.concat(paired)
    }

    Connections {
        target: view.engine.localSendPeers
        // Qt 5.6 handler syntax.
        onCountChanged: view.rebuild()
        onDataChanged: view.rebuild()
    }
    Connections {
        target: view.engine.quickSharePeers
        onCountChanged: view.rebuild()
        onDataChanged: view.rebuild()
    }
    Connections {
        target: view.engine.bluetoothDevices
        onCountChanged: view.rebuild()
    }
    Connections {
        target: view.engine
        onSettingsChanged: view.rebuild()
        onProtocolsChanged: view.rebuild()
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

    /// "Phone · Quick Share, LocalSend": what it says it is, and the ways
    /// it can be sent to.
    function deviceLine(device) {
        var names = []
        for (var i = 0; i < device.peers.length; i++) {
            names.push(view.engine.protocolName(device.peers[i].protocol))
        }
        var kind = device.found ? view.engine.deviceTypeText(device.deviceType)
                                //: Send tab: a paired Bluetooth device, whose kind Sukkula cannot tell.
                                : qsTr("Paired device")
        return kind.length > 0 ? kind + " · " + names.join(", ") : names.join(", ")
    }

    function deviceGlyph(device) {
        if (!device.found) {
            return "bluetooth"
        }
        switch (device.deviceType) {
        case "tablet": return "tablet"
        case "computer": return "computer"
        }
        return "phone"
    }

    // ---- Sending --------------------------------------------------------

    /// A device was tapped: sent to over its first way, or over
    /// `protocol` when its menu said which.
    function choose(device, protocol) {
        if (view.hasOutgoing && !view.outgoingEnded) {
            return
        }
        if (view.sending || !device || device.peers.length === 0 || !view.hasPayload) {
            return
        }
        var peer = device.peers[0]
        for (var i = 0; protocol && i < device.peers.length; i++) {
            if (device.peers[i].protocol === protocol) {
                peer = device.peers[i]
            }
        }
        view.dismiss()
        view.start({ key: device.key, protocol: peer.protocol, name: device.name, target: peer.target })
    }

    /// The protocol a send with a code starts with: Magic Wormhole for
    /// one file, croc for several (Magic Wormhole takes one at a time), and
    /// whichever is switched on.
    function codeProtocol() {
        if (view.wormholeOn && (view.payload.itemCount === 1 || !view.crocOn)) {
            return "wormhole"
        }
        return view.crocOn ? "croc" : ""
    }

    /// Starts a send with a code over `protocol`, giving up the one on its
    /// way, if any, first. False when it cannot be started.
    function startCode(protocol) {
        if (!view.hasPayload || view.sending || (protocol !== "wormhole" && protocol !== "croc")
                || !view.engine.protocolEnabled(protocol)) {
            return false
        }
        if (protocol === "wormhole" && view.payload.itemCount !== 1) {
            //: Send with a code: Magic Wormhole chosen with several files.
            view.banner.show(qsTr("Magic Wormhole sends one file at a time. Choose croc to send several."))
            return false
        }
        if (view.hasOutgoing && !view.outgoingEnded) {
            if (view.outgoing.key !== "code" || !view.codeWaiting) {
                return false
            }
            view.giveUpCode()
        }
        view.dismiss()
        view.start({ key: "code", protocol: protocol, name: "", target: { protocol: protocol } })
        return true
    }

    /// The code page was left before anyone came for the code: the send
    /// is given up, since nobody can use a code no longer shown.
    function giveUpCode() {
        if (!view.hasOutgoing || view.outgoing.key !== "code" || !view.codeWaiting) {
            return
        }
        if (view.outgoing.transferId >= 0) {
            view.engine.cancel(view.outgoing.transferId)
        } else {
            view.cancelWhenKnown = true
        }
        lingering.stop()
        view.outgoing = null
        view.outgoingTransfer = null
        view.outgoingCode = ""
        view.outgoingQr = null
    }

    function start(what) {
        view.sending = true
        view.cancelWhenKnown = false
        view.outgoing = { key: what.key, protocol: what.protocol, name: what.name, transferId: -1 }
        view.outgoingTransfer = null
        view.outgoingCode = ""
        view.outgoingQr = null
        var self = view
        view.engine.send(what.target, view.payload.items(), function (ok, error, transfer) {
            if (self.alive !== true) {
                return
            }
            self.sending = false
            if (self.cancelWhenKnown) {
                self.cancelWhenKnown = false
                if (ok) {
                    self.engine.cancel(transfer)
                }
                return
            }
            if (!ok) {
                self.outgoing = null
                self.banner.show(self.engine.errorText(error))
                return
            }
            var f = self.outgoing
            if (f) {
                self.outgoing = { key: f.key, protocol: f.protocol, name: f.name, transferId: transfer }
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
        view.outgoingQr = c ? c.qr : null
    }

    onOutgoingEndedChanged: {
        if (view.outgoingEnded) {
            lingering.restart()
        }
    }

    /// Back to the plain list once a send has ended. What was sent stays
    /// chosen.
    function dismiss() {
        if (!view.outgoingEnded) {
            return
        }
        lingering.stop()
        view.outgoing = null
        view.outgoingTransfer = null
        view.outgoingCode = ""
        view.outgoingQr = null
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

    function statusText() {
        var t = view.outgoingTransfer
        switch (view.outgoingState) {
        case "starting":
            //: Send tab: a send was asked for, the engine has not answered yet.
            return qsTr("Connecting…")
        case "active":
            if (t && t.bytes > 0) {
                //: Send tab: the files are going.
                return qsTr("Sending…")
            }
            //: Send tab: the other device has been asked and has not answered yet.
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

    // ---- Choosing -------------------------------------------------------

    /// Opens the picker for "photo", "video", "document" or "file".
    function pick(kind) {
        if (view.payload.itemCount >= view.payload.maxFiles) {
            //: Send tab: the most files one send can carry are chosen already.
            view.banner.show(qsTr("That is as many files as one send can take."))
            return
        }
        if (view.pickers === null) {
            var made = Qt.createComponent(Qt.resolvedUrl("../pages/Pickers.qml"))
            if (made.status === Component.Ready) {
                view.pickers = made.createObject(view)
                view.pickers.picked.connect(view.picked)
            }
        }
        if (view.pickers !== null) {
            pageStack.push(view.pickers.component(kind))
            return
        }
        // No dialogs for several files here: the file browser, one at a time.
        var one = pageStack.push(Qt.resolvedUrl("../pages/SingleFilePicker.qml"))
        if (one) {
            one.picked.connect(view.picked)
        }
    }

    /// [{path, size}] from a picker.
    function picked(files) {
        var list = Array.isArray(files) ? files : []
        for (var i = 0; i < list.length; i++) {
            view.payload.addFile(list[i].path, list[i].size)
        }
    }

    /// Everything chosen, off again -- after a moment to change one's mind.
    function clearPayload() {
        if (view.hasOutgoing && !view.outgoingEnded) {
            return
        }
        //: Remorse: the chosen files are about to be cleared.
        view.remorse.execute(qsTr("Clearing"), function () {
            if (view.alive === true && !(view.hasOutgoing && !view.outgoingEnded)) {
                view.dismiss()
                view.payload.clear()
            }
        })
    }

    /// Opens the page for sending with a code, and starts the send.
    function openCode() {
        var protocol = view.codeProtocol()
        if (protocol === "") {
            return
        }
        var sameSend = view.hasOutgoing && view.outgoing.key === "code" && !view.outgoingEnded
        if (!sameSend && !view.startCode(protocol)) {
            return
        }
        pageStack.push(Qt.resolvedUrl("../pages/SendCodePage.qml"), { engine: view.engine, view: view })
    }

    function openDevice(device) {
        pageStack.push(Qt.resolvedUrl("../pages/DevicePage.qml"), { engine: view.engine, device: device })
    }

    // ---- The list -------------------------------------------------------

    Column {
        id: column
        width: parent.width

        Item {
            width: 1
            height: view.topInset
        }

        // ---- Nothing chosen: what to send ------------------------------

        Label {
            objectName: "sendQuestion"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            visible: !view.hasPayload
            topPadding: Theme.paddingLarge
            bottomPadding: Theme.paddingLarge
            //: Send tab with nothing chosen yet.
            text: qsTr("What would you like to send?")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeExtraLarge
            color: Theme.highlightColor
        }

        Repeater {
            model: view.hasPayload ? [] : [
                //: Send tab: the tile that opens Gallery's photos.
                { kind: "photo", name: "pickPhotos", title: qsTr("Photos"),
                  //: Send tab: under the Photos and Videos tiles.
                  hint: qsTr("From Gallery") },
                //: Send tab: the tile that opens Gallery's videos.
                { kind: "video", name: "pickVideos", title: qsTr("Videos"), hint: qsTr("From Gallery") },
                //: Send tab: the tile that opens the documents list.
                { kind: "document", name: "pickDocuments", title: qsTr("Documents"),
                  //: Send tab: under the Documents tile.
                  hint: qsTr("PDFs, notes, sheets") },
                //: Send tab: the tile that opens the file browser.
                { kind: "file", name: "pickFiles", title: qsTr("Any file"),
                  //: Send tab: under the Any file tile.
                  hint: qsTr("Browse your folders") }
            ]
            delegate: Item {
                width: column.width
                height: tile.height + Theme.paddingSmall

                BackgroundItem {
                    id: tile
                    objectName: modelData.name
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    height: Theme.itemSizeExtraLarge
                    onClicked: view.pick(modelData.kind)

                    Rectangle {
                        anchors.fill: parent
                        radius: Theme.paddingSmall
                        color: Theme.rgba(Theme.primaryColor, 0.05)
                    }
                    Glyph {
                        id: tileGlyph
                        x: Theme.paddingLarge
                        anchors.verticalCenter: parent.verticalCenter
                        width: Theme.iconSizeMedium
                        height: width
                        kind: modelData.kind
                        color: tile.highlighted ? Theme.highlightColor : Theme.primaryColor
                    }
                    Column {
                        anchors {
                            left: tileGlyph.right
                            leftMargin: Theme.paddingLarge
                            right: parent.right
                            rightMargin: Theme.paddingLarge
                            verticalCenter: parent.verticalCenter
                        }
                        Label {
                            width: parent.width
                            text: modelData.title
                            textFormat: Text.PlainText
                            truncationMode: TruncationMode.Fade
                            color: tile.highlighted ? Theme.highlightColor : Theme.primaryColor
                        }
                        Label {
                            width: parent.width
                            text: modelData.hint
                            textFormat: Text.PlainText
                            truncationMode: TruncationMode.Fade
                            font.pixelSize: Theme.fontSizeSmall
                            color: tile.highlighted ? Theme.secondaryHighlightColor : Theme.secondaryColor
                        }
                    }
                }
            }
        }

        // Who is about, while nothing is chosen yet.
        Row {
            anchors.horizontalCenter: parent.horizontalCenter
            visible: !view.hasPayload && nearbyLine.text.length > 0
            spacing: Theme.paddingMedium
            topPadding: Theme.paddingLarge

            BusyIndicator {
                anchors.verticalCenter: parent.verticalCenter
                size: BusyIndicatorSize.ExtraSmall
                running: view.discovering && view.current && view.foundCount === 0
                visible: running
            }
            Label {
                id: nearbyLine
                objectName: "nearbyLine"
                width: Math.min(implicitWidth, column.width - 2 * Theme.horizontalPageMargin
                                - Theme.iconSizeExtraSmall - Theme.paddingMedium)
                anchors.verticalCenter: parent.verticalCenter
                text: view.nearbyText()
                textFormat: Text.PlainText
                truncationMode: TruncationMode.Fade
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
        }

        // ---- Files chosen: what they are --------------------------------

        Item {
            objectName: "payloadRow"
            width: parent.width
            height: Theme.itemSizeLarge
            visible: view.hasPayload

            Glyph {
                id: payloadGlyph
                x: Theme.horizontalPageMargin
                anchors.verticalCenter: parent.verticalCenter
                width: Theme.iconSizeMedium
                height: width
                kind: view.payloadKind
                color: Theme.primaryColor
            }
            Column {
                anchors {
                    left: payloadGlyph.right
                    leftMargin: Theme.paddingLarge
                    right: addMore.left
                    rightMargin: Theme.paddingSmall
                    verticalCenter: parent.verticalCenter
                }
                Label {
                    objectName: "payloadSummary"
                    width: parent.width
                    text: view.engine.bundleName(view.payload.names(), view.payload.itemCount)
                    textFormat: Text.PlainText
                    truncationMode: TruncationMode.Fade
                }
                Label {
                    objectName: "payloadSize"
                    width: parent.width
                    visible: view.payload.totalSize >= 0
                    text: Format.formatFileSize(Math.max(0, view.payload.totalSize))
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryColor
                }
            }
            IconButton {
                id: addMore
                objectName: "addMore"
                anchors {
                    right: clear.left
                    verticalCenter: parent.verticalCenter
                }
                enabled: !(view.hasOutgoing && !view.outgoingEnded)
                icon.source: "image://theme/icon-m-add"
                onClicked: view.pick(view.payloadKind)
            }
            IconButton {
                id: clear
                objectName: "clearPayload"
                anchors {
                    right: parent.right
                    rightMargin: Theme.horizontalPageMargin - Theme.paddingMedium
                    verticalCenter: parent.verticalCenter
                }
                enabled: !(view.hasOutgoing && !view.outgoingEnded)
                icon.source: "image://theme/icon-m-clear"
                onClicked: view.clearPayload()
            }
        }

        // ---- Who to: nearby ------------------------------------------

        Item {
            width: parent.width
            height: nearbyHeader.height
            visible: view.hasPayload

            Row {
                x: Theme.horizontalPageMargin
                anchors.verticalCenter: parent.verticalCenter
                spacing: Theme.paddingMedium
                visible: view.discovering && view.nearbyOn && !(view.hasOutgoing && !view.outgoingEnded)

                BusyIndicator {
                    anchors.verticalCenter: parent.verticalCenter
                    size: BusyIndicatorSize.ExtraSmall
                    running: parent.visible && view.current
                }
                Label {
                    anchors.verticalCenter: parent.verticalCenter
                    //: Send tab, beside "Nearby": discovery is still running.
                    text: qsTr("Looking for more")
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryColor
                }
            }
            SectionHeader {
                id: nearbyHeader
                //: Send tab: the section of devices to send to on the same network or paired.
                text: qsTr("Nearby")
            }
        }

        Repeater {
            model: view.hasPayload ? view.devices : []
            delegate: Item {
                id: deviceSlot
                readonly property bool sendingHere: view.hasOutgoing && view.outgoing.key === modelData.key
                width: column.width
                height: deviceSlot.sendingHere ? deviceProgress.height : deviceRow.height

                IconListItem {
                    id: deviceRow
                    objectName: "deviceRow"
                    visible: !deviceSlot.sendingHere
                    glyph: view.deviceGlyph(modelData)
                    title: modelData.name
                    subtitle: view.deviceLine(modelData)
                    dimmed: view.hasOutgoing && !view.outgoingEnded
                    menu: deviceMenu
                    onClicked: view.choose(modelData)

                    Component {
                        id: deviceMenu
                        ContextMenu {
                            Repeater {
                                model: modelData.peers
                                delegate: MenuItem {
                                    objectName: "sendWith"
                                    //: A device's menu: send to it over this protocol; %1 is its name, e.g. "Quick Share".
                                    text: qsTr("Send with %1").arg(view.engine.protocolName(modelData.protocol))
                                    onClicked: view.choose(deviceSlot.device, modelData.protocol)
                                }
                            }
                            MenuItem {
                                objectName: "aboutDevice"
                                //: A device's menu: the page with what Sukkula knows about it.
                                text: qsTr("About this device")
                                onClicked: view.openDevice(deviceSlot.device)
                            }
                        }
                    }
                }

                ProgressRow {
                    id: deviceProgress
                    objectName: "sendProgress"
                    visible: deviceSlot.sendingHere
                    title: modelData.name
                    status: deviceSlot.sendingHere ? view.statusText() : ""
                    phase: deviceSlot.sendingHere ? view.outgoingState : "active"
                    bytes: deviceSlot.sendingHere && view.outgoingTransfer ? view.outgoingTransfer.bytes : 0
                    total: deviceSlot.sendingHere && view.outgoingTransfer ? view.outgoingTransfer.total : 0
                    onCancelClicked: view.cancelSend()
                }

                /// The device this row is, for its menu.
                readonly property var device: modelData
            }
        }

        // A send to a device that has since gone from the list stays in view.
        ProgressRow {
            objectName: "sendProgress"
            visible: view.hasOutgoing && view.outgoing.key !== "code" && view.indexOfDevice(view.outgoing.key) < 0
            title: view.hasOutgoing ? view.outgoing.name : ""
            status: visible ? view.statusText() : ""
            phase: visible ? view.outgoingState : "active"
            bytes: visible && view.outgoingTransfer ? view.outgoingTransfer.bytes : 0
            total: visible && view.outgoingTransfer ? view.outgoingTransfer.total : 0
            onCancelClicked: view.cancelSend()
        }

        Label {
            objectName: "nearbyHint"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            visible: view.hasPayload
            topPadding: Theme.paddingMedium
            bottomPadding: Theme.paddingMedium
            text: !view.nearbyOn
                  //: Send tab: Quick Share, LocalSend and Bluetooth are all switched off.
                  ? qsTr("Sending nearby is switched off in Settings.")
                  : view.devices.length === 0 && view.discovering
                    //: Send tab: discovery is running and has found nobody yet.
                    ? qsTr("Looking for devices nearby…")
                    //: Send tab, under the devices nearby.
                    : qsTr("Someone missing? They need to be on the same Wi-Fi, with their device ready to receive.")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }

        // ---- Who to: far away -----------------------------------------

        SectionHeader {
            visible: view.hasPayload && view.codeOn
            //: Send tab: the section for sending over the internet with a code.
            text: qsTr("Far away")
        }

        IconListItem {
            objectName: "sendWithCode"
            visible: view.hasPayload && view.codeOn && !(view.hasOutgoing && view.outgoing.key === "code"
                                                         && !view.codeWaiting)
            glyph: "qr"
            //: Send tab: sending over the internet with a code.
            title: qsTr("Send with a code")
            subtitle: view.codeWaiting
                      //: Send tab: a send with a code waits for the other side.
                      ? qsTr("Waiting for them to type the code…")
                      //: Send tab: under "Send with a code".
                      : qsTr("They scan it, or type it into their app")
            dimmed: view.hasOutgoing && !view.outgoingEnded && view.outgoing.key !== "code"
            onClicked: view.openCode()
        }

        ProgressRow {
            objectName: "codeProgress"
            visible: view.hasOutgoing && view.outgoing.key === "code" && !view.codeWaiting
            //: Send tab: the row of a send with a code once its receiver has come.
            title: qsTr("Send with a code")
            status: visible ? view.statusText() : ""
            phase: visible ? view.outgoingState : "active"
            bytes: visible && view.outgoingTransfer ? view.outgoingTransfer.bytes : 0
            total: visible && view.outgoingTransfer ? view.outgoingTransfer.total : 0
            onCancelClicked: view.cancelSend()
        }

        Item {
            width: 1
            height: Theme.paddingLarge
        }
    }

    function indexOfDevice(key) {
        for (var i = 0; i < view.devices.length; i++) {
            if (view.devices[i].key === key) {
                return i
            }
        }
        return -1
    }

    /// The line at the foot while nothing is chosen: who is about.
    function nearbyText() {
        var found = []
        for (var i = 0; i < view.devices.length; i++) {
            if (view.devices[i].found) {
                found.push(view.devices[i].name)
            }
        }
        if (found.length === 1) {
            //: Send tab, at the foot: one device nearby; %1 is its name.
            return qsTr("%1 is nearby").arg(found[0])
        }
        if (found.length > 1) {
            //: Send tab, at the foot: devices nearby; %1 is one's name, %n how many more.
            return qsTr("%1 and %n more nearby", "", found.length - 1).arg(found[0])
        }
        if (!view.nearbyOn) {
            //: Send tab: Quick Share, LocalSend and Bluetooth are all switched off.
            return qsTr("Sending nearby is switched off in Settings.")
        }
        //: Send tab: discovery is running and has found nobody yet.
        return view.discovering ? qsTr("Looking for devices nearby…") : ""
    }
}
