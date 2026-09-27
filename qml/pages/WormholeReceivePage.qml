// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Receive over Magic Wormhole or croc by typing the sender's code (F-MW2,
 * F-CR2), or by scanning the QR code on the sender's screen (spec v0.6,
 * ScanPage.qml). The offer then comes up in the consent dialog like any
 * other, before any data flows.
 *
 * A scanned code decides the protocol: a croc code scanned here receives
 * over croc, if croc is switched on (F-C1). A Magic Wormhole QR code may
 * name its mailbox server, which this receive then uses, as long as the
 * code in the field is still the one scanned.
 */
Page {
    id: page
    objectName: "wormholeReceivePage"

    property QtObject engine
    /// "wormhole" or "croc".
    property string protocol: "wormhole"
    readonly property bool croc: page.protocol === "croc"
    property bool busy: false
    property bool alive: true
    /// The code was taken: go back once this page is on top. See leave().
    property bool leaving: false
    /// The last code scanned, and the mailbox server its QR code named.
    property string scannedCode: ""
    property string scannedMailbox: ""

    /// The code as the engine wants it: trimmed, spaces as dashes --
    /// people read "7 guitarist revenge" aloud -- and a wormhole code in
    /// lower case. A croc code is compared byte for byte, so its case
    /// stays as typed.
    readonly property string trimmed: codeField.text.replace(/^\s+|\s+$/g, "").replace(/\s+/g, "-")
    readonly property string code: page.croc ? page.trimmed : page.trimmed.toLowerCase()
    /// croc takes any 6 to 128 printable ASCII characters.
    readonly property bool valid: page.croc ? /^[\x21-\x7e]{6,128}$/.test(page.code)
                                            : page.code.length <= 100 && /^[0-9]+(-[a-z0-9]+)+$/.test(page.code)

    Component.onDestruction: page.alive = false

    function receive() {
        if (!page.valid || page.busy) {
            return
        }
        page.busy = true
        var self = page
        var reply = function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.busy = false
            if (ok) {
                // The consent dialog comes up on its own once the sender's
                // offer arrives.
                self.leaving = true
                self.leave()
            } else {
                banner.show(self.engine.errorText(error))
            }
        }
        if (page.croc) {
            page.engine.receiveCroc(page.code, reply)
        } else {
            // The scanned mailbox goes with the scanned code only.
            var mailbox = page.code === page.scannedCode ? page.scannedMailbox : ""
            page.engine.receiveWormhole(page.code, reply, mailbox)
        }
    }

    /// Opens the camera; what it reads comes back to takeScan().
    function scan() {
        var scanning = pageStack.push(Qt.resolvedUrl("ScanPage.qml"), { engine: page.engine })
        scanning.scanned.connect(page.takeScan)
    }

    /// A code read off the sender's screen: {protocol, code, mailboxUrl}.
    function takeScan(result) {
        if (page.alive !== true || page.busy) {
            return
        }
        if (!page.engine.protocolEnabled(result.protocol)) {
            banner.show(result.protocol === "croc"
                        //: A croc QR code was scanned, and croc is off.
                        ? qsTr("That is a croc code, and croc is switched off in Settings.")
                        //: A Magic Wormhole QR code was scanned, and Magic Wormhole is off.
                        : qsTr("That is a Magic Wormhole code, and Magic Wormhole is switched off in Settings."))
            return
        }
        page.protocol = result.protocol
        codeField.text = result.code
        page.scannedCode = page.code
        page.scannedMailbox = result.protocol === "wormhole" ? result.mailboxUrl : ""
        if (page.valid) {
            page.receive()
        } else {
            //: A scanned code this app cannot receive with.
            banner.show(qsTr("That code cannot be used here."))
        }
    }

    /// Back to the main page -- this page only, from the top only: the
    /// reply can come while a consent dialog is over this page, and a bare
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

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width
            spacing: Theme.paddingMedium

            PageHeader {
                //: Page title: receive over Magic Wormhole or croc.
                title: qsTr("Receive with a code")
                description: page.engine.protocolName(page.protocol)
            }

            Banner {
                id: banner
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                text: page.croc
                      //: How to receive with croc.
                      ? qsTr("Type the code the sender's croc shows, such as gala-tulip-acorn, or scan its QR code. You will see what is offered before anything is saved.")
                      //: How to receive with Magic Wormhole.
                      : qsTr("Type the code the sender's screen shows, such as 7-guitarist-revenge, or scan its QR code. You will see what is offered before anything is saved.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryHighlightColor
            }

            TextField {
                id: codeField
                objectName: "codeField"
                width: parent.width
                //: The text field for a Magic Wormhole or croc code.
                label: qsTr("Code")
                placeholderText: page.croc ? "gala-tulip-acorn" : "7-guitarist-revenge"
                inputMethodHints: Qt.ImhNoPredictiveText | Qt.ImhNoAutoUppercase | Qt.ImhPreferLowercase
                maximumLength: page.croc ? 128 : 100
                errorHighlight: text.length > 0 && !page.valid
                Keys.onReturnPressed: page.receive()
                Keys.onEnterPressed: page.receive()
            }

            Button {
                objectName: "receiveButton"
                anchors.horizontalCenter: parent.horizontalCenter
                //: Starts receiving with the typed code.
                text: qsTr("Receive")
                enabled: page.valid && !page.busy && page.engine.running
                         && page.engine.protocolEnabled(page.protocol)
                onClicked: page.receive()
            }

            Button {
                objectName: "scanButton"
                anchors.horizontalCenter: parent.horizontalCenter
                //: Opens the camera to read the code off the sender's screen.
                text: qsTr("Scan QR code")
                visible: page.engine.scanner !== null
                enabled: !page.busy && page.engine.running
                onClicked: page.scan()
            }

            BusyIndicator {
                anchors.horizontalCenter: parent.horizontalCenter
                running: page.busy
                visible: running
            }
        }
    }
}
