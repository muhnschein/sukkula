// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A thin progress bar. Drawn here rather than with Silica's ProgressBar,
 * whose label is a Silica text item this app does not control: nothing
 * from a peer is ever handed to one (S2).
 */
Item {
    id: line

    /// 0 to 1; anything else is clamped.
    property real value: 0
    property color color: Theme.highlightColor

    implicitHeight: Math.max(2, Math.round(Theme.paddingSmall))

    Rectangle {
        anchors.fill: parent
        radius: height / 2
        color: Theme.rgba(line.color, 0.2)
    }
    Rectangle {
        height: parent.height
        radius: height / 2
        width: parent.width * Math.max(0, Math.min(1, isFinite(line.value) ? line.value : 0))
        color: line.color
    }
}
