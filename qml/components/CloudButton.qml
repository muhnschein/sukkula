// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The radar's cloud: the internet, which Magic Wormhole and croc go
 * through. Tapping it shows or hides their tiles; the label under it says
 * what a tap does.
 */
Item {
    id: cloud

    /// Under the cloud; empty for none.
    property string label: ""
    /// Drawn faint: a send or receive nearby is the picture now.
    property bool faint: false
    property real cloudHeight: cloud.width * 0.6
    property real labelHeight: Theme.fontSizeExtraSmall * 1.5
    /// The label may be wider than the cloud.
    property real labelWidth: cloud.width * 2

    signal clicked()

    readonly property color ink: area.pressed ? Theme.highlightColor
                                 : cloud.faint ? Theme.rgba(Theme.secondaryColor, 0.35) : Theme.primaryColor

    height: cloud.cloudHeight + cloud.labelHeight

    Glyph {
        objectName: "cloudGlyph"
        width: parent.width
        height: cloud.cloudHeight
        kind: "cloud"
        color: cloud.ink
        lineWidth: Math.max(2, Theme.paddingSmall / 2)
    }

    Label {
        objectName: "cloudLabel"
        y: cloud.cloudHeight
        anchors.horizontalCenter: parent.horizontalCenter
        width: cloud.labelWidth
        visible: text.length > 0
        text: cloud.label
        textFormat: Text.PlainText
        horizontalAlignment: Text.AlignHCenter
        truncationMode: TruncationMode.Fade
        font.pixelSize: Theme.fontSizeExtraSmall
        color: area.pressed ? Theme.highlightColor : Theme.secondaryHighlightColor
    }

    MouseArea {
        id: area
        anchors.fill: parent
        enabled: !cloud.faint
        onClicked: cloud.clicked()
    }
}
