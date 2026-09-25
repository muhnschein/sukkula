// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One transfer in the main list (F-C5): who, what, how far, how it ended,
 * and a way to cancel it while it runs.
 *
 * `peer`, `files` and `saved` came from the other device (after S1/S2 in
 * the engine) and are shown only in this file's own labels, as plain text.
 */
ListItem {
    id: item

    property QtObject engine
    property var transferId: -1
    property string direction: ""
    property string protocol: ""
    property string peer: ""
    /// File names, one per line.
    property string files: ""
    property int fileCount: 0
    property real total: 0
    property real bytes: 0
    property string state: ""
    property string error: ""
    /// Names the files were saved under, one per line.
    property string saved: ""
    property int savedCount: 0

    signal cancelRequested()

    readonly property bool active: item.state === "active"
    readonly property bool incoming: item.direction === "incoming"
    readonly property string firstFile: item.files.split("\n")[0]

    contentHeight: column.height + 2 * Theme.paddingMedium
    menu: item.active ? cancelMenu : null

    Component {
        id: cancelMenu
        ContextMenu {
            MenuItem {
                //: Context menu: stop a transfer that is running.
                text: qsTr("Cancel")
                onClicked: item.cancelRequested()
            }
        }
    }

    Column {
        id: column
        x: Theme.horizontalPageMargin
        y: Theme.paddingMedium
        width: parent.width - 2 * Theme.horizontalPageMargin - (cancelButton.visible ? cancelButton.width : 0)
        spacing: Theme.paddingSmall

        Label {
            objectName: "transferPeer"
            width: parent.width
            // The arrow says which way, the protocol how; the name is the
            // peer's own, after S2.
            text: (item.incoming ? "↓ " : "↑ ") + (item.peer.length > 0
                  ? item.peer : item.engine.protocolName(item.protocol))
            textFormat: Text.PlainText
            truncationMode: TruncationMode.Fade
            color: item.highlighted ? Theme.highlightColor : Theme.primaryColor
        }

        Label {
            objectName: "transferFiles"
            width: parent.width
            text: item.fileCount === 0
                  //: A transfer that carries only a text, no files.
                  ? qsTr("Text")
                  : (item.fileCount === 1 ? item.firstFile
                  //: The first file's name, and how many others; %1 is the name.
                  : qsTr("%1 and %n more", "", item.fileCount - 1).arg(item.firstFile))
            textFormat: Text.PlainText
            elide: Text.ElideMiddle
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }

        ProgressLine {
            width: parent.width
            visible: item.active
            value: item.total > 0 ? item.bytes / item.total : 0
        }

        Label {
            objectName: "transferStatus"
            width: parent.width
            text: item.statusText()
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            maximumLineCount: 3
            elide: Text.ElideRight
            font.pixelSize: Theme.fontSizeExtraSmall
            color: item.state === "failed" ? Theme.errorColor : Theme.secondaryColor
        }
    }

    IconButton {
        id: cancelButton
        objectName: "cancelButton"
        anchors {
            right: parent.right
            rightMargin: Theme.paddingSmall
            verticalCenter: parent.verticalCenter
        }
        visible: item.active
        icon.source: "image://theme/icon-m-clear"
        onClicked: item.cancelRequested()
    }

    function statusText() {
        if (item.state === "active") {
            //: Progress of a transfer: %1 bytes so far, %2 bytes in all, both formatted.
            return qsTr("%1 of %2").arg(Format.formatFileSize(item.bytes))
                                   .arg(Format.formatFileSize(item.total))
        }
        if (item.state === "done") {
            if (!item.incoming) {
                //: A transfer from this phone arrived.
                return qsTr("Sent")
            }
            if (item.savedCount > 0) {
                //: Where received files went. %1 is the list of names they were saved under.
                return qsTr("Saved in Downloads/Sukkula: %1").arg(item.saved.split("\n").join(", "))
            }
            //: A transfer to this phone arrived.
            return qsTr("Received")
        }
        if (item.state === "cancelled") {
            //: A transfer was stopped by one of the two sides.
            return qsTr("Cancelled")
        }
        //: A transfer failed; %1 says why.
        return qsTr("Failed: %1").arg(item.engine.errorText({ code: item.error }))
    }
}
