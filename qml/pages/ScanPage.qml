// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import QtMultimedia 5.6
import Sailfish.Silica 1.0

/*
 * Receive by scanning the QR code on the sender's screen (spec v0.6,
 * F-MW2, F-CR2): the camera's viewfinder, a frame of it handed to the
 * engine a few times a second (src/scanner.h), and the first Magic
 * Wormhole or croc code read in one given back to the page that opened
 * this one, which receives with it.
 *
 * The camera runs only while this page is on top and the app in front.
 * Nothing a QR code says is shown here: a code goes to the receive page's
 * code field, and anything else is only said to be no code.
 */
Page {
    id: page
    objectName: "scanPage"

    property QtObject engine
    /// The app is in front; the camera stops when it is not.
    property bool foreground: Qt.application.state === Qt.ApplicationActive
    /// A code was read, and handed over: nothing more is scanned.
    property bool done: false
    /// A QR code was read that holds no code to receive with.
    property bool sawOther: false

    readonly property QtObject scanner: page.engine ? page.engine.scanner : null
    readonly property bool active: page.status === PageStatus.Active && page.foreground && !page.done
    readonly property bool cameraReady: camera.cameraStatus === Camera.ActiveStatus
    readonly property bool cameraMissing: camera.availability !== Camera.Available

    /// {protocol: "wormhole" | "croc", code, mailboxUrl}, from Engine.readScan().
    signal scanned(var result)

    allowedOrientations: Orientation.Portrait

    /// What the scanner read in a frame, as JSON.
    function take(json) {
        // A frame grabbed before the page was covered or left says nothing.
        if (!page.active) {
            return
        }
        var result = page.engine.readScan(json)
        if (result === null) {
            return
        }
        if (result.protocol === "") {
            page.sawOther = true
            otherShown.restart()
            return
        }
        page.done = true
        page.scanned(result)
        // On top and active, so this is the page a pop takes.
        pageStack.pop()
    }

    Camera {
        id: camera
        objectName: "camera"
        captureMode: Camera.CaptureViewfinder
        cameraState: page.active ? Camera.ActiveState : Camera.UnloadedState
        focus.focusMode: Camera.FocusContinuous
        flash.mode: Camera.FlashOff
    }

    Timer {
        id: tick
        objectName: "scanTick"
        interval: 250
        repeat: true
        running: page.active && page.cameraReady && page.scanner !== null
        onTriggered: {
            if (!page.scanner.busy) {
                page.scanner.scan(viewfinder)
            }
        }
    }

    Connections {
        target: page.scanner
        onFound: page.take(json)
    }

    // How long "no code" stays up after the last such QR code.
    Timer {
        id: otherShown
        interval: 3000
        onTriggered: page.sawOther = false
    }

    Column {
        width: parent.width
        spacing: Theme.paddingLarge

        PageHeader {
            //: Page title: receive by scanning the sender's QR code.
            title: qsTr("Scan QR code")
        }

        // Square, so a code in the middle is as large in the frame as the
        // screen allows.
        Item {
            width: parent.width
            height: width

            VideoOutput {
                id: viewfinder
                objectName: "viewfinder"
                anchors.fill: parent
                anchors.margins: Theme.horizontalPageMargin
                source: camera
                fillMode: VideoOutput.PreserveAspectCrop
                autoOrientation: true
                visible: !page.cameraMissing
            }

            // The corners of the square to aim with.
            Repeater {
                model: 4
                Item {
                    id: corner
                    readonly property bool atRight: index % 2 === 1
                    readonly property bool atBottom: index >= 2
                    readonly property real arm: Theme.itemSizeSmall
                    x: corner.atRight ? viewfinder.x + viewfinder.width - corner.arm : viewfinder.x
                    y: corner.atBottom ? viewfinder.y + viewfinder.height - corner.arm : viewfinder.y
                    width: corner.arm
                    height: corner.arm
                    visible: viewfinder.visible
                    Rectangle {
                        width: corner.arm
                        height: Theme.paddingSmall
                        y: corner.atBottom ? corner.arm - height : 0
                        color: Theme.highlightColor
                    }
                    Rectangle {
                        width: Theme.paddingSmall
                        height: corner.arm
                        x: corner.atRight ? corner.arm - width : 0
                        color: Theme.highlightColor
                    }
                }
            }
        }

        Label {
            objectName: "scanHint"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            color: page.sawOther || page.cameraMissing ? Theme.errorColor : Theme.highlightColor
            text: page.cameraMissing
                  //: The camera cannot be used (another app has it, or it is missing).
                  ? qsTr("The camera is not available.")
                  : page.sawOther
                    //: A QR code was read, and it is not a Magic Wormhole or croc code.
                    ? qsTr("That QR code holds no Magic Wormhole or croc code.")
                    //: How to scan.
                    : qsTr("Point the camera at the QR code on the sender's screen.")
        }
    }
}
