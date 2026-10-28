// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The circle both tabs and the code page are built round (spec v0.7): one
 * place on the screen for how things stand, so moving from step to step
 * changes what is in it and not where it is.
 *
 * - "idle": two faint rings round a disc with an icon in it.
 * - "looking": the same, with a sweep going round while the Send tab
 *   looks for devices.
 * - "ready": the same, with thin rings going out from the disc past the
 *   outer one, a second apart, while the Receive tab can be seen.
 * - "waiting": a ring with an arc going round it, round the disc, while
 *   an answer or the receiver is waited for.
 * - "progress": the ring filling up, with the percentage in the middle.
 * - "done": the ring whole, round a filled disc with a check.
 * - "failed": the same in red, with a cross.
 *
 * As the design canvas draws it: the disc is 0.432 of the outer ring,
 * the inner ring 0.705, and a line is the canvas's in its 176 px. The
 * sweep and the arc are drawn on canvases; a canvas loses what it drew
 * when the scene graph lets go of its texture, as it does while the phone
 * is locked, so each paints again whenever it can, is shown, or the app
 * comes back to the front.
 */
Item {
    id: anchor

    /// "idle", "looking", "ready", "waiting", "progress", "done" or "failed".
    property string mode: "idle"
    /// Glyph.qml's kind for the disc.
    property string glyph: "phone"
    /// How far, 0 to 1, in "progress".
    property real value: 0
    /// Animations may run: on screen, and the app in front.
    property bool running: true

    /// How far round the 3 s cycle the first pulse is.
    property real phase: 0
    readonly property real line: Math.max(1, anchor.width / 176)
    readonly property bool radar: anchor.mode === "idle" || anchor.mode === "looking" || anchor.mode === "ready"
    readonly property bool arc: anchor.mode === "waiting" || anchor.mode === "progress"
    readonly property bool ended: anchor.mode === "done" || anchor.mode === "failed"
    readonly property bool pulsing: anchor.mode === "ready" && anchor.running
    readonly property bool sweeping: anchor.mode === "looking" && anchor.running
    readonly property color ink: anchor.mode === "failed" ? Theme.errorColor : Theme.highlightColor

    implicitWidth: Theme.iconSizeMedium * 1.9 / 0.432
    implicitHeight: anchor.width

    /// CSS's ease-out, cubic-bezier(0, 0, 0.58, 1), which the canvas's
    /// pulses use: x(s) = 1.74 s^2 - 0.74 s^3 is found for `t` by
    /// halving, and y(s) = 3 s^2 - 2 s^3 is the eased value.
    function easeOut(t) {
        var lo = 0
        var hi = 1
        for (var i = 0; i < 16; i++) {
            var s = (lo + hi) / 2
            if (1.74 * s * s - 0.74 * s * s * s < t) {
                lo = s
            } else {
                hi = s
            }
        }
        var m = (lo + hi) / 2
        return 3 * m * m - 2 * m * m * m
    }

    function repaint() {
        sweep.requestPaint()
        arcCanvas.requestPaint()
    }

    onWidthChanged: anchor.repaint()
    onVisibleChanged: anchor.repaint()
    onModeChanged: anchor.repaint()
    onValueChanged: arcCanvas.requestPaint()

    Connections {
        target: Qt.application
        // Qt 5.6 handler syntax.
        onStateChanged: {
            if (Qt.application.state === Qt.ApplicationActive) {
                anchor.repaint()
            }
        }
    }

    // ---- The radar --------------------------------------------------------

    Repeater {
        model: [1, 0.705]
        delegate: Rectangle {
            objectName: "anchorRing"
            anchors.centerIn: parent
            width: anchor.width * modelData
            height: width
            radius: width / 2
            visible: anchor.radar
            color: "transparent"
            border.width: anchor.line
            border.color: Theme.rgba(Theme.highlightColor, 0.24)
        }
    }

    // A wedge of light, brightest at its leading edge, going round.
    Canvas {
        id: sweep
        objectName: "anchorSweep"
        anchors.fill: parent
        visible: anchor.sweeping

        onAvailableChanged: sweep.requestPaint()
        onPaint: {
            var ctx = sweep.getContext("2d")
            ctx.reset()
            var r = Math.min(sweep.width, sweep.height) / 2
            if (r <= 0) {
                return
            }
            // Counter-clockwise from the leading edge, which the rotation
            // carries clockwise.
            var g = ctx.createConicalGradient(r, r, -Math.PI / 2)
            g.addColorStop(0, Theme.rgba(Theme.highlightColor, 0.72))
            g.addColorStop(0.25, Theme.rgba(Theme.highlightColor, 0))
            g.addColorStop(1, Theme.rgba(Theme.highlightColor, 0))
            ctx.fillStyle = g
            ctx.beginPath()
            ctx.arc(r, r, r, 0, 2 * Math.PI, false)
            ctx.fill()
        }

        NumberAnimation on rotation {
            from: 0
            to: 360
            duration: 3000
            loops: Animation.Infinite
            running: anchor.sweeping
        }
    }

    NumberAnimation {
        target: anchor
        property: "phase"
        from: 0
        to: 1
        duration: 3000
        loops: Animation.Infinite
        running: anchor.pulsing
    }

    Repeater {
        model: 3
        delegate: Rectangle {
            id: pulse
            objectName: "pulse"
            /// This pulse's way out, 0 to 1, eased.
            readonly property real progress: anchor.easeOut((anchor.phase + index / 3) % 1)
            anchors.centerIn: parent
            width: anchor.width
            height: width
            radius: width / 2
            visible: anchor.pulsing
            color: "transparent"
            border.width: 1.5 * anchor.line
            border.color: Theme.highlightColor
            scale: 0.45 + 0.7 * pulse.progress
            opacity: 0.9 * (1 - pulse.progress)
        }
    }

    // ---- The ring ---------------------------------------------------------

    // Waiting: an arc going round. Going: the arc from the top, as far as
    // it has got, over a faint track.
    Canvas {
        id: arcCanvas
        objectName: "anchorArc"
        anchors.fill: parent
        visible: anchor.arc

        onAvailableChanged: arcCanvas.requestPaint()
        onPaint: {
            var ctx = arcCanvas.getContext("2d")
            ctx.reset()
            var w = 3 * anchor.line
            var r = Math.min(arcCanvas.width, arcCanvas.height) / 2 - w / 2
            if (r <= 0) {
                return
            }
            var c = r + w / 2
            ctx.lineWidth = w
            ctx.lineCap = "round"
            ctx.strokeStyle = Theme.rgba(Theme.primaryColor, 0.14)
            ctx.beginPath()
            ctx.arc(c, c, r, 0, 2 * Math.PI, false)
            ctx.stroke()
            var span = anchor.mode === "waiting" ? Math.PI * 0.45
                                                 : 2 * Math.PI * Math.max(0, Math.min(1, anchor.value))
            if (span <= 0) {
                return
            }
            ctx.strokeStyle = Theme.highlightColor
            ctx.beginPath()
            ctx.arc(c, c, r, -Math.PI / 2, -Math.PI / 2 + span, false)
            ctx.stroke()
        }

        RotationAnimation on rotation {
            from: 0
            to: 360
            duration: 1400
            loops: Animation.Infinite
            running: anchor.mode === "waiting" && anchor.running
            onRunningChanged: {
                if (!running) {
                    arcCanvas.rotation = 0
                }
            }
        }
    }

    // Done, or not: the ring whole.
    Rectangle {
        objectName: "anchorWhole"
        anchors.fill: parent
        radius: width / 2
        visible: anchor.ended
        color: "transparent"
        border.width: 3 * anchor.line
        border.color: anchor.ink
    }

    // ---- The middle -------------------------------------------------------

    Rectangle {
        id: disc
        objectName: "anchorDisc"
        anchors.centerIn: parent
        width: anchor.width * 0.432
        height: width
        radius: width / 2
        visible: anchor.mode !== "progress"
        color: anchor.ended ? anchor.ink : Theme.rgba(Theme.highlightColor, 0.1)
        border.width: anchor.ended ? 0 : 2 * anchor.line
        border.color: Theme.highlightColor

        Glyph {
            objectName: "anchorGlyph"
            anchors.centerIn: parent
            kind: anchor.mode === "done" ? "check" : anchor.mode === "failed" ? "cross" : anchor.glyph
            color: anchor.mode === "done" ? Theme.highlightDimmerColor
                   : anchor.mode === "failed" ? "#ffffff" : Theme.highlightColor
        }
    }

    Label {
        objectName: "anchorPercent"
        anchors.centerIn: parent
        visible: anchor.mode === "progress"
        text: Math.floor(100 * Math.max(0, Math.min(1, anchor.value))) + "%"
        textFormat: Text.PlainText
        font.pixelSize: Theme.fontSizeHuge
        color: Theme.highlightColor
    }
}
