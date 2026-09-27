// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A protocol that goes through a server on the internet, as a tile over
 * the radar's cloud: Magic Wormhole (F-MW1, F-MW2) or croc. Sending, it
 * starts a send and then shows the code to read out until the receiver
 * comes; receiving, it opens the page to type a code on.
 *
 * The code's number part is the server's, so it is shown as plain text
 * like everything else.
 */
Item {
    id: tile

    /// The protocol's name; not translated.
    property string title: ""
    /// Under the title while there is no code.
    property string hint: ""
    /// "" before a send; then the code.
    property string code: ""
    /// Waiting for the code.
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
            objectName: "tileTitle"
            width: parent.width
            text: tile.title
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: tile.hasCode || tile.starting ? Theme.fontSizeTiny : Theme.fontSizeSmall
            color: tile.hasCode || tile.starting ? Theme.secondaryHighlightColor : tile.ink
        }

        Label {
            objectName: "tileHint"
            width: parent.width
            visible: !tile.hasCode && !tile.starting && text.length > 0
            text: tile.hint
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeTiny
            color: Theme.secondaryColor
        }

        BusyIndicator {
            anchors.horizontalCenter: parent.horizontalCenter
            size: BusyIndicatorSize.Small
            running: tile.starting && !tile.hasCode
            visible: running
        }

        Label {
            objectName: "tileCode"
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
            //: Under a code on the send screen: tapping shows it big, with a QR code.
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
