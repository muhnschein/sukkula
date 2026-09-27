// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import QtMultimedia 5.6
import Sailfish.Silica 1.0

/*
 * The camera, pointed at the sender's QR code (spec v0.6, F-MW2, F-CR2).
 * After piirit's ScanView, which reads invite codes off the same phone's
 * camera: the viewfinder fills the view, and the line at the top says
 * what to point it at.
 *
 * Frames go to the scanner (src/scanner.h) a few times a second, the next
 * only once the last has been read. The first QR code with a Magic
 * Wormhole or croc code in it is handed out on `scanned`, as
 * Engine.readScan() reads it -- which protocol is the code's to say -- and
 * the camera stops until `reset()`. A QR code with anything else in it is
 * only said to be no code: nothing of it is shown.
 *
 * The only file that names a Camera: ScanPage.qml loads it by URL, so a
 * phone without one costs the viewfinder and not the typed code.
 *
 * Focus is asked for three ways, as piirit found a camera left to itself
 * too soft for any code to read: continuous autofocus in the video
 * pipeline, where the platform's cameras run it; a focus search every
 * couple of seconds while nothing has been read; and a tap on the
 * viewfinder, which focuses on the point under the finger.
 */
Item {
    id: view

    property QtObject engine
    /// The view is the one on screen. The camera runs only then.
    property bool active: false
    /// The page's typed code is open; the camera keeps looking behind it.
    property bool typing: false
    /// A code was read and handed out; nothing more is read until reset().
    property bool done: false
    /// A QR code was read that holds no code to receive with.
    property bool sawOther: false
    /// Frames read with nothing in them, for the spinner by the line.
    property int framesTried: 0

    readonly property QtObject scanner: view.engine ? view.engine.scanner : null
    readonly property bool cameraMissing: camera.availability !== Camera.Available
    readonly property bool looking: view.active && !view.done && !view.cameraMissing
    readonly property bool cameraReady: camera.cameraStatus === Camera.ActiveStatus

    /// {protocol: "wormhole" | "croc", code, mailboxUrl}.
    signal scanned(var result)

    /// Reads again, after the page could not use what was read.
    function reset() {
        view.done = false
        view.sawOther = false
    }

    /// What the scanner read in a frame, as the engine's JSON.
    function take(json) {
        // A frame grabbed before the view was covered or left says nothing.
        if (!view.looking) {
            return
        }
        var result = view.engine.readScan(json)
        if (result === null) {
            return
        }
        if (result.protocol === "") {
            view.sawOther = true
            otherShown.restart()
            return
        }
        view.done = true
        view.scanned(result)
    }

    Camera {
        id: camera
        objectName: "camera"
        // The video pipeline is where continuous autofocus runs.
        captureMode: Camera.CaptureVideo
        cameraState: view.looking ? Camera.ActiveState : Camera.UnloadedState
        focus.focusMode: Camera.FocusContinuous
        focus.focusPointMode: Camera.FocusPointAuto
        flash.mode: Camera.FlashOff
        // The resolution is picked as soon as the camera can say what it
        // has, which is before it goes active.
        onCameraStatusChanged: view.chooseViewfinder()
    }

    /// The long side asked of the camera. The platform's default is
    /// whatever is cheapest to run, and no grab can put back detail the
    /// camera never captured; the grab itself is scaled to what the
    /// engine reads (Scanner::MaxSide).
    readonly property int wantedLongSide: 1280
    property bool viewfinderChosen: false

    /// The smallest viewfinder that is big enough, or the biggest there is.
    /// Guarded all the way down: a camera that cannot say leaves the view
    /// working on whatever it gives.
    function chooseViewfinder() {
        if (view.viewfinderChosen || typeof camera.supportedViewfinderResolutions !== "function") {
            return
        }
        var offered = camera.supportedViewfinderResolutions()
        if (!offered || offered.length === 0) {
            return
        }
        var enough = null
        var largest = null
        for (var i = 0; i < offered.length; i++) {
            var size = offered[i]
            var longest = Math.max(size.width, size.height)
            if (longest <= 0) {
                continue
            }
            if (!largest || longest > Math.max(largest.width, largest.height)) {
                largest = size
            }
            if (longest >= view.wantedLongSide
                    && (!enough || longest < Math.max(enough.width, enough.height))) {
                enough = size
            }
        }
        var pick = enough || largest
        if (!pick) {
            return
        }
        camera.viewfinder.resolution = Qt.size(pick.width, pick.height)
        view.viewfinderChosen = true
    }

    // An autofocus run, for a camera that does not run one on its own.
    // Unlocked first: a search that finds focus locks it.
    function refocus() {
        camera.unlock()
        camera.searchAndLock()
    }

    Timer {
        objectName: "refocus"
        interval: 2500
        repeat: true
        triggeredOnStart: true
        running: view.looking && view.cameraReady
        onTriggered: view.refocus()
    }

    Timer {
        objectName: "grabber"
        interval: 300
        repeat: true
        running: view.looking && view.cameraReady && view.scanner !== null
        onTriggered: {
            if (!view.scanner.busy) {
                view.scanner.scan(viewfinder)
            }
        }
    }

    Connections {
        target: view.scanner
        onFound: view.take(json)
        onMissed: view.framesTried += 1
    }

    // How long "no code" stays up after the last such QR code.
    Timer {
        id: otherShown
        interval: 3000
        onTriggered: view.sawOther = false
    }

    VideoOutput {
        id: viewfinder
        objectName: "viewfinder"
        anchors.fill: parent
        source: camera
        // Filled rather than fitted: bars down the sides would be grabbed
        // with the picture, pixels the code does not get, and what is seen
        // is what is read.
        fillMode: VideoOutput.PreserveAspectCrop
        visible: !view.cameraMissing

        // Tap to focus on what is under the finger.
        MouseArea {
            objectName: "focusTap"
            anchors.fill: parent
            onClicked: {
                camera.focus.focusPointMode = Camera.FocusPointCustom
                camera.focus.customFocusPoint = Qt.point(mouse.x / width, mouse.y / height)
                view.refocus()
            }
        }
    }

    // Beside the line: the app is looking.
    BusyIndicator {
        id: spinner
        objectName: "looking"
        anchors {
            left: parent.left
            leftMargin: Theme.horizontalPageMargin
            verticalCenter: hint.verticalCenter
        }
        running: view.looking && view.framesTried > 0
        size: BusyIndicatorSize.Small
    }

    // At the top: the phone is held up, the code in the middle of the
    // picture, and the words belong where the eye already is.
    Label {
        id: hint
        objectName: "scanHint"
        anchors {
            left: spinner.right
            leftMargin: Theme.paddingMedium
            right: parent.right
            rightMargin: Theme.horizontalPageMargin
            top: parent.top
            topMargin: Theme.paddingMedium
        }
        visible: !view.done
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        color: view.sawOther || view.cameraMissing ? Theme.errorColor : Theme.secondaryHighlightColor
        text: view.cameraMissing
              //: The camera cannot be used (another app has it, or it is missing).
              ? qsTr("The camera is not available.")
              : view.sawOther
                //: A QR code was read, and it is not a Magic Wormhole or croc code.
                ? qsTr("That QR code holds no Magic Wormhole or croc code.")
                : view.typing
                  //: The code is being typed; the camera still reads.
                  ? qsTr("Or point the camera at the code")
                  //: How to scan.
                  : qsTr("Point the camera at the sender's QR code")
    }
}
