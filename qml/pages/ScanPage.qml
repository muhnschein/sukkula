// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Receive with a code (spec v0.6, F-MW2, F-CR2), from the Receive tab's
 * QR code: the camera, to read the code off the sender's screen, and under
 * it a button to type or paste the code instead. Nobody says whether it
 * is a Magic Wormhole or a croc code: a QR code says, and a typed one is
 * told by its shape (receive_code, docs/FFI.md). The offer then comes up
 * in the consent dialog like any other, before any data flows.
 *
 * After piirit's QR page and scan view: the viewfinder takes the page
 * under its header, and the typed code is a panel over it rather than a
 * page of its own, so reading and typing are one place. The viewfinder is
 * ScanView.qml, loaded by URL: the only file that names a Camera, so a
 * phone without one still has the typed code.
 *
 * A QR code of a Magic Wormhole code may name its mailbox server, which
 * that receive uses. A code read for a protocol switched off is refused
 * here, with a word why (F-C1); a typed one is refused by the engine.
 */
Page {
    id: page
    objectName: "scanPage"

    property QtObject engine
    /// The app is in front; the camera stops when it is not.
    property bool foreground: Qt.application.state === Qt.ApplicationActive
    /// A receive is under way: from the code to the answered offer.
    property bool busy: false
    property bool alive: true
    /// The code was taken: go back once this page is on top. See leave().
    property bool leaving: false
    /// The typed code's panel is open.
    property bool typing: false

    readonly property Item view: scanLoader.item
    readonly property bool cameraOn: page.status === PageStatus.Active && page.foreground
                                     && !page.busy && !page.leaving

    allowedOrientations: Orientation.Portrait

    Component.onDestruction: page.alive = false

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
        page.receive(function (reply) {
            if (result.protocol === "croc") {
                page.engine.receiveCroc(result.code, reply)
            } else {
                page.engine.receiveWormhole(result.code, reply, result.mailboxUrl)
            }
        })
    }

    /// The typed code: which protocol it is for, the engine tells.
    function followTyped() {
        var text = codeField.text.replace(/^\s+|\s+$/g, "")
        if (text.length === 0 || page.busy || !page.engine.running) {
            return
        }
        page.receive(function (reply) {
            page.engine.receiveCode(text, reply)
        })
    }

    function receive(send) {
        page.busy = true
        var self = page
        send(function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.busy = false
            if (ok) {
                // The consent dialog came up on its own once the sender's
                // offer arrived, and has been answered.
                self.leaving = true
                self.leave()
            } else {
                banner.show(self.engine.errorText(error))
                self.retry()
            }
        })
    }

    /// The camera reads again.
    function retry() {
        if (page.view) {
            page.view.reset()
        }
    }

    /// Opens the typed code, with the clipboard in it when it holds a
    /// code: a code copied a moment ago is the likely reason.
    function type() {
        page.typing = true
        var clip = Clipboard.text
        if (codeField.text.length === 0 && typeof clip === "string" && page.looksLikeCode(clip)) {
            codeField.text = clip.replace(/^\s+|\s+$/g, "")
        }
    }

    /// Whether some text looks like a code, so that a shopping list on the
    /// clipboard is not pasted. What it is, the engine decides.
    function looksLikeCode(text) {
        var t = text.replace(/^\s+|\s+$/g, "")
        if (t.length < 6 || t.length > 1024) {
            return false
        }
        var lower = t.toLowerCase()
        return lower.indexOf("wormhole-transfer:") === 0
                || lower.indexOf("https://getcroc.com/") === 0
                || /^[0-9]+[- ][a-z0-9]+([- ][a-z0-9]+)*$/.test(lower)
                || /^[a-z]+([- ][a-z]+){2,}$/.test(lower)
    }

    /// Back to the main page -- this page only, from the top only: the
    /// reply comes while the consent dialog is over this page, and a bare
    /// pop() took the dialog, declining its offer unanswered.
    function leave() {
        if (!page.leaving || page.alive !== true) {
            return
        }
        if (pageStack.currentPage !== page || page.status !== PageStatus.Active) {
            return
        }
        if (pageStack.busy) {
            leaveLater.restart()
            return
        }
        page.leaving = false
        pageStack.pop(pageStack.previousPage(page))
    }

    onStatusChanged: {
        if (page.status === PageStatus.Active) {
            page.leave()
        }
    }

    Timer {
        id: leaveLater
        interval: 100
        onTriggered: page.leave()
    }

    PageHeader {
        id: header
        //: Page title: receive over Magic Wormhole or croc.
        title: qsTr("Receive with a code")
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

        Binding {
            target: scanLoader.item
            property: "typing"
            value: page.typing
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
            //: This phone has no camera Sukkula can use; the code can still be typed.
            text: qsTr("The camera is not available. Type the code instead.")
        }

        // A code was read or typed, and the receive is under way.
        BusyIndicator {
            objectName: "receiving"
            anchors.centerIn: parent
            running: page.busy
            size: BusyIndicatorSize.Large
        }

        // The other way in, where a thumb finds it.
        Button {
            objectName: "typeCodeButton"
            anchors {
                horizontalCenter: parent.horizontalCenter
                bottom: parent.bottom
                bottomMargin: Theme.paddingLarge
            }
            visible: !page.typing && !page.busy
            //: Opens the field to type or paste the code instead of scanning it.
            text: qsTr("Enter code")
            onClicked: page.type()
        }

        Rectangle {
            objectName: "codePanel"
            anchors {
                left: parent.left
                right: parent.right
                bottom: parent.bottom
                bottomMargin: Theme.paddingLarge
            }
            height: codeField.height + receiveButton.height + 2 * Theme.paddingMedium
            visible: page.typing && !page.busy
            color: Theme.rgba(Theme.highlightDimmerColor, 0.8)

            TextField {
                id: codeField
                objectName: "codeField"
                anchors {
                    left: parent.left
                    right: parent.right
                    top: parent.top
                    topMargin: Theme.paddingMedium
                }
                //: The text field for a Magic Wormhole or croc code.
                label: qsTr("Code")
                placeholderText: "7-guitarist-revenge"
                inputMethodHints: Qt.ImhNoPredictiveText | Qt.ImhNoAutoUppercase | Qt.ImhPreferLowercase
                maximumLength: 1024
                Keys.onReturnPressed: page.followTyped()
                Keys.onEnterPressed: page.followTyped()
            }

            Button {
                id: receiveButton
                objectName: "receiveButton"
                anchors {
                    horizontalCenter: parent.horizontalCenter
                    top: codeField.bottom
                }
                //: Starts receiving with the typed code.
                text: qsTr("Receive")
                enabled: codeField.text.replace(/^\s+|\s+$/g, "").length > 0 && page.engine.running
                onClicked: page.followTyped()
            }
        }
    }
}
