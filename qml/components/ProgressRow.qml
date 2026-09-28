// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A transfer under way, in the list it was started from (F-C5): who, what
 * is going on, the percentage big, a cross to stop it, the progress line,
 * and how much of how much with roughly how long is left.
 *
 * `title` is the other device's name: shown only in this file's
 * plain-text labels (S2).
 */
Item {
    id: row

    property string title: ""
    /// What is going on, e.g. "Sending…"; says what went wrong once failed.
    property string status: ""
    /// "active", "done", "cancelled" or "failed".
    property string phase: "active"
    property real bytes: 0
    property real total: 0
    /// The line under the progress: the engine's words for an ending.
    property string detail: ""

    signal cancelClicked()

    readonly property bool active: row.phase === "active"
    readonly property real progress: row.phase === "done" ? 1
                                     : row.total > 0 ? Math.min(1, row.bytes / row.total) : 0

    TransferRate {
        id: rate
        bytes: row.bytes
        total: row.total
        active: row.active
    }

    function sizeLine() {
        return row.detail.length > 0 ? row.detail : rate.sizeLine()
    }

    width: parent ? parent.width : 0
    height: column.height + Theme.paddingMedium
    Column {
        id: column
        width: parent.width
        spacing: Theme.paddingSmall

        Item {
            width: parent.width
            height: Math.max(Theme.itemSizeMedium, percent.height)

            Column {
                anchors {
                    left: parent.left
                    leftMargin: Theme.horizontalPageMargin
                    right: percent.left
                    rightMargin: Theme.paddingMedium
                    verticalCenter: parent.verticalCenter
                }

                Label {
                    objectName: "progressTitle"
                    width: parent.width
                    text: row.title
                    textFormat: Text.PlainText
                    truncationMode: TruncationMode.Fade
                    color: Theme.highlightColor
                }
                Label {
                    objectName: "progressStatus"
                    width: parent.width
                    text: row.status
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    maximumLineCount: 3
                    font.pixelSize: Theme.fontSizeSmall
                    color: row.phase === "failed" ? Theme.errorColor : Theme.secondaryColor
                }
            }

            Label {
                id: percent
                objectName: "progressPercent"
                anchors {
                    right: cancel.visible ? cancel.left : parent.right
                    rightMargin: cancel.visible ? 0 : Theme.horizontalPageMargin
                    verticalCenter: parent.verticalCenter
                }
                visible: row.active || row.phase === "done"
                text: Math.floor(100 * row.progress) + "%"
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraLarge
                color: Theme.highlightColor
            }

            IconButton {
                id: cancel
                objectName: "cancelTransfer"
                anchors {
                    right: parent.right
                    rightMargin: Theme.horizontalPageMargin - Theme.paddingMedium
                    verticalCenter: parent.verticalCenter
                }
                visible: row.active
                icon.source: "image://theme/icon-m-clear"
                onClicked: row.cancelClicked()
            }
        }

        ProgressLine {
            objectName: "progressLine"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            visible: row.active || row.phase === "done"
            value: row.progress
        }

        Label {
            objectName: "progressDetail"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            visible: text.length > 0
            text: row.sizeLine()
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }
    }
}
