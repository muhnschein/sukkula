// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A straight line from (x1, y1) to (x2, y2) in the parent's coordinates,
 * less `startGap` and `endGap` pixels at its ends -- so that it stops at
 * the edge of a round avatar, whose fill is see-through: a thin
 * rectangle, turned. The path of a transfer on the send radar.
 */
Rectangle {
    id: segment

    property real x1: 0
    property real y1: 0
    property real x2: 0
    property real y2: 0
    property real startGap: 0
    property real endGap: 0
    property real thickness: Math.max(2, Math.round(Theme.paddingSmall * 0.75))

    readonly property real span: Math.sqrt((segment.x2 - segment.x1) * (segment.x2 - segment.x1)
                                           + (segment.y2 - segment.y1) * (segment.y2 - segment.y1))
    readonly property real ux: segment.span > 0 ? (segment.x2 - segment.x1) / segment.span : 0
    readonly property real uy: segment.span > 0 ? (segment.y2 - segment.y1) / segment.span : 0

    x: segment.x1 + segment.ux * segment.startGap
    y: segment.y1 + segment.uy * segment.startGap - segment.thickness / 2
    width: Math.max(0, segment.span - segment.startGap - segment.endGap)
    height: segment.thickness
    radius: segment.thickness / 2
    color: Theme.highlightColor
    antialiasing: true
    transformOrigin: Item.Left
    rotation: Math.atan2(segment.y2 - segment.y1, segment.x2 - segment.x1) * 180 / Math.PI
}
