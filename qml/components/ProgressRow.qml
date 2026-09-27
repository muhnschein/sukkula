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

    /// When the bytes started to come, and how many there were then, for
    /// how long is left.
    property real _since: 0
    property real _from: 0
    property real _now: 0

    onBytesChanged: {
        row._now = Date.now()
        if (row.bytes > 0 && row._since === 0) {
            row._since = row._now
            row._from = row.bytes
        }
    }

    /// Seconds left at the rate so far, or -1 while that says nothing yet.
    function secondsLeft() {
        var took = (row._now - row._since) / 1000
        var done = row.bytes - row._from
        if (!row.active || row._since === 0 || took < 2 || done <= 0 || row.total <= row.bytes) {
            return -1
        }
        return Math.ceil((row.total - row.bytes) / (done / took))
    }

    function sizeLine() {
        if (row.detail.length > 0) {
            return row.detail
        }
        if (!row.active || row.total <= 0) {
            return ""
        }
        //: Progress: %1 bytes so far, %2 bytes in all, both formatted.
        var line = qsTr("%1 of %2").arg(Format.formatFileSize(row.bytes)).arg(Format.formatFileSize(row.total))
        var left = row.secondsLeft()
        if (left < 0) {
            return line
        }
        //: Progress: how long is left, after how much is done; %1 is that, e.g. "4.3 MB of 8.2 MB".
        return left < 90 ? qsTr("%1 · about %n second(s) left", "", left).arg(line)
                         //: Progress: how long is left, after how much is done; %1 is that.
                         : qsTr("%1 · about %n minute(s) left", "", Math.round(left / 60)).arg(line)
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
