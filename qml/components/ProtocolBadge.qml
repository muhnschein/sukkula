// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Which protocol reaches a peer, as a small disc on its avatar: the
 * theme's Bluetooth icon, and Sukkula's own marks for LocalSend and
 * Quick Share.
 */
Rectangle {
    id: badge

    property string protocol: ""

    width: Theme.iconSizeSmall
    height: width
    radius: width / 2
    color: Theme.highlightBackgroundColor
    visible: badge.protocol !== ""

    Image {
        id: themed
        anchors.centerIn: parent
        width: parent.width * 0.8
        height: width
        sourceSize.width: width
        sourceSize.height: height
        visible: badge.protocol === "bluetooth" && themed.status === Image.Ready
        source: badge.protocol === "bluetooth" ? "image://theme/icon-m-bluetooth?" + Theme.primaryColor : ""
    }

    Glyph {
        anchors.centerIn: parent
        width: parent.width * 0.8
        height: width
        visible: badge.protocol !== "" && !themed.visible
        kind: badge.protocol
        color: Theme.primaryColor
        lineWidth: Math.max(1.5, width / 9)
    }
}
