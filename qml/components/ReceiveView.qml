// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The Receive tab (F-C1), at one glance: whether this phone is ready and
 * the name others see it by, under a radar that pulses while it waits;
 * what is coming in, with how far it has got and a cross to stop it
 * (F-C5); receiving with a code from far away, by scanning its QR code or
 * typing it in (F-MW2, F-CR2); and what came today. How each way of
 * receiving is doing is in Settings, beside its switch; a way that could
 * not start is said here in a line that leads there.
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
    /// The height of the list this is in: the radar takes what the rest
    /// leaves of it.
    property real viewHeight: 0
    /// How long an ended transfer stays under "Receiving", in ms.
    property int linger: 4000

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
    /// A way of receiving nearby is on and could not start.
    readonly property bool someFailed: view.receiving && view.nearbyEnabled && view.anyFailed
    /// The radar as the canvas drew it, round the theme's phone icon: the
    /// disc 1.9 times the icon, the outer ring 1 / 0.432 times the disc.
    readonly property real radarBase: Theme.iconSizeMedium * 1.9 / 0.432
    /// The room under the tabs that the words under the radar and
    /// everything below it leave.
    readonly property real radarRoom: view.viewHeight - view.topInset - heroWords.height - rest.height
                                      - 3 * Theme.paddingLarge
    /// The outer ring: the canvas's size, and up to a third more where
    /// there is room, with the pulses (to 1.15 times it) kept inside the
    /// page's margins.
    readonly property real radarSize: Math.min((view.width - 2 * Theme.horizontalPageMargin) / 1.15,
                                               Math.max(view.radarBase,
                                                        Math.min(view.radarBase * 1.35, view.radarRoom / 1.15)))

    implicitHeight: column.height

    /// CSS's ease-out, cubic-bezier(0, 0, 0.58, 1), which the canvas's
    /// pulses use: x(s) = 1.74 s^2 - 0.74 s^3 is found for `t` by
    /// halving, and y(s) = 3 s^2 - 2 s^3 is the eased value.
    function easeOut(t) {
        var lo = 0
        var hi = 1
        for (var i = 0; i < 16; i++) {
            var s = (lo + hi) / 2
            if (1.74 * s * s - 0.74 * s * s * s < t) {
                lo = s
            } else {
                hi = s
            }
        }
        var m = (lo + hi) / 2
        return 3 * m * m - 2 * m * m * m
    }

    function stateOf(protocol) {
        for (var i = 0; i < view.statuses.length; i++) {
            if (view.statuses[i].protocol === protocol) {
                return view.statuses[i].state
            }
        }
        return ""
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

    /// The camera, to read a code off the sender's screen: which protocol,
    /// the QR code says.
    function scanCode() {
        pageStack.push(Qt.resolvedUrl("../pages/ScanPage.qml"), { engine: view.engine })
    }

    /// A code typed in or pasted: which protocol, the engine tells.
    function typeCode() {
        pageStack.push(Qt.resolvedUrl("../pages/TypeCodePage.qml"), { engine: view.engine })
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
                   : qsTr("Saved in History")
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
            return view.codeOn ? qsTr("Ready to receive codes") : qsTr("Receiving is switched off in Settings.")
        }
        if (view.nearbyReady) {
            //: Receive tab: this phone can be found and sent to.
            return qsTr("Ready to receive")
        }
        if (view.anyFailed) {
            //: Receive tab: no way of receiving nearby could start.
            return qsTr("Others nearby cannot see you")
        }
        //: Receive tab: the protocols are starting.
        return qsTr("Starting…")
    }

    Column {
        id: column
        width: parent.width

        Item {
            width: 1
            height: view.topInset
        }

        // ---- Ready -----------------------------------------------------

        Item {
            id: hero
            objectName: "receiveHero"
            width: parent.width
            visible: !view.busy
            height: Math.max(heroColumn.height + 2 * Theme.paddingLarge,
                             view.viewHeight - view.topInset - rest.height)

            Column {
                id: heroColumn
                width: parent.width
                y: Math.max(Theme.paddingLarge, (hero.height - heroColumn.height) / 2)
                spacing: Theme.paddingLarge

                // As the canvas drew it: two faint rings, a disc round the
                // phone, and three pulses going out from the disc to past
                // the outer ring, fading as they go, one every second while
                // this phone waits to be sent to.
                Item {
                    id: radar
                    objectName: "radar"
                    /// How far round the 3 s cycle the first pulse is.
                    property real phase: 0
                    /// Line widths, the canvas's in its 176 px.
                    readonly property real line: Math.max(1, radar.width / 176)
                    anchors.horizontalCenter: parent.horizontalCenter
                    width: view.radarSize
                    height: width
                    opacity: view.nearbyReady ? 1 : 0.4

                    NumberAnimation {
                        target: radar
                        property: "phase"
                        from: 0
                        to: 1
                        duration: 3000
                        loops: Animation.Infinite
                        running: view.pulsing
                    }

                    Repeater {
                        model: [1, 0.705]
                        delegate: Rectangle {
                            anchors.centerIn: parent
                            width: radar.width * modelData
                            height: width
                            radius: width / 2
                            color: "transparent"
                            border.width: radar.line
                            border.color: Theme.rgba(Theme.highlightColor, 0.24)
                        }
                    }

                    Repeater {
                        model: 3
                        delegate: Rectangle {
                            id: pulse
                            objectName: "pulse"
                            /// This pulse's way out, 0 to 1, eased.
                            readonly property real progress: view.easeOut((radar.phase + index / 3) % 1)
                            anchors.centerIn: parent
                            width: radar.width
                            height: width
                            radius: width / 2
                            visible: view.pulsing
                            color: "transparent"
                            border.width: 1.5 * radar.line
                            border.color: Theme.highlightColor
                            scale: 0.45 + 0.7 * pulse.progress
                            opacity: 0.9 * (1 - pulse.progress)
                        }
                    }

                    Rectangle {
                        objectName: "radarCore"
                        anchors.centerIn: parent
                        width: radar.width * 0.432
                        height: width
                        radius: width / 2
                        color: Theme.rgba(Theme.highlightColor, 0.1)
                        border.width: 2 * radar.line
                        border.color: Theme.highlightColor
                    }
                    Glyph {
                        objectName: "radarGlyph"
                        anchors.centerIn: parent
                        kind: "phone"
                        color: Theme.highlightColor
                    }
                }

                Column {
                    id: heroWords
                    objectName: "heroWords"
                    width: parent.width
                    spacing: Theme.paddingSmall

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
                        objectName: "seenAs"
                        x: Theme.horizontalPageMargin
                        width: parent.width - 2 * Theme.horizontalPageMargin
                        visible: view.nearbyEnabled
                        horizontalAlignment: Text.AlignHCenter
                        //: Receive tab, over this phone's name as devices nearby list it.
                        text: qsTr("Others nearby see you as")
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

                    // A way that could not start: why is beside its switch.
                    BackgroundItem {
                        id: failedItem
                        objectName: "receiveFailed"
                        width: parent.width
                        height: failedLabel.height + 2 * Theme.paddingMedium
                        visible: view.someFailed
                        onClicked: view.openSettings()

                        Label {
                            id: failedLabel
                            objectName: "receiveFailedLine"
                            x: Theme.horizontalPageMargin
                            width: parent.width - 2 * Theme.horizontalPageMargin
                            anchors.verticalCenter: parent.verticalCenter
                            horizontalAlignment: Text.AlignHCenter
                            text: view.nearbyReady
                                  //: Receive tab, under "Ready to receive": one way of receiving nearby could not start; tapping opens Settings, which says why.
                                  ? qsTr("Some devices nearby cannot see you. Tap to see why.")
                                  //: Receive tab, under "Others nearby cannot see you": tapping opens Settings, which says why.
                                  : qsTr("Tap to see why.")
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            font.pixelSize: Theme.fontSizeSmall
                            color: failedItem.highlighted ? Theme.highlightColor : Theme.errorColor
                        }
                    }
                }
            }
        }

        Column {
            id: rest
            objectName: "receiveRest"
            width: parent.width

            // ---- Coming in -----------------------------------------------

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

            // ---- From far away --------------------------------------------

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
                title: qsTr("Scan a QR code")
                onClicked: view.scanCode()
            }

            IconListItem {
                objectName: "typeCode"
                visible: view.codeOn && view.engine.running
                glyph: "keyboard"
                //: Receive tab: opens the page to type or paste a code.
                title: qsTr("Type in a code")
                onClicked: view.typeCode()
            }

            // ---- Came today -----------------------------------------------

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
                           : view.engine.commonKind(receivedRow.names,
                                                    Math.max(model.savedCount, receivedRow.names.length))
                    title: receivedRow.isText
                           //: Receive tab: a text message arrived.
                           ? qsTr("Text message")
                           : view.engine.bundleName(receivedRow.names,
                                                    Math.max(model.savedCount, receivedRow.names.length))
                    subtitle: view.fromText(model.peer, model.total)
                    onClicked: view.openReceived(model.transferId)
                }
            }

            Label {
                objectName: "savedIn"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: view.anyToday
                topPadding: Theme.paddingSmall
                bottomPadding: Theme.paddingSmall
                //: Receive tab: where received files are.
                text: qsTr("Saved in Downloads › Sukkula")
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
}
