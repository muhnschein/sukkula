// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Magic Wormhole on the send radar (F-MW1): a tile to start a send with,
 * which then shows the code to read out until the receiver comes.
 *
 * The code's number part is the mailbox server's, so it is shown as plain
 * text like everything else.
 */
Item {
    id: tile

    /// "" before a send; "starting" until the code comes; then the code.
    property string code: ""
    property bool starting: false

    signal clicked()

    readonly property bool hasCode: tile.code.length > 0
    readonly property color ink: area.pressed || tile.starting || tile.hasCode ? Theme.highlightColor
                                                                                : Theme.primaryColor

    Rectangle {
        anchors.fill: parent
        radius: Theme.paddingSmall
        color: Theme.rgba(Theme.highlightBackgroundColor, area.pressed ? 0.3 : 0.1)
        border.width: 2
        border.color: tile.ink
    }

    Column {
        anchors {
            left: parent.left
            right: parent.right
            verticalCenter: parent.verticalCenter
            margins: Theme.paddingMedium
        }
        spacing: Theme.paddingSmall / 2

        Label {
            objectName: "wormholeTileTitle"
            width: parent.width
            text: "Magic Wormhole"
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: tile.hasCode || tile.starting ? Theme.fontSizeTiny : Theme.fontSizeSmall
            color: tile.hasCode || tile.starting ? Theme.secondaryHighlightColor : tile.ink
        }

        BusyIndicator {
            anchors.horizontalCenter: parent.horizontalCenter
            size: BusyIndicatorSize.Small
            running: tile.starting && !tile.hasCode
            visible: running
        }

        Label {
            objectName: "wormholeTileCode"
            width: parent.width
            visible: tile.hasCode
            text: tile.code
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.highlightColor
        }

        Label {
            width: parent.width
            visible: tile.hasCode
            //: Under a wormhole code on the send screen: tapping shows it big, with a QR code.
            text: qsTr("Tap for the QR code")
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeTiny
            color: Theme.secondaryHighlightColor
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        onClicked: tile.clicked()
    }
}
