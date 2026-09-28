// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Receive with a code typed in (spec v0.6, F-MW2, F-CR2), from the Receive
 * tab's "Type in a code": the code the sender read out or sent, typed or
 * pasted. Nobody says whether it is a Magic Wormhole or a croc code: the
 * engine tells by its shape (receive_code, docs/FFI.md), and refuses one
 * for a protocol switched off (F-C1). The offer then comes up in the
 * consent dialog like any other, before any data flows
 * (CodeReceiver.qml).
 *
 * The keyboard is up as soon as the page is, and a code on the clipboard
 * is already in the field -- a code copied a moment ago is the likely
 * reason to be here -- while anything else there is left alone.
 */
Page {
    id: page
    objectName: "typeCodePage"

    property QtObject engine
    /// The code field holds what the clipboard did, untouched since.
    property bool fromClipboard: false
    /// The keyboard was brought up once.
    property bool focused: false
    readonly property bool busy: receiver.busy
    readonly property string code: codeField.text.replace(/^\s+|\s+$/g, "")

    allowedOrientations: Orientation.Portrait

    CodeReceiver {
        id: receiver
        host: page
        engine: page.engine
        onFailed: banner.show(message)
    }

    Component.onCompleted: {
        var clip = Clipboard.text
        if (typeof clip === "string" && page.looksLikeCode(clip)) {
            codeField.text = clip.replace(/^\s+|\s+$/g, "")
            page.fromClipboard = true
        }
    }

    onStatusChanged: {
        if (page.status === PageStatus.Active && !page.focused) {
            page.focused = true
            codeField.forceActiveFocus()
        }
    }

    /// The code typed: which protocol it is for, the engine tells.
    function follow() {
        var text = page.code
        if (text.length === 0 || receiver.busy || !page.engine.running) {
            return
        }
        receiver.receive(function (reply) {
            page.engine.receiveCode(text, reply)
        })
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

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width
            spacing: Theme.paddingMedium

            PageHeader {
                //: Page title: receive over Magic Wormhole or croc with a code typed in.
                title: qsTr("Type in a code")
            }

            Banner {
                id: banner
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: Type in a code: what to type; the example is a Magic Wormhole code.
                text: qsTr("The sender's app shows it as a few words, like 7-guitarist-revenge.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryHighlightColor
            }

            TextField {
                id: codeField
                objectName: "codeField"
                width: parent.width
                enabled: !receiver.busy
                label: page.fromClipboard
                       //: The code field, filled in from the clipboard.
                       ? qsTr("Code, from the clipboard")
                       //: The text field for a Magic Wormhole or croc code.
                       : qsTr("Code")
                //: The code field when empty.
                placeholderText: qsTr("Code")
                inputMethodHints: Qt.ImhNoPredictiveText | Qt.ImhNoAutoUppercase | Qt.ImhPreferLowercase
                maximumLength: 1024
                onTextChanged: {
                    if (page.fromClipboard && codeField.text !== String(Clipboard.text).replace(/^\s+|\s+$/g, "")) {
                        page.fromClipboard = false
                    }
                }
                Keys.onReturnPressed: page.follow()
                Keys.onEnterPressed: page.follow()
            }

            Button {
                id: receiveButton
                objectName: "receiveButton"
                anchors.horizontalCenter: parent.horizontalCenter
                //: Starts receiving with the typed code.
                text: qsTr("Receive")
                enabled: page.code.length > 0 && page.engine.running && !receiver.busy
                onClicked: page.follow()
            }
        }

        VerticalScrollDecorator {}
    }

    // The code was sent off, and the offer is on its way.
    BusyIndicator {
        objectName: "receiving"
        anchors.centerIn: parent
        running: receiver.busy
        size: BusyIndicatorSize.Large
    }
}
