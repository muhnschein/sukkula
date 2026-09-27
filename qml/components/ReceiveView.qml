// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The Receive tab (F-C1), at one glance: whether this phone is ready and
 * the name others see it by; what is coming in, with how far it has got
 * and a cross to stop it (F-C5); receiving with a code from far away
 * (F-MW2, F-CR2); what came today; and, folded away at the foot, how
 * others can reach this phone, one row per way.
 *
 * Every offer still waits on the consent dialog (F-C2), which comes up
 * over whatever shows. Files that came together are one row ("3
 * photos"), and tapping it lists them. A file's kind is told by its name
 * alone: nothing received is ever opened or drawn (S2, S8).
 *
 * Sender and file names are the peers' own (after S1, S2) and are shown
 * only in IconListItem's, ProgressRow's and this file's plain-text
 * labels.
 */
Item {
    id: view

    property QtObject engine
    /// Where to say what went wrong: a Banner.
    property Item banner
    /// This is the tab on screen.
    property bool current: true
    /// The app is in front.
    property bool foreground: true
    /// Room at the top for the tabs.
    property real topInset: 0
    /// How long an ended transfer stays under "Receiving", in ms.
    property int linger: 4000
    /// How others can reach this phone is unfolded.
    property bool reachOpen: false

    /// Ended incoming transfers still under "Receiving": transferId -> when.
    property var ended: ({})
    /// Goes up whenever a transfer changes: what reads them reads it.
    property int tick: 0

    readonly property bool receiving: view.engine.receiving
    readonly property var statuses: view.engine.protocolStatuses
    readonly property bool nearbyEnabled: view.engine.protocolEnabled("local_send")
                                          || view.engine.protocolEnabled("quick_share")
    readonly property bool nearbyReady: view.stateOf("local_send") === "ready"
                                        || view.stateOf("quick_share") === "ready"
    readonly property bool anyFailed: view.stateOf("local_send") === "failed"
                                      || view.stateOf("quick_share") === "failed"
    readonly property bool codeOn: view.engine.protocolEnabled("wormhole") || view.engine.protocolEnabled("croc")
    /// Something is under "Receiving".
    readonly property bool busy: (view.tick, view.countShown() > 0)
    /// Something came today.
    readonly property bool anyToday: (view.tick, view.countToday() > 0)
    readonly property bool pulsing: view.current && view.foreground && view.receiving && view.nearbyReady
                                    && !view.busy

    implicitHeight: column.height

    function stateOf(protocol) {
        for (var i = 0; i < view.statuses.length; i++) {
            if (view.statuses[i].protocol === protocol) {
                return view.statuses[i].state
            }
        }
        return ""
    }
    function statusOf(protocol) {
        for (var i = 0; i < view.statuses.length; i++) {
            if (view.statuses[i].protocol === protocol) {
                return view.statuses[i]
            }
        }
        return null
    }

    /// An incoming transfer is under "Receiving": running, or ended a
    /// moment ago.
    function shows(direction, state, transferId) {
        return direction === "incoming" && (state === "active" || view.ended[transferId] !== undefined)
    }
    /// An incoming transfer is under "Received today": done today, and no
    /// longer under "Receiving".
    function today(direction, state, transferId, endedAt) {
        if (direction !== "incoming" || state !== "done" || view.ended[transferId] !== undefined) {
            return false
        }
        var midnight = new Date()
        midnight.setHours(0, 0, 0, 0)
        return endedAt >= midnight.getTime()
    }
    function countShown() {
        var n = 0
        for (var i = 0; i < view.engine.transfers.count; i++) {
            var t = view.engine.transfers.get(i)
            if (view.shows(t.direction, t.state, t.transferId)) {
                n++
            }
        }
        return n
    }
    function countToday() {
        var n = 0
        for (var i = 0; i < view.engine.transfers.count; i++) {
            var t = view.engine.transfers.get(i)
            if (view.today(t.direction, t.state, t.transferId, t.endedAt)) {
                n++
            }
        }
        return n
    }

    Connections {
        target: view.engine.transfers
        // Qt 5.6 handler syntax.
        onCountChanged: view.tick++
    }
    Connections {
        target: view.engine
        onTransferUpdated: view.tick++
        onTransferEnded: {
            if (direction === "incoming") {
                view.ending(transferId)
            }
        }
    }

    /// An incoming transfer has ended: it stays under "Receiving" for
    /// `linger`, saying how.
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
        view.tick++
        prune.restart()
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
                view.tick++
            }
            if (left === 0) {
                prune.stop()
            }
        }
    }

    /// The camera, to read or type a code: which protocol, the code says.
    function scanCode() {
        pageStack.push(Qt.resolvedUrl("../pages/ScanPage.qml"), { engine: view.engine })
    }

    function openSettings() {
        pageStack.push(Qt.resolvedUrl("../pages/SettingsPage.qml"), { engine: view.engine })
    }

    /// A row under "Received today" was tapped.
    function openReceived(transferId) {
        var t = view.engine.transfer(transferId)
        if (!t) {
            return
        }
        if (t.savedCount === 0 && t.fileCount === 0) {
            for (var i = 0; i < view.engine.texts.count; i++) {
                var text = view.engine.texts.get(i)
                if (text.transferId === transferId) {
                    pageStack.push(Qt.resolvedUrl("../pages/TextPage.qml"), { from: text.from, text: text.text })
                    return
                }
            }
            return
        }
        if (view.namesOf(t).length > 1) {
            pageStack.push(Qt.resolvedUrl("../pages/ReceivedPage.qml"), { engine: view.engine, transferId: transferId })
        }
    }

    /// The names a transfer's files were saved under, or were offered
    /// under while it runs.
    function namesOf(t) {
        var list = t.savedCount > 0 ? t.saved : t.files
        return list.length > 0 ? list.split("\n") : []
    }

    function receivingText(t) {
        switch (t.state) {
        case "active":
            //: Receive tab: files are coming; %1 is what, e.g. "3 photos" or a file's name.
            return qsTr("Receiving %1…").arg(view.engine.bundleName(view.namesOf(t), t.fileCount))
        case "done":
            return t.savedCount > 0
                   //: Receive tab: files arrived.
                   ? qsTr("Saved in Downloads › Sukkula")
                   //: Receive tab: a text arrived; it is on the History page.
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

    /// "From Anna's Pixel 8 · 8.2 MB": who a row under "Received today"
    /// came from, and how much. The name is the peer's: the last .arg().
    function fromText(peer, total) {
        //: Receive tab: who files came from, then how much; %1 is the formatted size, %2 the sender's name.
        return qsTr("From %2 · %1").arg(Format.formatFileSize(total)).arg(peer)
    }

    function hero() {
        if (!view.receiving) {
            //: Receive tab: receiving was asked for and is not on yet.
            return qsTr("Switching on…")
        }
        if (!view.nearbyEnabled) {
            //: Receive tab: Quick Share and LocalSend are switched off; only codes can be received.
            return view.codeOn ? qsTr("Ready for codes") : qsTr("Receiving is switched off in Settings.")
        }
        if (view.nearbyReady) {
            //: Receive tab: this phone can be found and sent to.
            return qsTr("Ready to receive")
        }
        if (view.anyFailed) {
            //: Receive tab: no protocol could start.
            return qsTr("Nobody nearby can see this phone")
        }
        //: Receive tab: the protocols are starting.
        return qsTr("Starting…")
    }

    /// How others can reach this phone: one row per way, [{name, title,
    /// line, state, failed}].
    function reachRows() {
        var rows = []
        var s = view.engine.settings
        if (view.engine.hasProtocol("quick_share")) {
            var qs = s.quickshare || {}
            rows.push(view.reachRow("quick_share",
                //: Receive tab, how others can reach this phone: Quick Share's row.
                qsTr("Android phones nearby"),
                qs.visibility === "hidden"
                    //: Receive tab, Quick Share's row: nobody can find this phone.
                    ? qsTr("Quick Share, hidden")
                    //: Receive tab, Quick Share's row: anyone nearby can find this phone.
                    : qsTr("Quick Share, visible to everyone")))
        }
        if (view.engine.hasProtocol("local_send")) {
            var ls = s.localsend || {}
            rows.push(view.reachRow("local_send",
                //: Receive tab, how others can reach this phone: LocalSend's row.
                qsTr("Computers and other phones nearby"),
                typeof ls.pin === "string" && ls.pin.length > 0
                    //: Receive tab, LocalSend's row: senders must type a PIN.
                    ? qsTr("LocalSend, with a PIN")
                    //: Receive tab, LocalSend's row: no PIN is asked for.
                    : qsTr("LocalSend, no PIN")))
        }
        var codes = []
        if (view.engine.protocolEnabled("wormhole")) {
            codes.push("Magic Wormhole")
        }
        if (view.engine.protocolEnabled("croc")) {
            codes.push("croc")
        }
        if (view.engine.hasProtocol("wormhole") || view.engine.hasProtocol("croc")) {
            rows.push({
                name: "code",
                //: Receive tab, how others can reach this phone: receiving with a code.
                title: qsTr("Anyone with a code"),
                line: codes.length === 0 ? view.engine.stateText("off")
                      : codes.length === 1 ? view.engine.stateText("ready") + " · " + codes[0]
                      //: Two protocols' names, e.g. "Magic Wormhole and croc".
                      : view.engine.stateText("ready") + " · " + qsTr("%1 and %2").arg(codes[0]).arg(codes[1]),
                state: codes.length > 0 ? "ready" : "off",
                failed: false
            })
        }
        if (view.engine.hasProtocol("bluetooth")) {
            rows.push({
                name: "bluetooth",
                title: "Bluetooth",
                //: Receive tab, Bluetooth's row: Sukkula does not receive over Bluetooth, the phone does.
                line: qsTr("In the phone's own Bluetooth settings"),
                state: "off",
                failed: false
            })
        }
        return rows
    }

    function reachRow(protocol, title, detail) {
        var status = view.statusOf(protocol)
        var state = !view.engine.protocolEnabled(protocol) ? "off"
                    : status && view.receiving ? status.state : "off"
        var line = view.engine.stateText(state) + " · " + detail
        if (state === "failed") {
            line = qsTr("%1 could not start: %2").arg(view.engine.protocolName(protocol))
                                                 .arg(view.engine.errorText({ code: status.errorCode }))
        }
        return { name: protocol, title: title, line: line, state: state, failed: state === "failed" }
    }

    Column {
        id: column
        width: parent.width

        Item {
            width: 1
            height: view.topInset
        }

        // ---- Ready -----------------------------------------------------

        Column {
            objectName: "receiveHero"
            width: parent.width
            visible: !view.busy
            spacing: Theme.paddingSmall

            Item {
                id: rings
                anchors.horizontalCenter: parent.horizontalCenter
                width: Theme.itemSizeExtraLarge * 1.6
                height: width

                Repeater {
                    model: 3
                    delegate: Rectangle {
                        id: ring
                        anchors.centerIn: parent
                        width: rings.width
                        height: width
                        radius: width / 2
                        color: "transparent"
                        border.width: Math.max(1, Math.round(Theme.paddingSmall / 3))
                        border.color: Theme.highlightColor
                        opacity: 0
                        scale: 0.4

                        SequentialAnimation {
                            running: view.pulsing
                            loops: Animation.Infinite
                            onRunningChanged: {
                                if (!running) {
                                    ring.opacity = 0
                                    ring.scale = 0.4
                                }
                            }
                            PauseAnimation { duration: index * 700 }
                            ParallelAnimation {
                                NumberAnimation { target: ring; property: "scale"; from: 0.4; to: 1; duration: 2100 }
                                NumberAnimation { target: ring; property: "opacity"; from: 0.6; to: 0; duration: 2100 }
                            }
                            PauseAnimation { duration: (2 - index) * 700 }
                        }
                    }
                }
                Rectangle {
                    anchors.centerIn: parent
                    width: parent.width * 0.4
                    height: width
                    radius: width / 2
                    color: Theme.rgba(Theme.highlightBackgroundColor, 0.2)
                }
                Glyph {
                    anchors.centerIn: parent
                    width: parent.width * 0.3
                    height: width
                    kind: "phone"
                    color: Theme.highlightColor
                }
            }

            Label {
                objectName: "receiveState"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                horizontalAlignment: Text.AlignHCenter
                text: view.hero()
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeExtraLarge
                color: Theme.highlightColor
            }
            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: view.nearbyEnabled
                horizontalAlignment: Text.AlignHCenter
                //: Receive tab, over this phone's name.
                text: qsTr("Nearby, this phone shows up as")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
            Label {
                objectName: "deviceNameLabel"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: view.nearbyEnabled
                horizontalAlignment: Text.AlignHCenter
                text: view.engine.effectiveDeviceName
                textFormat: Text.PlainText
                truncationMode: TruncationMode.Fade
            }
        }

        // ---- Coming in ---------------------------------------------------

        SectionHeader {
            visible: view.busy
            //: Receive tab: the section of transfers coming in.
            text: qsTr("Receiving")
        }

        Repeater {
            model: view.engine.transfers
            delegate: ProgressRow {
                objectName: "receiveProgress"
                visible: (view.tick, view.shows(model.direction, model.state, model.transferId))
                title: model.peer
                status: (view.tick, view.receivingText(view.engine.transfer(model.transferId) || {}))
                phase: model.state
                bytes: model.bytes
                total: model.total
                onCancelClicked: view.engine.cancel(model.transferId)
            }
        }

        // ---- From far away ------------------------------------------------

        SectionHeader {
            visible: view.codeOn && view.engine.running
            //: Receive tab: the section for receiving over the internet with a code.
            text: qsTr("From far away")
        }

        IconListItem {
            objectName: "scanCode"
            visible: view.codeOn && view.engine.running
            glyph: "qr"
            //: Receive tab: opens the camera to read the sender's QR code.
            title: qsTr("Scan a code")
            //: Receive tab: under "Scan a code".
            subtitle: qsTr("Or type the one you were given")
            onClicked: view.scanCode()
        }

        // ---- Came today -------------------------------------------------

        SectionHeader {
            visible: view.anyToday
            //: Receive tab: the section of what arrived today.
            text: qsTr("Received today")
        }

        Repeater {
            model: view.engine.transfers
            delegate: IconListItem {
                id: receivedRow
                readonly property var names: model.savedCount > 0 ? model.saved.split("\n")
                                             : model.files.length > 0 ? model.files.split("\n") : []
                readonly property bool isText: model.savedCount === 0 && model.fileCount === 0
                objectName: "receivedRow"
                visible: (view.tick, view.today(model.direction, model.state, model.transferId, model.endedAt))
                glyph: receivedRow.isText ? "text"
                       : view.engine.commonKind(receivedRow.names, Math.max(model.savedCount, receivedRow.names.length))
                title: receivedRow.isText
                       //: Receive tab: a text message arrived.
                       ? qsTr("Text message")
                       : view.engine.bundleName(receivedRow.names, Math.max(model.savedCount, receivedRow.names.length))
                subtitle: view.fromText(model.peer, model.total)
                onClicked: view.openReceived(model.transferId)
            }
        }

        Row {
            objectName: "savedIn"
            x: Theme.horizontalPageMargin
            visible: view.anyToday
            spacing: Theme.paddingMedium
            topPadding: Theme.paddingSmall
            bottomPadding: Theme.paddingSmall

            Glyph {
                anchors.verticalCenter: parent.verticalCenter
                width: Theme.iconSizeSmall
                height: width
                kind: "folder"
                color: Theme.secondaryColor
            }
            Label {
                anchors.verticalCenter: parent.verticalCenter
                //: Receive tab: where received files are.
                text: qsTr("Saved in Downloads › Sukkula")
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
        }

        // ---- How others can reach this phone ------------------------------

        BackgroundItem {
            id: reachToggle
            objectName: "reachToggle"
            width: parent.width
            height: Theme.itemSizeSmall
            onClicked: view.reachOpen = !view.reachOpen

            Label {
                anchors {
                    right: arrow.left
                    rightMargin: Theme.paddingSmall
                    verticalCenter: parent.verticalCenter
                }
                //: Receive tab, at the foot: unfolds one row per way others can send to this phone.
                text: qsTr("How others can reach this phone")
                textFormat: Text.PlainText
                color: Theme.highlightColor
            }
            Image {
                id: arrow
                anchors {
                    right: parent.right
                    rightMargin: Theme.horizontalPageMargin
                    verticalCenter: parent.verticalCenter
                }
                source: "image://theme/icon-m-down?" + Theme.highlightColor
                rotation: view.reachOpen ? 180 : 0
            }
        }

        Repeater {
            model: view.reachOpen ? view.reachRows() : []
            delegate: ListItem {
                id: reachItem
                objectName: "reachRow"
                contentHeight: Theme.itemSizeMedium
                onClicked: view.openSettings()

                Item {
                    id: dotSlot
                    x: Theme.horizontalPageMargin
                    width: Theme.iconSizeMedium
                    height: parent.height

                    Rectangle {
                        objectName: "reachDot"
                        anchors.centerIn: parent
                        width: Theme.paddingMedium
                        height: width
                        radius: width / 2
                        color: modelData.failed ? Theme.errorColor
                               : modelData.state === "ready" ? Theme.highlightColor : "transparent"
                        border.width: modelData.state === "ready" || modelData.failed ? 0 : 1
                        border.color: Theme.secondaryColor
                    }
                }
                Column {
                    anchors {
                        left: dotSlot.right
                        leftMargin: Theme.paddingLarge
                        right: parent.right
                        rightMargin: Theme.horizontalPageMargin
                        verticalCenter: parent.verticalCenter
                    }
                    Label {
                        width: parent.width
                        text: modelData.title
                        textFormat: Text.PlainText
                        truncationMode: TruncationMode.Fade
                        color: reachItem.highlighted ? Theme.highlightColor : Theme.primaryColor
                    }
                    Label {
                        objectName: "reachLine"
                        width: parent.width
                        text: modelData.line
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        font.pixelSize: Theme.fontSizeSmall
                        color: modelData.failed ? Theme.errorColor : Theme.secondaryColor
                    }
                }
            }
        }

        Label {
            x: Theme.horizontalPageMargin + Theme.iconSizeMedium + Theme.paddingLarge
            width: parent.width - x - Theme.horizontalPageMargin
            visible: view.reachOpen
            topPadding: Theme.paddingSmall
            //: Receive tab, under how others can reach this phone.
            text: qsTr("Tap one to change it in Settings.")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }

        Item {
            width: 1
            height: Theme.paddingLarge
        }
    }
}
