// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Receive with a code by its QR code (spec v0.6, F-MW2, F-CR2), from the
 * Receive tab's "Scan a QR code": the camera, to read the code off the
 * sender's screen. Nobody says whether it is a Magic Wormhole or a croc
 * code: the QR code says. The offer then comes up in the consent dialog
 * like any other, before any data flows (CodeReceiver.qml). A code typed
 * in has a page of its own, TypeCodePage.qml, which this page leads to
 * when there is no camera to use.
 *
 * After piirit's QR page and scan view: the viewfinder takes the page
 * under its header. It is ScanView.qml, loaded by URL: the only file that
 * names a Camera, so a phone without one still has the typed code.
 *
 * A QR code of a Magic Wormhole code may name its mailbox server, which
 * that receive uses. A code read for a protocol switched off is refused
 * here, with a word why (F-C1).
 */
Page {
    id: page
    objectName: "scanPage"

    property QtObject engine
    /// The app is in front; the camera stops when it is not.
    property bool foreground: Qt.application.state === Qt.ApplicationActive
    readonly property bool busy: receiver.busy

    readonly property Item view: scanLoader.item
    readonly property bool cameraOn: page.status === PageStatus.Active && page.foreground
                                     && !receiver.busy && !receiver.leaving
    /// No camera to read with: the code can be typed in instead.
    readonly property bool noCamera: scanLoader.status === Loader.Error
                                     || (page.view !== null && page.view.cameraMissing)

    allowedOrientations: Orientation.Portrait

    CodeReceiver {
        id: receiver
        host: page
        engine: page.engine
        onFailed: {
            banner.show(message)
            page.retry()
        }
    }

    /// A QR code's code: {protocol, code, mailboxUrl}.
    function follow(result) {
        if (!page.engine.protocolEnabled(result.protocol)) {
            banner.show(result.protocol === "croc"
                        //: A croc QR code was scanned, and croc is off.
                        ? qsTr("That is a croc code, and croc is switched off in Settings.")
                        //: A Magic Wormhole QR code was scanned, and Magic Wormhole is off.
                        : qsTr("That is a Magic Wormhole code, and Magic Wormhole is switched off in Settings."))
            page.retry()
            return
        }
        receiver.receive(function (reply) {
            if (result.protocol === "croc") {
                page.engine.receiveCroc(result.code, reply)
            } else {
                page.engine.receiveWormhole(result.code, reply, result.mailboxUrl)
            }
        })
    }

    /// The camera reads again.
    function retry() {
        if (page.view) {
            page.view.reset()
        }
    }

    /// No camera: the page to type the code in, in this one's place.
    function typeInstead() {
        pageStack.replace(Qt.resolvedUrl("TypeCodePage.qml"), { engine: page.engine })
    }

    PageHeader {
        id: header
        //: Page title: receive over Magic Wormhole or croc by scanning the sender's QR code.
        title: qsTr("Scan a QR code")
    }

    Banner {
        id: banner
        anchors.top: header.bottom
    }

    Item {
        id: scanArea
        objectName: "scanArea"
        anchors {
            top: banner.bottom
            left: parent.left
            right: parent.right
            bottom: parent.bottom
        }

        Loader {
            id: scanLoader
            objectName: "scanLoader"
            anchors.fill: parent
            source: Qt.resolvedUrl("../components/ScanView.qml")
            onLoaded: {
                scanLoader.item.engine = page.engine
                scanLoader.item.scanned.connect(page.follow)
            }
        }

        Binding {
            target: scanLoader.item
            property: "active"
            value: page.cameraOn
            when: scanLoader.item !== null
        }

        Label {
            objectName: "noCamera"
            visible: scanLoader.status === Loader.Error
            anchors.centerIn: parent
            width: parent.width - 2 * Theme.horizontalPageMargin
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            color: Theme.secondaryHighlightColor
            //: This phone has no camera Sukkula can use; the code can still be typed in.
            text: qsTr("The camera is not available.")
        }

        // A code was read, and the receive is under way.
        BusyIndicator {
            objectName: "receiving"
            anchors.centerIn: parent
            running: receiver.busy
            size: BusyIndicatorSize.Large
        }

        // No camera to read with: the other way in, where a thumb finds it.
        Button {
            objectName: "typeInstead"
            anchors {
                horizontalCenter: parent.horizontalCenter
                bottom: parent.bottom
                bottomMargin: Theme.paddingLarge
            }
            visible: page.noCamera && !receiver.busy
            //: On the scan page when there is no camera: opens the page to type the code in instead.
            text: qsTr("Type in a code")
            onClicked: page.typeInstead()
        }
    }
}
