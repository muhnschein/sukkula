// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The Send tab (F-C6): what to send, then who to, round the anchor
 * (StatusHero), which stays where it is from step to step.
 *
 * Nothing chosen: "Ready to send", who is nearby in the grey line -- the
 * anchor's sweep going round while discovery runs, which it does from
 * the start -- and "Choose files", the platform's content picker, which
 * bundles pictures, videos, music and documents and the file system as
 * piirit's attach button does. Files shared from another app arrive here
 * already chosen.
 *
 * Files chosen: what they are ("3 photos", 8.2 MB) with Add files and
 * Clear (with a remorse to undo). Under "Nearby", every device discovery
 * found (F-LS1, F-QS1) and every paired Bluetooth device (F-BT1), one row
 * each: a device found over both Quick Share and LocalSend, by the same
 * name, is one row, which is a guess, so its menu says which way to send.
 * The protocol is only ever in the grey line. Under "Far away", sending
 * with a code, on a page of its own (F-MW1, F-CR1).
 *
 * Tapping a device sends to it at once, and the tab is that send: the
 * anchor waiting for an answer, then filling as the files go, with
 * Cancel; then a check and "Send to another" (the files stay chosen) or
 * "Done" (they go), or a cross, why, and "Try again" or "Back". A send
 * cancelled here goes straight back to the devices.
 *
 * Peer names are the peers' own (after S2) and are shown only in
 * IconListItem's and StatusHero's plain-text labels.
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
    /// The app is in front.
    property bool foreground: true
    /// Room at the top for the tabs.
    property real topInset: 0
    /// The height of the list this is in, which places the anchor.
    property real viewHeight: 0
    property bool alive: true

    /// Every device a send can go to, one per name: [{key, name,
    /// deviceType, peers: [{protocol, target, peerId, deviceModel,
    /// deviceType, address, fingerprint}]}]. LAN devices by name first,
    /// then the paired Bluetooth devices no LAN device shares a name with.
    property var devices: []

    /// The send on screen, or null: {key, protocol, name, glyph, target,
    /// transferId}. `key` is its device's, or "code" for a send with a
    /// code.
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
    /// The send on screen was cancelled here: it goes once it has ended.
    property bool cancelling: false
    /// The pickers (Pickers.qml), made on first use.
    property QtObject pickers: null

    readonly property bool hasPayload: view.payload.itemCount > 0
    readonly property bool hasOutgoing: view.outgoing !== null
    readonly property string outgoingState: !view.hasOutgoing ? ""
        : view.outgoingTransfer !== null ? view.outgoingTransfer.state
        : view.outgoing.transferId >= 0 ? "active" : "starting"
    readonly property bool outgoingEnded: view.outgoingState === "done" || view.outgoingState === "failed"
                                       || view.outgoingState === "cancelled"
    /// Bytes are going.
    readonly property bool outgoingMoving: view.outgoingState === "active" && view.outgoingTransfer !== null
                                           && view.outgoingTransfer.bytes > 0
    /// A send with a code whose receiver has not come yet.
    readonly property bool codeWaiting: view.hasOutgoing && view.outgoing.key === "code"
        && (view.outgoingState === "starting" || (view.outgoingState === "active" && !view.outgoingMoving))

    readonly property bool wormholeOn: view.engine.protocolEnabled("wormhole")
    readonly property bool crocOn: view.engine.protocolEnabled("croc")
    readonly property bool codeOn: view.wormholeOn || view.crocOn
    /// Something that finds devices nearby is switched on.
    readonly property bool nearbyOn: view.engine.protocolEnabled("local_send")
                                     || view.engine.protocolEnabled("quick_share")
                                     || view.engine.protocolEnabled("bluetooth")
    /// Devices are being looked for: the anchor's sweep.
    readonly property bool looking: view.discovering && view.nearbyOn
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
    /// "3 photos", or the one file's name.
    readonly property string payloadName: view.engine.bundleName(view.payload.names(), view.payload.itemCount)
    /// "8.2 MB", or "" where a picker did not say.
    readonly property string payloadSize: view.payload.totalSize >= 0 ? Format.formatFileSize(view.payload.totalSize)
                                                                      : ""
    /// The anchor's mode for the step on screen.
    readonly property string heroMode: !view.hasOutgoing ? (view.looking ? "looking" : "idle")
        : view.outgoingState === "done" ? "done"
        : view.outgoingEnded ? "failed"
        : view.outgoingMoving ? "progress" : "waiting"

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
        if (view.hasOutgoing || view.sending || !device || device.peers.length === 0 || !view.hasPayload) {
            return
        }
        var peer = device.peers[0]
        for (var i = 0; protocol && i < device.peers.length; i++) {
            if (device.peers[i].protocol === protocol) {
                peer = device.peers[i]
            }
        }
        view.start({ key: device.key, protocol: peer.protocol, name: device.name,
                     glyph: view.deviceGlyph(device), target: peer.target })
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
        view.start({ key: "code", protocol: protocol, name: "", glyph: "qr", target: { protocol: protocol } })
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
        view.cancelling = false
        view.outgoing = null
        view.outgoingTransfer = null
        view.outgoingCode = ""
        view.outgoingQr = null
    }

    function start(what) {
        view.sending = true
        view.cancelWhenKnown = false
        view.cancelling = false
        view.outgoing = { key: what.key, protocol: what.protocol, name: what.name, glyph: what.glyph,
                          target: what.target, transferId: -1 }
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
                self.outgoing = { key: f.key, protocol: f.protocol, name: f.name, glyph: f.glyph,
                                  target: f.target, transferId: transfer }
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

    // A send cancelled here goes as soon as it has ended -- once the
    // update that ended it is through; any other ending stays until a
    // button says where to.
    onOutgoingEndedChanged: {
        if (view.outgoingEnded && view.cancelling) {
            dismissSoon.restart()
        }
    }

    Timer {
        id: dismissSoon
        interval: 0
        onTriggered: view.dismiss()
    }

    /// Back to the devices once a send has ended. What was sent stays
    /// chosen.
    function dismiss() {
        if (!view.outgoingEnded) {
            return
        }
        view.cancelling = false
        view.outgoing = null
        view.outgoingTransfer = null
        view.outgoingCode = ""
        view.outgoingQr = null
    }

    /// "Done": back to the start, with nothing chosen.
    function finish() {
        if (!view.outgoingEnded) {
            return
        }
        view.dismiss()
        view.payload.clear()
    }

    /// "Try again": the same files to the same device, the same way; a
    /// send with a code gets a new code.
    function retry() {
        if (!view.outgoingEnded) {
            return
        }
        var last = view.outgoing
        view.dismiss()
        if (last.key === "code") {
            view.openCode()
        } else {
            view.start({ key: last.key, protocol: last.protocol, name: last.name, glyph: last.glyph,
                         target: last.target })
        }
    }

    /// "Cancel": a send on its way is stopped, and one still waiting for
    /// its code or its receiver is given up.
    function cancelSend() {
        if (!view.hasOutgoing || view.outgoingEnded) {
            return
        }
        if (view.codeWaiting) {
            view.giveUpCode()
            return
        }
        if (view.outgoing.transferId >= 0 && view.outgoingState === "active") {
            view.cancelling = true
            view.engine.cancel(view.outgoing.transferId)
        }
    }

    TransferRate {
        id: rate
        bytes: view.outgoingTransfer ? view.outgoingTransfer.bytes : 0
        total: view.outgoingTransfer ? view.outgoingTransfer.total : 0
        active: view.outgoingState === "active"
    }
    onOutgoingChanged: {
        if (view.outgoing === null || view.outgoing.transferId < 0) {
            rate.reset()
        }
    }

    // ---- The hero's words -------------------------------------------------

    function heroTitle() {
        if (!view.hasPayload && !view.hasOutgoing) {
            //: Send tab with nothing chosen: the title under the anchor.
            return qsTr("Ready to send")
        }
        if (!view.hasOutgoing) {
            return view.payloadName
        }
        switch (view.heroMode) {
        case "done":
            //: Send tab: the files arrived.
            return qsTr("Sent")
        case "failed":
            //: Send tab: the files did not arrive; the line under it says why.
            return qsTr("Not sent")
        }
        return view.outgoing.key === "code" ? view.payloadName : view.outgoing.name
    }

    function heroSubtitle() {
        if (!view.hasPayload && !view.hasOutgoing) {
            return view.nearbyText()
        }
        if (!view.hasOutgoing) {
            return view.payloadSize
        }
        var code = view.outgoing.key === "code"
        switch (view.heroMode) {
        case "waiting":
            if (view.outgoingState === "starting" || (code && view.outgoingCode === "")) {
                return code
                       //: Send with a code: waiting for the server to hand out a code.
                       ? qsTr("Getting a code…")
                       //: Send tab: a send was asked for, the engine has not answered yet.
                       : qsTr("Connecting…")
            }
            return code
                   //: Send tab: a send with a code waits for the other side to scan or type the code.
                   ? qsTr("Waiting for the receiver…")
                   //: Send tab: the other device has been asked and has not answered yet.
                   : qsTr("Waiting for them to accept…")
        case "progress":
            return rate.sizeLine()
        case "done":
            return code ? view.withSize(view.payloadName)
                        //: Send tab, under "Sent": what went where; %1 is what, e.g. "3 photos", %2 the device's name.
                        : qsTr("%1 to %2").arg(view.payloadName).arg(view.outgoing.name)
        case "failed":
            return view.outgoingState === "cancelled"
                   //: A transfer was stopped by one of the two sides.
                   ? qsTr("Cancelled")
                   : view.engine.errorText({ code: view.outgoingTransfer ? view.outgoingTransfer.error : "" })
        }
        return ""
    }

    function heroLine() {
        if (!view.hasOutgoing || view.outgoing.key === "code") {
            return ""
        }
        switch (view.heroMode) {
        case "waiting":
        case "failed":
            return view.withSize(view.payloadName)
        case "progress":
            return view.payloadName
        }
        return ""
    }

    /// "3 photos · 8.2 MB", or the name alone where the size is not known.
    function withSize(name) {
        return view.payloadSize.length > 0 ? name + " · " + view.payloadSize : name
    }

    // ---- Choosing -------------------------------------------------------

    /// Opens the content picker, several files at a time.
    function pick() {
        if (view.payload.itemCount >= view.payload.maxFiles) {
            //: Send tab: the most files one send can carry are chosen already; %n is that many.
            view.banner.show(qsTr("A send can include up to %n file(s).", "", view.payload.maxFiles))
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
            pageStack.push(view.pickers.content)
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

        StatusHero {
            id: hero
            objectName: "sendHero"
            topInset: view.topInset
            pageHeight: view.viewHeight
            mode: view.heroMode
            glyph: !view.hasOutgoing ? (view.hasPayload ? view.payloadKind : "phone") : view.outgoing.glyph
            value: view.outgoingTransfer && view.outgoingTransfer.total > 0
                   ? view.outgoingTransfer.bytes / view.outgoingTransfer.total : 0
            running: view.current && view.foreground
            title: view.heroTitle()
            failed: view.heroMode === "failed"
            subtitle: view.heroSubtitle()
            line: view.heroLine()

            Button {
                objectName: "chooseFiles"
                visible: !view.hasPayload && !view.hasOutgoing
                //: Send tab with nothing chosen: opens the picker for pictures, videos, music, documents and other files.
                text: qsTr("Choose files")
                onClicked: view.pick()
            }
            Button {
                objectName: "addMore"
                visible: view.hasPayload && !view.hasOutgoing
                width: hero.pairWidth
                //: Send tab: chooses more files to send with those chosen.
                text: qsTr("Add files")
                onClicked: view.pick()
            }
            Button {
                objectName: "clearPayload"
                visible: view.hasPayload && !view.hasOutgoing
                width: hero.pairWidth
                //: Send tab: clears the chosen files.
                text: qsTr("Clear")
                onClicked: view.clearPayload()
            }
            Button {
                objectName: "cancelSend"
                visible: view.hasOutgoing && !view.outgoingEnded
                //: Stops a send or a receive under way.
                text: qsTr("Cancel")
                onClicked: view.cancelSend()
            }
            Button {
                objectName: "sendAnother"
                visible: view.heroMode === "done"
                width: hero.pairWidth
                //: Send tab, after a send: back to the devices, the same files still chosen.
                text: qsTr("Send to another")
                onClicked: view.dismiss()
            }
            Button {
                objectName: "sendDone"
                visible: view.heroMode === "done"
                width: hero.pairWidth
                //: Send tab, after a send: back to the start, nothing chosen.
                text: qsTr("Done")
                onClicked: view.finish()
            }
            Button {
                objectName: "sendRetry"
                visible: view.heroMode === "failed"
                width: hero.pairWidth
                //: Send tab, after a send failed: sends the same files the same way again.
                text: qsTr("Try again")
                onClicked: view.retry()
            }
            Button {
                objectName: "sendBack"
                visible: view.heroMode === "failed"
                width: hero.pairWidth
                //: Send tab, after a send failed: back to the devices, the same files still chosen.
                text: qsTr("Back")
                onClicked: view.dismiss()
            }
        }

        // ---- Who to: nearby ------------------------------------------

        Item {
            width: parent.width
            height: nearbyHeader.height
            visible: view.hasPayload && !view.hasOutgoing

            Row {
                objectName: "lookingRow"
                x: Theme.horizontalPageMargin
                anchors.verticalCenter: parent.verticalCenter
                spacing: Theme.paddingMedium
                visible: view.looking

                BusyIndicator {
                    anchors.verticalCenter: parent.verticalCenter
                    size: BusyIndicatorSize.ExtraSmall
                    running: parent.visible && view.current
                }
                Label {
                    objectName: "lookingLabel"
                    anchors.verticalCenter: parent.verticalCenter
                    text: view.devices.length > 0
                          //: Send tab, beside "Nearby": discovery is still running.
                          ? qsTr("Looking for more")
                          //: Send tab: discovery is running and has found nobody yet.
                          : qsTr("Looking for devices nearby…")
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
            model: view.hasPayload && !view.hasOutgoing ? view.devices : []
            delegate: IconListItem {
                id: deviceRow
                objectName: "deviceRow"
                glyph: view.deviceGlyph(modelData)
                title: modelData.name
                subtitle: view.deviceLine(modelData)
                menu: deviceMenu
                onClicked: view.choose(modelData)

                /// The device this row is, for its menu.
                readonly property var device: modelData

                Component {
                    id: deviceMenu
                    ContextMenu {
                        Repeater {
                            model: deviceRow.device.peers
                            delegate: MenuItem {
                                objectName: "sendWith"
                                //: A device's menu: send to it over this protocol; %1 is its name, e.g. "Quick Share".
                                text: qsTr("Send with %1").arg(view.engine.protocolName(modelData.protocol))
                                onClicked: view.choose(deviceRow.device, modelData.protocol)
                            }
                        }
                        MenuItem {
                            objectName: "aboutDevice"
                            //: A device's menu: the page with what Sukkula knows about it.
                            text: qsTr("About this device")
                            onClicked: view.openDevice(deviceRow.device)
                        }
                    }
                }
            }
        }

        Label {
            objectName: "nearbyHint"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            visible: view.hasPayload && !view.hasOutgoing
            topPadding: Theme.paddingLarge
            bottomPadding: Theme.paddingLarge
            text: !view.nearbyOn
                  //: Send tab: Quick Share, LocalSend and Bluetooth are all switched off.
                  ? qsTr("Sending nearby is switched off in Settings.")
                  //: Send tab, under the devices nearby: why one may be missing.
                  : qsTr("Devices must be on the same Wi-Fi and ready to receive.")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }

        // ---- Who to: far away -----------------------------------------

        SectionHeader {
            visible: view.hasPayload && !view.hasOutgoing && view.codeOn
            //: Send tab: the section for sending over the internet with a code.
            text: qsTr("Far away")
        }

        IconListItem {
            objectName: "sendWithCode"
            visible: view.hasPayload && !view.hasOutgoing && view.codeOn
            glyph: "qr"
            //: Send tab: sending over the internet with a code.
            title: qsTr("Send with a code")
            onClicked: view.openCode()
        }

        Item {
            width: 1
            height: Theme.paddingLarge
        }
    }

    /// The line under "Ready to send": who is about.
    function nearbyText() {
        var found = []
        for (var i = 0; i < view.devices.length; i++) {
            if (view.devices[i].found) {
                found.push(view.devices[i].name)
            }
        }
        if (found.length === 1) {
            //: Send tab, under "Ready to send": one device nearby; %1 is its name.
            return qsTr("%1 is nearby").arg(found[0])
        }
        if (found.length > 1) {
            //: Send tab, under "Ready to send": devices nearby; %1 is one's name, %n how many more.
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
