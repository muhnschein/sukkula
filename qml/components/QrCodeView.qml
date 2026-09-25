// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * A QR code from the engine's rows of "0" and "1" (F-MW1), drawn dark on
 * white with the four-module quiet zone scanners need, whatever the
 * ambience. Engine.qml has already checked the shape: `size` rows of `size`
 * characters, 21 to 177.
 */
Item {
    id: view

    /// ["0101...", ...], `size` of them.
    property var rows: []
    property int size: 0

    readonly property int quiet: 4
    readonly property bool valid: view.size > 0 && view.rows && view.rows.length === view.size
    /// Whole pixels per module, so edges stay sharp.
    readonly property int module: view.valid
        ? Math.max(1, Math.floor(Math.min(view.width, view.height) / (view.size + 2 * view.quiet)))
        : 0

    implicitWidth: 300
    implicitHeight: implicitWidth
    visible: view.valid

    onRowsChanged: canvas.requestPaint()
    onSizeChanged: canvas.requestPaint()
    onModuleChanged: canvas.requestPaint()

    Canvas {
        id: canvas
        objectName: "qrCanvas"
        readonly property int side: view.module * (view.size + 2 * view.quiet)
        width: side
        height: side
        anchors.centerIn: parent

        onPaint: {
            var ctx = getContext("2d")
            ctx.fillStyle = "#ffffff"
            ctx.fillRect(0, 0, width, height)
            if (!view.valid || view.module < 1) {
                return
            }
            ctx.fillStyle = "#000000"
            var m = view.module
            var offset = view.quiet * m
            for (var y = 0; y < view.size; y++) {
                var row = view.rows[y]
                // Runs of dark modules as one rectangle each.
                var x = 0
                while (x < view.size) {
                    if (row.charAt(x) !== "1") {
                        x++
                        continue
                    }
                    var start = x
                    while (x < view.size && row.charAt(x) === "1") {
                        x++
                    }
                    ctx.fillRect(offset + start * m, offset + y * m, (x - start) * m, m)
                }
            }
        }
    }
}
