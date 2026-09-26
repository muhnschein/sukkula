// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * The send radar's line drawings, in the ambience's colours: a person, a
 * file, a text, the cloud the internet protocols go through, and the
 * protocol marks. The marks are Sukkula's own, not the protocols' logos.
 *
 * Drawn in a unit square (the cloud: a unit-wide box) scaled to the
 * item, with ES5 only (Qt 5.6).
 */
Canvas {
    id: glyph

    /// "person", "file", "text", "cloud", "local_send", "quick_share" or
    /// "bluetooth".
    property string kind: "person"
    property color color: "white"
    /// Stroke width in pixels.
    property real lineWidth: Math.max(1.5, glyph.width / 16)

    implicitWidth: 64
    implicitHeight: glyph.kind === "cloud" ? Math.round(glyph.width * 0.6) : glyph.width

    onKindChanged: glyph.requestPaint()
    onColorChanged: glyph.requestPaint()
    onLineWidthChanged: glyph.requestPaint()
    onWidthChanged: glyph.requestPaint()
    onHeightChanged: glyph.requestPaint()

    function _deg(d) {
        return d * Math.PI / 180
    }

    function _person(ctx) {
        ctx.beginPath()
        ctx.arc(0.5, 0.36, 0.16, 0, 2 * Math.PI, false)
        ctx.stroke()
        ctx.beginPath()
        ctx.arc(0.5, 0.92, 0.32, Math.PI * 1.08, Math.PI * 1.92, false)
        ctx.stroke()
    }

    function _file(ctx) {
        ctx.beginPath()
        ctx.moveTo(0.28, 0.14)
        ctx.lineTo(0.58, 0.14)
        ctx.lineTo(0.74, 0.30)
        ctx.lineTo(0.74, 0.86)
        ctx.lineTo(0.28, 0.86)
        ctx.closePath()
        ctx.stroke()
        ctx.beginPath()
        ctx.moveTo(0.58, 0.14)
        ctx.lineTo(0.58, 0.30)
        ctx.lineTo(0.74, 0.30)
        ctx.stroke()
    }

    function _text(ctx) {
        var rows = [[0.22, 0.30, 0.78], [0.22, 0.45, 0.78], [0.22, 0.60, 0.78], [0.22, 0.75, 0.56]]
        ctx.beginPath()
        for (var i = 0; i < rows.length; i++) {
            ctx.moveTo(rows[i][0], rows[i][1])
            ctx.lineTo(rows[i][2], rows[i][1])
        }
        ctx.stroke()
    }

    // Three circles and a flat base; the angles are where they meet.
    function _cloud(ctx) {
        ctx.beginPath()
        ctx.arc(0.27, 0.44, 0.17, glyph._deg(109.75), glyph._deg(270.69), false)
        ctx.arc(0.49, 0.30, 0.22, glyph._deg(187.83), glyph._deg(342.13), false)
        ctx.arc(0.73, 0.42, 0.19, glyph._deg(260.73), glyph._deg(431.33), false)
        ctx.closePath()
        ctx.stroke()
    }

    // LocalSend: a dotted ring round a dot.
    function _localSend(ctx) {
        var dots = 8
        for (var i = 0; i < dots; i++) {
            var a = 2 * Math.PI * i / dots
            ctx.beginPath()
            ctx.arc(0.5 + 0.28 * Math.cos(a), 0.5 + 0.28 * Math.sin(a), 0.055, 0, 2 * Math.PI, false)
            ctx.fill()
        }
        ctx.beginPath()
        ctx.arc(0.5, 0.5, 0.11, 0, 2 * Math.PI, false)
        ctx.fill()
    }

    // Quick Share: two chevrons, quick.
    function _quickShare(ctx) {
        ctx.beginPath()
        ctx.moveTo(0.26, 0.28)
        ctx.lineTo(0.46, 0.50)
        ctx.lineTo(0.26, 0.72)
        ctx.moveTo(0.52, 0.28)
        ctx.lineTo(0.72, 0.50)
        ctx.lineTo(0.52, 0.72)
        ctx.stroke()
    }

    // Bluetooth's rune, for when the theme has no icon for it.
    function _bluetooth(ctx) {
        ctx.beginPath()
        ctx.moveTo(0.30, 0.34)
        ctx.lineTo(0.68, 0.66)
        ctx.lineTo(0.50, 0.82)
        ctx.lineTo(0.50, 0.18)
        ctx.lineTo(0.68, 0.34)
        ctx.lineTo(0.30, 0.66)
        ctx.stroke()
    }

    onPaint: {
        var ctx = glyph.getContext("2d")
        ctx.reset()
        if (glyph.width <= 0 || glyph.height <= 0) {
            return
        }
        var scale = glyph.width
        var y0 = glyph.kind === "cloud" ? (glyph.height - 0.52 * scale) / 2 - 0.08 * scale : 0
        ctx.translate(0, y0)
        ctx.scale(scale, scale)
        ctx.lineWidth = glyph.lineWidth / scale
        ctx.lineCap = "round"
        ctx.lineJoin = "round"
        ctx.strokeStyle = glyph.color
        ctx.fillStyle = glyph.color
        switch (glyph.kind) {
        case "person": glyph._person(ctx); break
        case "file": glyph._file(ctx); break
        case "text": glyph._text(ctx); break
        case "cloud": glyph._cloud(ctx); break
        case "local_send": glyph._localSend(ctx); break
        case "quick_share": glyph._quickShare(ctx); break
        case "bluetooth": glyph._bluetooth(ctx); break
        }
    }
}
