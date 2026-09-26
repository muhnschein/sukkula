// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One peer on the send radar: a round avatar, the protocol's badge, the
 * peer's name under it and, while a transfer to it runs, the avatar
 * filling up from below.
 *
 * `name` is the peer's own (after S2 in the engine) and is shown only in
 * this file's label, as plain text.
 */
Item {
    id: bubble

    property string name: ""
    property string protocol: ""
    /// -1 for none; otherwise 0 to 1, drawn as the avatar filling up.
    property real progress: -1
    /// Drawn in the highlight colour: the peer a transfer goes to.
    property bool active: false
    property bool showName: true
    /// The avatar's diameter; the name hangs below it.
    property real size: Theme.itemSizeMedium

    signal clicked()

    readonly property real fill: bubble.progress < 0 ? 0
                                 : Math.max(0, Math.min(1, isFinite(bubble.progress) ? bubble.progress : 0))
    readonly property color ink: bubble.active || area.pressed ? Theme.highlightColor : Theme.primaryColor

    width: bubble.size
    height: bubble.size

    Rectangle {
        id: disc
        anchors.fill: parent
        radius: width / 2
        color: Theme.rgba(Theme.highlightBackgroundColor, area.pressed ? 0.3 : 0.1)
        border.width: Math.max(2, Math.round(bubble.size / 30))
        border.color: bubble.ink
    }

    // The bottom slice of a full disc, as tall as the progress: clipping
    // a rectangle is all Qt 5.6 has without a mask.
    Item {
        objectName: "bubbleFill"
        anchors {
            left: parent.left
            right: parent.right
            bottom: parent.bottom
        }
        height: parent.height * bubble.fill
        clip: true
        visible: bubble.progress >= 0

        Rectangle {
            y: parent.height - bubble.height
            width: bubble.width
            height: bubble.height
            radius: width / 2
            color: Theme.highlightColor
            opacity: 0.8
        }
    }

    Glyph {
        anchors.centerIn: parent
        width: parent.width * 0.7
        height: width
        kind: "person"
        color: bubble.ink
    }

    ProtocolBadge {
        anchors {
            right: parent.right
            bottom: parent.bottom
            rightMargin: -width * 0.15
            bottomMargin: -width * 0.1
        }
        width: bubble.size * 0.4
        protocol: bubble.protocol
    }

    Label {
        objectName: "bubbleName"
        anchors {
            top: parent.bottom
            topMargin: Theme.paddingSmall / 2
            horizontalCenter: parent.horizontalCenter
        }
        width: bubble.size * 1.6
        visible: bubble.showName && text.length > 0
        text: bubble.name
        textFormat: Text.PlainText
        horizontalAlignment: Text.AlignHCenter
        truncationMode: TruncationMode.Fade
        font.pixelSize: Theme.fontSizeExtraSmall
        color: bubble.ink
    }

    MouseArea {
        id: area
        anchors.fill: parent
        onClicked: bubble.clicked()
    }
}
