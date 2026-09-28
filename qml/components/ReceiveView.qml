// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The Receive tab (F-C1), at one glance, round the anchor (StatusHero):
 * whether this phone is ready and the name others see it by, the anchor
 * pulsing while it waits; what is coming in, the anchor filling as it
 * comes, with Cancel (F-C5), then a check and where it went, or a cross
 * and why, for a few seconds; receiving with a code from far away, by
 * scanning its QR code or typing it in (F-MW2, F-CR2); and what came
 * today. How each way of receiving is doing is in Settings, beside its
 * switch; a way that could not start is said here in a line that leads
 * there.
 *
 * Every offer still waits on the consent dialog (F-C2), which comes up
 * over whatever shows. Files that came together are one row ("3
 * photos"), and tapping it lists them. A file's kind is told by its name
 * alone: nothing received is ever opened or drawn (S2, S8).
 *
 * Sender and file names are the peers' own (after S1, S2) and are shown
 * only in IconListItem's, ProgressRow's, StatusHero's and this file's
 * plain-text labels.
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
    /// The height of the list this is in, which places the anchor.
    property real viewHeight: 0
    /// How long an ended transfer stays in the hero, in ms.
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
    /// Something is coming in, or has just ended.
    readonly property bool busy: (view.tick, view.countShown() > 0)
    /// Something came today.
    readonly property bool anyToday: (view.tick, view.countToday() > 0)
    readonly property bool pulsing: view.current && view.foreground && view.receiving && view.nearbyReady
                                    && !view.busy
    /// A way of receiving nearby is on and could not start.
    readonly property bool someFailed: view.receiving && view.nearbyEnabled && view.anyFailed
    /// The transfer the hero shows: the first coming in, or ended a
    /// moment ago; null while none is.
    readonly property var heroTransfer: (view.tick, view.findHeroTransfer())
    readonly property int heroId: view.heroTransfer ? view.heroTransfer.transferId : -1
    /// The anchor's mode.
    readonly property string heroMode: !view.heroTransfer ? (view.pulsing ? "ready" : "idle")
        : view.heroTransfer.state === "active" ? (view.heroTransfer.bytes > 0 ? "progress" : "waiting")
        : view.heroTransfer.state === "done" ? "done" : "failed"

    implicitHeight: column.height

    onHeroIdChanged: rate.reset()

    TransferRate {
        id: rate
        bytes: view.heroTransfer ? view.heroTransfer.bytes : 0
        total: view.heroTransfer ? view.heroTransfer.total : 0
        active: view.heroTransfer !== null && view.heroTransfer.state === "active"
    }

    function findHeroTransfer() {
        for (var i = 0; i < view.engine.transfers.count; i++) {
            var t = view.engine.transfers.get(i)
            if (view.shows(t.direction, t.state, t.transferId)) {
                return view.engine.transfer(t.transferId)
            }
        }
        return null
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

    function heroTitle() {
        var t = view.heroTransfer
        if (!t) {
            return view.hero()
        }
        switch (view.heroMode) {
        case "done":
            //: Receive tab: files or a text arrived.
            return qsTr("Received")
        case "failed":
            //: Receive tab: what was coming did not arrive; the line under it says why.
            return qsTr("Not received")
        }
        return t.peer
    }

    function heroSubtitle() {
        var t = view.heroTransfer
        if (!t) {
            //: Receive tab, over this phone's name as devices nearby list it.
            return view.nearbyEnabled ? qsTr("Others nearby see you as") : ""
        }
        switch (view.heroMode) {
        case "waiting":
            return view.receivingText(t)
        case "progress":
            return rate.sizeLine()
        case "done":
            return t.savedCount > 0
                   //: Receive tab: files arrived.
                   ? qsTr("Saved in Downloads › Sukkula")
                   //: Receive tab: a text arrived; it is on the History page.
                   : qsTr("Saved in History")
        }
        return t.state === "cancelled"
               //: A transfer was stopped by one of the two sides.
               ? qsTr("Cancelled")
               : view.engine.errorText({ code: t.error })
    }

    function heroLine() {
        var t = view.heroTransfer
        if (!t) {
            return view.nearbyEnabled ? view.engine.effectiveDeviceName : ""
        }
        var what = t.savedCount === 0 && t.fileCount === 0
                   //: Receive tab: a text message arrived.
                   ? qsTr("Text message")
                   : view.engine.bundleName(view.namesOf(t), Math.max(t.savedCount, t.fileCount))
        switch (view.heroMode) {
        case "waiting":
            return ""
        case "progress":
            return what
        }
        //: Receive tab, under "Received": what came from whom; %1 is what, e.g. "3 photos", %2 the sender's name.
        return qsTr("%1 from %2").arg(what).arg(t.peer)
    }

    Column {
        id: column
        width: parent.width

        Item {
            width: 1
            height: view.topInset
        }

        StatusHero {
            id: hero
            objectName: "receiveHero"
            topInset: view.topInset
            pageHeight: view.viewHeight
            mode: view.heroMode
            glyph: "phone"
            value: view.heroTransfer && view.heroTransfer.total > 0 ? view.heroTransfer.bytes / view.heroTransfer.total : 0
            running: view.current && view.foreground
            title: view.heroTitle()
            failed: view.heroMode === "failed"
            subtitle: view.heroSubtitle()
            line: view.heroLine()

            // A way that could not start: why is beside its switch.
            BackgroundItem {
                id: failedItem
                objectName: "receiveFailed"
                width: hero.width
                height: failedLabel.height + 2 * Theme.paddingMedium
                visible: view.someFailed && !view.heroTransfer
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

            Button {
                objectName: "cancelReceive"
                visible: view.heroTransfer !== null && view.heroTransfer.state === "active"
                //: Stops a send or a receive under way.
                text: qsTr("Cancel")
                onClicked: view.engine.cancel(view.heroId)
            }
        }

        Column {
            id: rest
            objectName: "receiveRest"
            width: parent.width

            // ---- Coming in -----------------------------------------------

            // More than one at a time: the others, under the hero's.
            SectionHeader {
                visible: (view.tick, view.countShown() > 1)
                //: Receive tab: the section of transfers coming in.
                text: qsTr("Receiving")
            }

            Repeater {
                model: view.engine.transfers
                delegate: ProgressRow {
                    objectName: "receiveProgress"
                    visible: (view.tick, view.shows(model.direction, model.state, model.transferId)
                              && model.transferId !== view.heroId)
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
