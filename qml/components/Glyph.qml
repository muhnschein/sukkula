// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * Sukkula's line drawings, in the ambience's colours: the devices (a
 * phone, a tablet, a computer, Bluetooth's rune for a paired one), the
 * kinds of file (a photo, a video, music, a document, any file), a
 * folder, a text, and a QR code, which is piirit's icon
 * (icons/cover/qr.svg there), redrawn here. A file's kind is told by its
 * name alone: no picture a peer sent is ever drawn.
 *
 * A canvas loses what it drew when the scene graph lets go of its
 * texture, as it does while the phone is locked: it paints again whenever
 * it can, is shown, or the app comes back to the front.
 *
 * Drawn in a unit square scaled to the item, with ES5 only (Qt 5.6).
 */
Canvas {
    id: glyph

    /// "phone", "tablet", "computer", "bluetooth", "file", "photo",
    /// "video", "music", "document", "folder", "text" or "qr".
    property string kind: "file"
    property color color: "white"
    /// Stroke width in pixels.
    property real lineWidth: Math.max(1.5, glyph.width / 16)

    implicitWidth: 64
    implicitHeight: glyph.width

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

    // A tablet: a wider slab, the button's dot at its foot.
    function _tablet(ctx) {
        glyph._roundRect(ctx, 0.20, 0.12, 0.60, 0.76, 0.06)
        ctx.stroke()
        ctx.beginPath()
        ctx.arc(0.5, 0.79, 0.025, 0, 2 * Math.PI, false)
        ctx.fill()
    }

    // A computer: a screen on a stand.
    function _computer(ctx) {
        glyph._roundRect(ctx, 0.12, 0.20, 0.76, 0.48, 0.04)
        ctx.stroke()
        ctx.beginPath()
        ctx.moveTo(0.5, 0.68)
        ctx.lineTo(0.5, 0.80)
        ctx.moveTo(0.34, 0.80)
        ctx.lineTo(0.66, 0.80)
        ctx.stroke()
    }

    // A photo: a frame with hills and a sun.
    function _photo(ctx) {
        glyph._roundRect(ctx, 0.12, 0.20, 0.76, 0.60, 0.06)
        ctx.stroke()
        ctx.beginPath()
        ctx.moveTo(0.12, 0.68)
        ctx.lineTo(0.34, 0.47)
        ctx.lineTo(0.52, 0.64)
        ctx.lineTo(0.62, 0.55)
        ctx.lineTo(0.88, 0.76)
        ctx.stroke()
        ctx.beginPath()
        ctx.arc(0.65, 0.38, 0.06, 0, 2 * Math.PI, false)
        ctx.stroke()
    }

    // A video: a frame with a play mark.
    function _video(ctx) {
        glyph._roundRect(ctx, 0.12, 0.20, 0.76, 0.60, 0.06)
        ctx.stroke()
        ctx.beginPath()
        ctx.moveTo(0.43, 0.38)
        ctx.lineTo(0.63, 0.50)
        ctx.lineTo(0.43, 0.62)
        ctx.closePath()
        ctx.stroke()
    }

    // Music: a quaver.
    function _music(ctx) {
        ctx.beginPath()
        ctx.arc(0.40, 0.72, 0.09, 0, 2 * Math.PI, false)
        ctx.stroke()
        ctx.beginPath()
        ctx.moveTo(0.49, 0.72)
        ctx.lineTo(0.49, 0.18)
        ctx.lineTo(0.70, 0.28)
        ctx.stroke()
    }

    // A document: a file with lines of text on it.
    function _document(ctx) {
        glyph._file(ctx)
        ctx.beginPath()
        ctx.moveTo(0.38, 0.48)
        ctx.lineTo(0.64, 0.48)
        ctx.moveTo(0.38, 0.60)
        ctx.lineTo(0.64, 0.60)
        ctx.moveTo(0.38, 0.72)
        ctx.lineTo(0.54, 0.72)
        ctx.stroke()
    }

    // A folder, with its tab.
    function _folder(ctx) {
        ctx.beginPath()
        ctx.moveTo(0.12, 0.26)
        ctx.lineTo(0.38, 0.26)
        ctx.lineTo(0.46, 0.34)
        ctx.lineTo(0.88, 0.34)
        ctx.lineTo(0.88, 0.78)
        ctx.lineTo(0.12, 0.78)
        ctx.closePath()
        ctx.stroke()
        ctx.beginPath()
        ctx.moveTo(0.12, 0.44)
        ctx.lineTo(0.88, 0.44)
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

    // Bluetooth's rune: a paired device.
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
        var scale = Math.min(glyph.width, glyph.height)
        ctx.translate((glyph.width - scale) / 2, (glyph.height - scale) / 2)
        ctx.scale(scale, scale)
        ctx.lineWidth = glyph.lineWidth / scale
        ctx.lineCap = "round"
        ctx.lineJoin = "round"
        ctx.strokeStyle = glyph.color
        ctx.fillStyle = glyph.color
        switch (glyph.kind) {
        case "phone": glyph._phone(ctx); break
        case "tablet": glyph._tablet(ctx); break
        case "computer": glyph._computer(ctx); break
        case "file": glyph._file(ctx); break
        case "photo": glyph._photo(ctx); break
        case "video": glyph._video(ctx); break
        case "music": glyph._music(ctx); break
        case "document": glyph._document(ctx); break
        case "folder": glyph._folder(ctx); break
        case "text": glyph._text(ctx); break
        case "qr": glyph._qr(ctx); break
        case "bluetooth": glyph._bluetooth(ctx); break
        }
    }
}
