// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The picture Send and Receive mode share: this phone at the foot, round
 * rings centred on it, and the cloud the internet protocols go through
 * above them, with two tiles over the cloud (Magic Wormhole on the left,
 * croc on the right).
 *
 * The rings are about a peer apart and no wider than the screen, so the
 * outer ones run off the screen's sides (as AirDrop's and PairDrop's do).
 * Peers sit in slots packed along them at run time; SendView and
 * ReceiveView decide who goes where with place().
 */
Item {
    id: radar

    /// The rings pulse outwards: looking, or visible.
    property bool pulsing: false
    /// The rings are drawn faint: something else is the picture now.
    property bool faint: false
    /// Kept clear at the top, for what lies over the picture.
    property real topInset: 0

    /// Slot index -> the key of what is placed there, or "".
    property var slots: []
    /// Keys with no slot free.
    property int overflow: 0
    /// More places nobody may be put: [[l, t, r, b]].
    property var reserved: []

    // ---- Geometry -------------------------------------------------------

    readonly property real margin: Theme.horizontalPageMargin
    readonly property real avatar: Theme.itemSizeMedium * 0.9
    readonly property real originSize: Theme.itemSizeMedium
    readonly property real tileWidth: (radar.width - 3 * radar.margin) / 2
    readonly property real tileHeight: Theme.itemSizeLarge
    readonly property real cloudWidth: Math.min(radar.width * 0.42, Theme.itemSizeExtraLarge * 2)
    readonly property real cloudHeight: radar.cloudWidth * 0.6
    /// The line under the cloud and the one under the centre.
    readonly property real summaryHeight: Theme.fontSizeExtraSmall * 1.5
    /// The radar's centre: this phone.
    readonly property real ox: radar.width / 2
    readonly property real oy: radar.height - radar.originSize / 2 - radar.summaryHeight - Theme.paddingMedium
    /// The outer ring's radius: no wider than the screen, and low enough
    /// that the cloud, its label and the tiles fit above it.
    readonly property real outer: {
        var room = radar.oy - radar.topInset - Theme.paddingLarge - radar.tileHeight - radar.cloudHeight
                   - radar.summaryHeight - 2 * Theme.paddingMedium - radar.avatar / 2 - Theme.paddingLarge
        return Math.max(radar.avatar, Math.min(radar.width, room))
    }
    /// The cloud sits just above the outer ring, the tiles just above it:
    /// on a tall screen they come down with the rings rather than stay at
    /// the top.
    readonly property real cloudX: radar.ox
    readonly property real cloudY: radar.oy - radar.outer - radar.avatar / 2 - Theme.paddingMedium
                                   - radar.summaryHeight - radar.cloudHeight / 2
    readonly property real tilesY: Math.max(radar.topInset + Theme.paddingLarge,
                                            radar.cloudY - radar.cloudHeight / 2 - Theme.paddingMedium
                                            - radar.tileHeight)
    /// Where a tile's peer sits: the middle of the left tile (Magic
    /// Wormhole) or the right one (croc).
    readonly property real leftTileX: radar.margin + radar.tileWidth / 2
    readonly property real rightTileX: radar.width - radar.margin - radar.tileWidth / 2
    readonly property real tileMidY: radar.tilesY + radar.tileHeight / 2
    /// The rings' radii, innermost first, about a peer apart.
    readonly property var rings: {
        var count = Math.max(3, Math.floor(radar.outer / radar.avatar))
        var out = []
        for (var i = 1; i <= count; i++) {
            out.push(radar.outer * i / count)
        }
        return out
    }
    /// Beside this phone, inside the first ring's reach: "+N" on the right,
    /// a small button on the left.
    readonly property real sideGap: Math.max(radar.rings[0], radar.originSize * 1.3)
    readonly property real moreX: radar.ox + radar.sideGap
    readonly property real lessX: radar.ox - radar.sideGap
    /// Where peers go, in the order they are filled: [{x, y}], packed from
    /// the outer ring inwards, each ring from its top outwards in pairs,
    /// so that no avatar or name covers another, the centre or what is
    /// beside it. A taller screen has room for more.
    readonly property var slotSpots: radar.packSlots(radar.width, radar.rings)

    /// The box a peer at (x, y) takes, its name included: [l, t, r, b].
    function peerBox(x, y) {
        var half = radar.avatar * 1.3 / 2
        return [x - half, y - radar.avatar / 2, x + half,
                y + radar.avatar / 2 + Theme.paddingSmall / 2 + Theme.fontSizeExtraSmall * 1.4]
    }

    function packSlots(width, rings) {
        var out = []
        if (width <= 0 || !rings || rings.length === 0) {
            return out
        }
        var gap = Theme.paddingSmall / 2
        var overlaps = function (a, b) {
            return !(a[2] + gap <= b[0] || b[2] + gap <= a[0] || a[3] + gap <= b[1] || b[3] + gap <= a[1])
        }
        var reach = radar.originSize / 2 + Theme.paddingMedium
        var side = radar.avatar * 0.4 + gap
        var blocked = [
            [radar.ox - reach, radar.oy - reach, radar.ox + reach, radar.oy + reach + radar.summaryHeight],
            [radar.moreX - side, radar.oy - side, radar.moreX + side, radar.oy + side],
            [radar.lessX - side, radar.oy - side, radar.lessX + side, radar.oy + side]
        ]
        for (var x = 0; x < radar.reserved.length; x++) {
            blocked.push(radar.reserved[x])
        }
        var top = radar.cloudY + radar.cloudHeight / 2 + radar.summaryHeight + Theme.paddingSmall
        var edge = radar.avatar / 2 + Theme.paddingSmall
        var boxes = []
        var fits = function (p) {
            if (p.x < edge || p.x > width - edge || p.y - radar.avatar / 2 < top) {
                return false
            }
            var b = radar.peerBox(p.x, p.y)
            for (var i = 0; i < blocked.length; i++) {
                if (overlaps(b, blocked[i])) {
                    return false
                }
            }
            for (var j = 0; j < boxes.length; j++) {
                if (overlaps(b, boxes[j])) {
                    return false
                }
            }
            return true
        }
        // The innermost ring hugs this phone: nobody goes there.
        for (var r = rings.length - 1; r >= 1; r--) {
            for (var d = 0; d <= 90; d++) {
                var angles = d === 0 ? [90] : [90 - d, 90 + d]
                var pair = []
                for (var a = 0; a < angles.length; a++) {
                    var rad = angles[a] * Math.PI / 180
                    pair.push({ x: radar.ox + rings[r] * Math.cos(rad), y: radar.oy - rings[r] * Math.sin(rad) })
                }
                if (pair.length === 2 && overlaps(radar.peerBox(pair[0].x, pair[0].y),
                                                  radar.peerBox(pair[1].x, pair[1].y))) {
                    continue
                }
                var ok = true
                for (var q = 0; q < pair.length; q++) {
                    ok = ok && fits(pair[q])
                }
                if (ok) {
                    for (var k = 0; k < pair.length; k++) {
                        out.push(pair[k])
                        boxes.push(radar.peerBox(pair[k].x, pair[k].y))
                    }
                }
            }
        }
        return out
    }

    /// A slot's place; anything without one is drawn in the first.
    function spotX(slot) {
        if (radar.slotSpots.length === 0) {
            return radar.ox
        }
        return radar.slotSpots[slot >= 0 && slot < radar.slotSpots.length ? slot : 0].x
    }
    function spotY(slot) {
        if (radar.slotSpots.length === 0) {
            return radar.oy - radar.outer
        }
        return radar.slotSpots[slot >= 0 && slot < radar.slotSpots.length ? slot : 0].y
    }

    /// Gives each key a slot, keeping every key's slot while it stays:
    /// nobody jumps when somebody else comes or goes. `wanted` maps a new
    /// key to the slot it should have if that is free (a peer that was
    /// already on screen under another key).
    function place(keys, wanted) {
        var present = {}
        for (var i = 0; i < keys.length; i++) {
            present[keys[i]] = true
        }
        var next = []
        var placed = {}
        for (var s = 0; s < radar.slotSpots.length; s++) {
            var key = s < radar.slots.length ? radar.slots[s] : ""
            if (key !== "" && present[key] === true) {
                next.push(key)
                placed[key] = true
            } else {
                next.push("")
            }
        }
        var left = 0
        for (var k = 0; k < keys.length; k++) {
            if (placed[keys[k]] === true) {
                continue
            }
            var want = wanted && wanted[keys[k]] !== undefined ? wanted[keys[k]] : -1
            var free = want >= 0 && want < next.length && next[want] === "" ? want : next.indexOf("")
            if (free < 0) {
                left++
                continue
            }
            next[free] = keys[k]
            placed[keys[k]] = true
        }
        if (JSON.stringify(next) !== JSON.stringify(radar.slots)) {
            radar.slots = next
        }
        radar.overflow = left
    }

    clip: true

    // The rings, pulsing outwards.
    Repeater {
        model: radar.rings
        delegate: Rectangle {
            readonly property real r: modelData
            x: radar.ox - r
            y: radar.oy - r
            width: 2 * r
            height: width
            radius: r
            color: "transparent"
            border.width: 2
            border.color: radar.faint ? Theme.rgba(Theme.secondaryColor, 0.35) : Theme.primaryColor
            opacity: 0.6

            SequentialAnimation on opacity {
                running: radar.pulsing
                loops: Animation.Infinite
                alwaysRunToEnd: true
                PauseAnimation { duration: 250 * index }
                NumberAnimation { to: 1; duration: 350; easing.type: Easing.OutQuad }
                NumberAnimation { to: 0.6; duration: 700; easing.type: Easing.InQuad }
                PauseAnimation { duration: 250 * (radar.rings.length - 1 - index) + 600 }
            }
        }
    }
}
