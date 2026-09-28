// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A QR code, as an icon: piirit's (icons/cover/qr.svg there), drawn here
 * on its 32-unit grid -- three finder patterns, each an outline round a
 * filled eye, and five modules of data -- in the colour given. The theme
 * has no QR code icon, and rectangles stay sharp at any size and take
 * the ambience's colour as the theme's icons do.
 *
 * The SVG strokes each outline 2.5 units wide about its edge; a
 * Rectangle's border is inside its edge, so each outline here is 1.25
 * units bigger all round.
 */
Item {
    id: mark

    property color color: Theme.primaryColor

    /// One unit of piirit's grid.
    readonly property real unit: mark.width / 32

    implicitWidth: Theme.iconSizeMedium
    implicitHeight: mark.width

    Repeater {
        model: [[4.5, 4.5], [18.5, 4.5], [4.5, 18.5]]
        delegate: Rectangle {
            objectName: "qrFinder"
            x: (modelData[0] - 1.25) * mark.unit
            y: (modelData[1] - 1.25) * mark.unit
            width: 11.5 * mark.unit
            height: width
            radius: 2.75 * mark.unit
            color: "transparent"
            border.width: 2.5 * mark.unit
            border.color: mark.color

            Rectangle {
                x: 4.5 * mark.unit
                y: x
                width: 2.5 * mark.unit
                height: width
                antialiasing: true
                color: mark.color
            }
        }
    }

    Repeater {
        model: [[17.25, 17.25], [24.75, 17.25], [21, 21], [17.25, 24.75], [24.75, 24.75]]
        delegate: Rectangle {
            objectName: "qrModule"
            x: modelData[0] * mark.unit
            y: modelData[1] * mark.unit
            width: 4 * mark.unit
            height: width
            radius: 0.5 * mark.unit
            color: mark.color
        }
    }
}
