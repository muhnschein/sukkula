// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Sending with a code (F-MW1, F-CR1): the code big, the same code as a QR
 * code -- Magic Wormhole's `wormhole-transfer:` URI, croc's code on its
 * own (spec v0.6) -- Copy and Share, then how far the send has got.
 *
 * Nobody has to know Magic Wormhole from croc: the page starts with the
 * one that takes what is chosen (Magic Wormhole takes one file, croc
 * several), and "Their app" switches to the other for someone whose app
 * speaks only that one. Switching gives up the code shown and gets a new
 * one; so does leaving the page before anyone has come for it.
 *
 * The send itself is the Send tab's (SendView): this page shows it and
 * asks it to switch or give up. The code comes from our engine, but a
 * wormhole code's number part is the mailbox server's, so it is shown as
 * plain text like everything else.
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
    readonly property var transfer: page.mine ? page.view.outgoingTransfer : null
    readonly property string phase: page.mine ? page.view.outgoingState : ""
    readonly property bool waiting: page.mine && page.view.codeWaiting
    readonly property bool severalFiles: page.view !== null && page.view.payload.itemCount > 1

    Component.onCompleted: page.chosen = page.mine ? page.outgoing.protocol : ""

    // Left for good: a code nobody has come for is given up.
    Component.onDestruction: {
        if (page.view && page.waiting) {
            page.view.giveUpCode()
        }
    }

    /// "Their app" was changed.
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

    function statusText() {
        var t = page.transfer
        switch (page.phase) {
        case "starting":
            //: Send with a code: waiting for the server to hand out a code.
            return qsTr("Getting a code…")
        case "active":
            if (!t || t.bytes <= 0) {
                return page.code === "" ? qsTr("Getting a code…")
                                        //: Send with a code: the code is shown, nobody has used it yet.
                                        : qsTr("Waiting for the receiver…")
            }
            //: Send with a code: the files are going.
            return qsTr("Sending…")
        case "done":
            //: A send arrived.
            return qsTr("Sent")
        case "cancelled":
            //: A send was stopped by one of the two sides.
            return qsTr("Cancelled")
        case "failed":
            //: A send failed; %1 says why.
            return qsTr("Failed: %1").arg(page.engine.errorText({ code: t ? t.error : "" }))
        }
        return ""
    }

    /// Where the send goes through, and where to change that.
    function serverText() {
        var s = page.engine.settings
        if (page.protocol === "croc") {
            return s.croc && typeof s.croc.relay === "string" && s.croc.relay.length > 0
                   //: Send with a code, at the foot: croc goes through the user's own relay.
                   ? qsTr("Uses the croc relay set in Settings.")
                   //: Send with a code, at the foot: croc goes through croc's public relay.
                   : qsTr("Uses croc's public relay. You can set your own in Settings.")
        }
        return s.wormhole && typeof s.wormhole.mailbox_url === "string" && s.wormhole.mailbox_url.length > 0
               //: Send with a code, at the foot: Magic Wormhole goes through the user's own server.
               ? qsTr("Uses the Magic Wormhole server set in Settings.")
               //: Send with a code, at the foot: Magic Wormhole goes through its public server.
               : qsTr("Uses Magic Wormhole's public server. You can set your own in Settings.")
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
            spacing: Theme.paddingMedium

            PageHeader {
                //: Page title: sending over the internet with a code.
                title: qsTr("Send with a code")
            }

            // What is being sent: the user's own files, in a label of
            // our own all the same.
            Label {
                objectName: "codePayload"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                horizontalAlignment: Text.AlignRight
                text: {
                    var p = page.view.payload
                    var name = page.engine.bundleName(p.names(), p.itemCount)
                    return p.totalSize >= 0 ? name + " · " + Format.formatFileSize(p.totalSize) : name
                }
                textFormat: Text.PlainText
                truncationMode: TruncationMode.Fade
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: Send with a code: what to do with the code.
                text: qsTr("The receiver scans the QR code or types in the code.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
            }

            Label {
                objectName: "sendCode"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.code !== ""
                text: page.code
                textFormat: Text.PlainText
                wrapMode: Text.WrapAnywhere
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: Theme.fontSizeExtraLarge
                color: Theme.highlightColor
            }

            BusyIndicator {
                anchors.horizontalCenter: parent.horizontalCenter
                size: BusyIndicatorSize.Large
                running: page.code === "" && (page.phase === "starting" || page.phase === "active")
                visible: running
            }

            QrCodeView {
                objectName: "sendQr"
                anchors.horizontalCenter: parent.horizontalCenter
                width: Math.min(page.width, page.height) * 0.6
                height: width
                visible: page.waiting && valid
                rows: page.qr ? page.qr.rows : []
                size: page.qr ? page.qr.size : 0
            }

            Label {
                objectName: "codeStatus"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.waiting || page.phase === "starting"
                horizontalAlignment: Text.AlignHCenter
                text: page.statusText()
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }

            // Once the other side has come: how far, and a way to stop.
            ProgressRow {
                objectName: "codeProgress"
                visible: page.mine && !page.waiting && page.phase !== "starting"
                //: Send with a code: the row of the send once its receiver has come.
                title: qsTr("Sending")
                status: page.statusText()
                phase: page.phase === "" ? "active" : page.phase
                bytes: page.transfer ? page.transfer.bytes : 0
                total: page.transfer ? page.transfer.total : 0
                onCancelClicked: page.view.cancelSend()
            }

            Row {
                anchors.horizontalCenter: parent.horizontalCenter
                spacing: Theme.paddingLarge
                visible: page.code !== "" && page.waiting

                Button {
                    objectName: "copyCode"
                    //: Copies the code to the clipboard.
                    text: qsTr("Copy")
                    onClicked: page.copy()
                }
                Button {
                    objectName: "shareCode"
                    visible: shareLoader.status === Loader.Ready
                    //: Opens the system share sheet with the code.
                    text: qsTr("Share…")
                    onClicked: page.share()
                }
            }

            ComboBox {
                id: appBox
                objectName: "theirApp"
                width: parent.width
                visible: page.engine.protocolEnabled("wormhole") && page.engine.protocolEnabled("croc")
                enabled: page.waiting || !page.mine
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

            Label {
                objectName: "codeServer"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                text: page.serverText()
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
        }

        VerticalScrollDecorator {}
    }

    Banner {
        id: banner
        anchors.top: parent.top
        z: 1
    }
}
