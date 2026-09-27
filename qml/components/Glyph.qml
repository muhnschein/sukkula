// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * The radars' line drawings, in the ambience's colours: a person, a
 * phone, a file, a text, a plus, the cloud the internet protocols go
 * through, a QR code, and the protocol marks. The marks are Sukkula's own,
 * not the protocols' logos; the QR code is piirit's icon
 * (icons/cover/qr.svg there), redrawn here.
 *
 * A canvas loses what it drew when the scene graph lets go of its
 * texture, as it does while the phone is locked: it paints again whenever
 * it can, is shown, or the app comes back to the front.
 *
 * Drawn in a unit square (the cloud: a unit-wide box) scaled to the
 * item, with ES5 only (Qt 5.6).
 */
Canvas {
    id: glyph

    /// "person", "phone", "file", "text", "add", "cloud", "qr",
    /// "local_send", "quick_share" or "bluetooth".
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
    onAvailableChanged: glyph.requestPaint()
    onVisibleChanged: glyph.requestPaint()

    Connections {
        target: Qt.application
        // Qt 5.6 handler syntax.
        onStateChanged: {
            if (Qt.application.state === Qt.ApplicationActive) {
                glyph.requestPaint()
            }
        }
    }

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

    // A phone: a rounded slab with a line for its speaker.
    function _phone(ctx) {
        var l = 0.30, t = 0.12, r = 0.70, b = 0.88, c = 0.08
        ctx.beginPath()
        ctx.moveTo(l + c, t)
        ctx.lineTo(r - c, t)
        ctx.arcTo(r, t, r, t + c, c)
        ctx.lineTo(r, b - c)
        ctx.arcTo(r, b, r - c, b, c)
        ctx.lineTo(l + c, b)
        ctx.arcTo(l, b, l, b - c, c)
        ctx.lineTo(l, t + c)
        ctx.arcTo(l, t, l + c, t, c)
        ctx.closePath()
        ctx.stroke()
        ctx.beginPath()
        ctx.moveTo(0.44, 0.22)
        ctx.lineTo(0.56, 0.22)
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

    // A plus, with no ring round it: the centre's disc is the ring.
    function _add(ctx) {
        ctx.beginPath()
        ctx.moveTo(0.5, 0.24)
        ctx.lineTo(0.5, 0.76)
        ctx.moveTo(0.24, 0.5)
        ctx.lineTo(0.76, 0.5)
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

    // A rounded rectangle's outline, as a path.
    function _roundRect(ctx, x, y, w, h, r) {
        ctx.beginPath()
        ctx.moveTo(x + r, y)
        ctx.lineTo(x + w - r, y)
        ctx.arcTo(x + w, y, x + w, y + r, r)
        ctx.lineTo(x + w, y + h - r)
        ctx.arcTo(x + w, y + h, x + w - r, y + h, r)
        ctx.lineTo(x + r, y + h)
        ctx.arcTo(x, y + h, x, y + h - r, r)
        ctx.lineTo(x, y + r)
        ctx.arcTo(x, y, x + r, y, r)
        ctx.closePath()
    }

    // A QR code: piirit's icon on its 32-unit grid -- three finder
    // patterns, outlined round a filled eye, and five modules of data.
    // Stroked at the width the caller gives (piirit's is 2.5 units).
    function _qr(ctx) {
        var u = 1 / 32
        var finders = [[4.5, 4.5], [18.5, 4.5], [4.5, 18.5]]
        for (var i = 0; i < finders.length; i++) {
            glyph._roundRect(ctx, finders[i][0] * u, finders[i][1] * u, 9 * u, 9 * u, 1.5 * u)
            ctx.stroke()
            ctx.fillRect((finders[i][0] + 3.25) * u, (finders[i][1] + 3.25) * u, 2.5 * u, 2.5 * u)
        }
        var data = [[17.25, 17.25], [24.75, 17.25], [21, 21], [17.25, 24.75], [24.75, 24.75]]
        for (var j = 0; j < data.length; j++) {
            glyph._roundRect(ctx, data[j][0] * u, data[j][1] * u, 4 * u, 4 * u, 0.5 * u)
            ctx.fill()
        }
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
        case "phone": glyph._phone(ctx); break
        case "file": glyph._file(ctx); break
        case "text": glyph._text(ctx); break
        case "add": glyph._add(ctx); break
        case "cloud": glyph._cloud(ctx); break
        case "qr": glyph._qr(ctx); break
        case "local_send": glyph._localSend(ctx); break
        case "quick_share": glyph._quickShare(ctx); break
        case "bluetooth": glyph._bluetooth(ctx); break
        }
    }
}
