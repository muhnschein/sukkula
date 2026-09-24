// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * A Magic Wormhole send (F-MW1): the code to read out, big, and the same
 * code as a QR code, then the transfer's progress.
 *
 * The code comes from the engine, but its number part is the mailbox
 * server's, so it is shown as plain text like everything else.
 */
Page {
    id: page
    objectName: "wormholeCodePage"

    property QtObject engine
    property var transferId: -1

    property string code: ""
    property var qr: null
    property var transfer: null

    readonly property bool active: page.transfer !== null && page.transfer.state === "active"

    function refresh() {
        var c = page.engine.wormholeCode(page.transferId)
        if (c) {
            page.code = c.code
            page.qr = c.qr
        }
        page.transfer = page.engine.transfer(page.transferId)
    }

    function statusText() {
        var t = page.transfer
        if (!t) {
            return ""
        }
        if (t.state === "active") {
            if (t.bytes > 0) {
                //: Progress of a transfer: %1 bytes so far, %2 bytes in all, both formatted.
                return qsTr("%1 of %2").arg(Format.formatFileSize(t.bytes)).arg(Format.formatFileSize(t.total))
            }
            //: Wormhole send: the code is shown, nobody has used it yet.
            return qsTr("Waiting for the receiver…")
        }
        if (t.state === "done") {
            //: A transfer from this phone arrived.
            return qsTr("Sent")
        }
        if (t.state === "cancelled") {
            //: A transfer was stopped by one of the two sides.
            return qsTr("Cancelled")
        }
        //: A transfer failed; %1 says why.
        return qsTr("Failed: %1").arg(page.engine.errorText({ code: t.error }))
    }

    Component.onCompleted: page.refresh()

    Connections {
        target: page.engine
        // Qt 5.6 handler syntax.
        onWormholeCodeArrived: {
            if (transferId === page.transferId) {
                page.refresh()
            }
        }
        onTransferUpdated: {
            if (transferId === page.transferId) {
                page.refresh()
            }
        }
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        PullDownMenu {
            visible: page.active
            MenuItem {
                //: Pulley menu: stop the running transfer.
                text: qsTr("Cancel transfer")
                onClicked: page.engine.cancel(page.transferId)
            }
        }

        Column {
            id: column
            width: parent.width
            spacing: Theme.paddingLarge

            PageHeader {
                title: "Magic Wormhole"
            }

            BusyIndicator {
                anchors.horizontalCenter: parent.horizontalCenter
                size: BusyIndicatorSize.Large
                running: page.code === "" && (page.transfer === null || page.active)
                visible: running
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.code === ""
                //: Wormhole send: waiting for the mailbox server to hand out a code.
                text: qsTr("Getting a code…")
                textFormat: Text.PlainText
                horizontalAlignment: Text.AlignHCenter
                color: Theme.secondaryHighlightColor
            }

            Label {
                objectName: "wormholeCode"
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

            Button {
                anchors.horizontalCenter: parent.horizontalCenter
                visible: page.code !== ""
                //: Copies the wormhole code to the clipboard.
                text: qsTr("Copy code")
                onClicked: Clipboard.text = page.code
            }

            QrCodeView {
                objectName: "wormholeQr"
                anchors.horizontalCenter: parent.horizontalCenter
                width: Math.min(page.width, page.height) - 2 * Theme.horizontalPageMargin
                height: width
                rows: page.qr ? page.qr.rows : []
                size: page.qr ? page.qr.size : 0
            }

            ProgressLine {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.active && page.transfer.total > 0 && page.transfer.bytes > 0
                value: page.transfer && page.transfer.total > 0 ? page.transfer.bytes / page.transfer.total : 0
            }

            Label {
                objectName: "wormholeStatus"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                text: page.statusText()
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                horizontalAlignment: Text.AlignHCenter
                color: page.transfer && page.transfer.state === "failed" ? Theme.errorColor
                                                                          : Theme.secondaryHighlightColor
            }
        }

        VerticalScrollDecorator {}
    }
}
