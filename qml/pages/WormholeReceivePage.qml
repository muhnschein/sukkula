// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Receive over Magic Wormhole by typing the sender's code (F-MW2). The
 * offer then comes up in the consent dialog like any other, before any
 * data flows. No scanning: that would need the Camera permission, which
 * spec §2 does not grant.
 */
Page {
    id: page
    objectName: "wormholeReceivePage"

    property QtObject engine
    property bool busy: false
    property bool alive: true

    /// The code as the engine wants it: trimmed, lower case, spaces as
    /// dashes -- people read "7 guitarist revenge" aloud.
    readonly property string code: codeField.text.replace(/^\s+|\s+$/g, "").toLowerCase().replace(/\s+/g, "-")
    readonly property bool valid: page.code.length <= 100 && /^[0-9]+(-[a-z0-9]+)+$/.test(page.code)

    Component.onDestruction: page.alive = false

    function receive() {
        if (!page.valid || page.busy) {
            return
        }
        page.busy = true
        var self = page
        page.engine.receiveWormhole(page.code, function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.busy = false
            if (ok) {
                // The consent dialog comes up on its own once the sender's
                // offer arrives.
                pageStack.pop()
            } else {
                banner.show(self.engine.errorText(error))
            }
        })
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width
            spacing: Theme.paddingMedium

            PageHeader {
                //: Page title: receive over Magic Wormhole.
                title: qsTr("Receive with a code")
            }

            Banner {
                id: banner
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: How to receive with Magic Wormhole.
                text: qsTr("Type the code the sender's screen shows, such as 7-guitarist-revenge. You will see what is offered before anything is saved.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryHighlightColor
            }

            TextField {
                id: codeField
                objectName: "codeField"
                width: parent.width
                //: The text field for a Magic Wormhole code.
                label: qsTr("Code")
                placeholderText: "7-guitarist-revenge"
                inputMethodHints: Qt.ImhNoPredictiveText | Qt.ImhNoAutoUppercase | Qt.ImhPreferLowercase
                maximumLength: 100
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
                onClicked: page.receive()
            }

            BusyIndicator {
                anchors.horizontalCenter: parent.horizontalCenter
                running: page.busy
                visible: running
            }
        }
    }
}
