// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Sending with a code (F-MW1, F-CR1), round the anchor as the Send tab is
 * (StatusHero), and at the same place on the screen.
 *
 * While nobody has come for it, the QR code takes the anchor's place --
 * Magic Wormhole's `wormhole-transfer:` URI, croc's code on its own (spec
 * v0.6) -- the code itself is the title, and Copy and Share are under it.
 * Nobody has to know Magic Wormhole from croc: the page starts with the
 * one that takes what is chosen (Magic Wormhole takes one file, croc
 * several), and "Receiver's app" switches to the other for someone whose
 * app speaks only that one. Switching gives up the code shown and gets a
 * new one; so does leaving the page before anyone has come for it.
 *
 * Once the receiver has come, the page is the send: the anchor filling
 * with Cancel, then a check with "Send to another" and "Done", or a cross,
 * why, and "Try again" or "Back" -- the same steps as a send to a device.
 * Which server a send goes through is in Settings, not here.
 *
 * The send itself is the Send tab's (SendView): this page shows it and
 * asks it to switch, give up or end. The code comes from our engine, but
 * a wormhole code's number part is the mailbox server's, so it is shown
 * as plain text like everything else.
 */
Page {
    id: page
    objectName: "sendCodePage"

    property QtObject engine
    /// The Send tab's SendView, which holds the send.
    property Item view

    readonly property var outgoing: page.view ? page.view.outgoing : null
    readonly property bool mine: page.outgoing !== null && page.outgoing.key === "code"
    readonly property string protocol: page.mine ? page.outgoing.protocol : page.chosen
    /// The protocol asked for last, while its send is on its way.
    property string chosen: ""
    readonly property string code: page.mine ? page.view.outgoingCode : ""
    readonly property var qr: page.mine ? page.view.outgoingQr : null
    readonly property string phase: page.mine ? page.view.outgoingState : ""
    readonly property bool waiting: page.mine && page.view.codeWaiting
    /// The code is shown, and nobody has come for it yet.
    readonly property bool showingCode: page.waiting && page.code !== ""
    readonly property bool severalFiles: page.view !== null && page.view.payload.itemCount > 1

    Component.onCompleted: page.chosen = page.mine ? page.outgoing.protocol : ""

    // Left for good: a code nobody has come for is given up.
    Component.onDestruction: {
        if (page.view && page.waiting) {
            page.view.giveUpCode()
        }
    }

    /// "Receiver's app" was changed.
    function switchTo(protocol) {
        if (protocol === page.protocol && page.mine) {
            return
        }
        page.chosen = protocol
        if (!page.view.startCode(protocol)) {
            page.chosen = page.mine ? page.outgoing.protocol : ""
            appBox.currentIndex = page.protocol === "croc" ? 1 : 0
        }
    }

    function copy() {
        Clipboard.text = page.code
        //: Shown after the code was copied to the clipboard.
        banner.show(qsTr("Copied"), "info")
    }

    function share() {
        if (shareLoader.item) {
            shareLoader.item.code = page.code
            shareLoader.item.trigger()
        }
    }

    /// Back to the Send tab, the send going on or ended there.
    function leave() {
        if (pageStack.currentPage === page) {
            pageStack.pop()
        }
    }

    /// "Cancel": the send stops, and the page goes with it.
    function cancel() {
        page.view.cancelSend()
        page.leave()
    }

    /// "Send to another" and "Back": to the Send tab's devices, the same
    /// files still chosen.
    function back() {
        page.view.dismiss()
        page.leave()
    }

    /// "Done": to the Send tab's start, nothing chosen.
    function done() {
        page.view.finish()
        page.leave()
    }

    /// "Try again": a new code, the same way.
    function retry() {
        page.view.startCode(page.protocol)
    }

    Loader {
        id: shareLoader
        source: Qt.resolvedUrl("../share/ShareCode.qml")
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width

            PageHeader {
                id: header
                //: Page title: sending over the internet with a code.
                title: qsTr("Send with a code")
            }

            StatusHero {
                id: hero
                objectName: "codeHero"
                topInset: header.height
                pageHeight: page.height
                faceShown: page.showingCode
                mode: page.view ? page.view.heroMode : "waiting"
                glyph: "qr"
                value: page.view && page.view.outgoingTransfer && page.view.outgoingTransfer.total > 0
                       ? page.view.outgoingTransfer.bytes / page.view.outgoingTransfer.total : 0
                running: page.status === PageStatus.Active && Qt.application.state === Qt.ApplicationActive
                title: page.showingCode ? page.code : (page.view ? page.view.heroTitle() : "")
                titleWraps: page.showingCode
                failed: hero.mode === "failed"
                subtitle: page.showingCode
                          //: Send with a code, under the code: what the receiver does with it.
                          ? qsTr("The receiver scans it or types it in.")
                          : (page.view ? page.view.heroSubtitle() : "")

                Button {
                    objectName: "copyCode"
                    visible: page.showingCode
                    //: Copies the code to the clipboard.
                    text: qsTr("Copy")
                    onClicked: page.copy()
                }
                Button {
                    objectName: "shareCode"
                    visible: page.showingCode && shareLoader.status === Loader.Ready
                    //: Opens the system share sheet with the code.
                    text: qsTr("Share…")
                    onClicked: page.share()
                }
                Button {
                    objectName: "cancelSend"
                    visible: page.mine && page.phase === "active" && !page.waiting
                    //: Stops a send or a receive under way.
                    text: qsTr("Cancel")
                    onClicked: page.cancel()
                }
                Button {
                    objectName: "sendAnother"
                    visible: hero.mode === "done"
                    width: hero.pairWidth
                    //: Send tab, after a send: back to the devices, the same files still chosen.
                    text: qsTr("Send to another")
                    onClicked: page.back()
                }
                Button {
                    objectName: "sendDone"
                    visible: hero.mode === "done"
                    width: hero.pairWidth
                    //: Send tab, after a send: back to the start, nothing chosen.
                    text: qsTr("Done")
                    onClicked: page.done()
                }
                Button {
                    objectName: "sendRetry"
                    visible: hero.mode === "failed"
                    width: hero.pairWidth
                    //: Send tab, after a send failed: sends the same files the same way again.
                    text: qsTr("Try again")
                    onClicked: page.retry()
                }
                Button {
                    objectName: "sendBack"
                    visible: hero.mode === "failed"
                    width: hero.pairWidth
                    //: Send tab, after a send failed: back to the devices, the same files still chosen.
                    text: qsTr("Back")
                    onClicked: page.back()
                }
            }

            ComboBox {
                id: appBox
                objectName: "theirApp"
                width: parent.width
                visible: page.engine.protocolEnabled("wormhole") && page.engine.protocolEnabled("croc")
                         && (page.waiting || !page.mine)
                currentIndex: page.protocol === "croc" ? 1 : 0
                //: Send with a code: which app the other person has, which decides the code.
                label: qsTr("Receiver's app")
                description: page.protocol === "croc"
                             //: Send with a code: who can take a croc code.
                             ? qsTr("Works with Sukkula and croc.")
                             : page.severalFiles
                               //: Send with a code: Magic Wormhole cannot take several files.
                               ? qsTr("Magic Wormhole sends one file at a time.")
                               //: Send with a code: who can take a Magic Wormhole code.
                               : qsTr("Works with Sukkula, Warp and the wormhole command.")
                menu: ContextMenu {
                    MenuItem {
                        text: "Magic Wormhole"
                        enabled: !page.severalFiles
                        onClicked: page.switchTo("wormhole")
                    }
                    MenuItem {
                        text: "croc"
                        onClicked: page.switchTo("croc")
                    }
                }
            }
        }

        VerticalScrollDecorator {}
    }

    // The code as a QR code, where the anchor would be.
    QrCodeView {
        objectName: "sendQr"
        parent: hero.face
        anchors.fill: parent
        visible: page.showingCode && valid
        rows: page.qr ? page.qr.rows : []
        size: page.qr ? page.qr.size : 0
    }

    Banner {
        id: banner
        anchors.top: parent.top
        z: 1
    }
}
