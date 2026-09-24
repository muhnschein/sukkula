// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The one screen most people will use: the Receive switch (F-C1) with how
 * each protocol is doing, the transfers with their progress and a way to
 * cancel (F-C5), and received texts with a Copy button (F-C4).
 *
 * Received texts are shown, never opened: no link in one is clickable,
 * nothing here calls Qt.openUrlExternally, and every label that shows what
 * a peer sent is plain text (S2, S8).
 */
Page {
    id: page
    objectName: "mainPage"

    property QtObject engine
    property bool switching: false
    property bool alive: true

    Component.onDestruction: page.alive = false

    function toggleReceiving() {
        if (page.switching) {
            return
        }
        page.switching = true
        var self = page
        page.engine.setReceiving(!page.engine.receiving, function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.switching = false
            if (!ok) {
                banner.show(self.engine.errorText(error))
            }
        })
    }

    function copyText(text) {
        Clipboard.text = text
        //: Shown after a received text was copied to the clipboard.
        banner.show(qsTr("Copied"), "info")
    }

    Connections {
        target: page.engine
        // Qt 5.6 handler syntax.
        onFailed: banner.show(message)
    }

    SilicaFlickable {
        id: flickable
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        PullDownMenu {
            MenuItem {
                //: Pulley menu: the page with the version and licence.
                text: qsTr("About Sukkula")
                onClicked: pageStack.push(Qt.resolvedUrl("AboutPage.qml"), { engine: page.engine })
            }
            MenuItem {
                //: Pulley menu.
                text: qsTr("Settings")
                onClicked: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"), { engine: page.engine })
            }
            MenuItem {
                //: Pulley menu: take finished transfers and received texts off the list.
                text: qsTr("Clear list")
                visible: page.engine.texts.count > 0
                         || page.engine.transfers.count > page.engine.activeTransfers
                onClicked: {
                    page.engine.clearFinished()
                    page.engine.clearTexts()
                }
            }
            MenuItem {
                //: Pulley menu: receive over Magic Wormhole by typing the sender's code.
                text: qsTr("Receive with a code")
                enabled: page.engine.running && page.engine.hasProtocol("wormhole")
                onClicked: pageStack.push(Qt.resolvedUrl("WormholeReceivePage.qml"),
                                          { engine: page.engine })
            }
            MenuItem {
                //: Pulley menu: choose files or a text and a way to send them.
                text: qsTr("Send…")
                enabled: page.engine.running
                onClicked: pageStack.push(Qt.resolvedUrl("SendPage.qml"), { engine: page.engine })
            }
        }

        Column {
            id: column
            width: parent.width

            PageHeader {
                title: "Sukkula"
            }

            Banner {
                id: banner
            }

            // The engine could not start: nothing below would work.
            Column {
                width: parent.width
                visible: page.engine.fatalCode !== ""
                spacing: Theme.paddingSmall

                Label {
                    objectName: "fatalLabel"
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    //: The engine failed to start; %1 says why.
                    text: qsTr("Sukkula could not start: %1").arg(page.engine.errorText({ code: page.engine.fatalCode }))
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    color: Theme.errorColor
                }
                Label {
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    text: page.engine.fatalDetail
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    visible: text.length > 0
                    font.pixelSize: Theme.fontSizeExtraSmall
                    color: Theme.secondaryColor
                }
            }

            // F-C1: one switch for every enabled receiver.
            TextSwitch {
                id: receiveSwitch
                objectName: "receiveSwitch"
                //: The main switch: listen for offers from nearby devices.
                text: qsTr("Receive")
                description: page.engine.receiving
                             //: Under the Receive switch while it is on.
                             ? qsTr("Nearby devices can offer you files")
                             //: Under the Receive switch while it is off.
                             : qsTr("Nobody nearby can see this phone")
                checked: page.engine.receiving
                automaticCheck: false
                busy: page.switching
                enabled: page.engine.running
                onClicked: page.toggleReceiving()
            }

            Label {
                objectName: "deviceNameLabel"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.engine.effectiveDeviceName.length > 0
                //: The name other devices see; %1 is that name.
                text: qsTr("Shown to others as %1").arg(page.engine.effectiveDeviceName)
                textFormat: Text.PlainText
                truncationMode: TruncationMode.Fade
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryHighlightColor
            }

            Item {
                width: 1
                height: Theme.paddingMedium
            }

            Repeater {
                model: page.engine.protocolStatuses
                delegate: Column {
                    width: column.width

                    Label {
                        x: Theme.horizontalPageMargin
                        width: parent.width - 2 * Theme.horizontalPageMargin
                        //: A protocol and its state, e.g. "LocalSend: Ready".
                        text: qsTr("%1: %2").arg(page.engine.protocolName(modelData.protocol))
                                            .arg(page.engine.stateText(modelData.state))
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.fontSizeSmall
                        color: modelData.state === "failed" ? Theme.errorColor : Theme.secondaryColor
                    }
                    Label {
                        x: Theme.horizontalPageMargin
                        width: parent.width - 2 * Theme.horizontalPageMargin
                        visible: modelData.errorCode !== ""
                        text: modelData.errorCode !== "" ? page.engine.errorText({ code: modelData.errorCode }) : ""
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.secondaryColor
                    }
                }
            }

            SectionHeader {
                //: Section heading over the list of transfers.
                text: qsTr("Transfers")
                visible: page.engine.transfers.count > 0
            }

            Repeater {
                model: page.engine.transfers
                delegate: TransferItem {
                    width: column.width
                    engine: page.engine
                    transferId: model.transferId
                    direction: model.direction
                    protocol: model.protocol
                    peer: model.peer
                    files: model.files
                    fileCount: model.fileCount
                    total: model.total
                    bytes: model.bytes
                    state: model.state
                    error: model.error
                    saved: model.saved
                    savedCount: model.savedCount
                    onCancelRequested: page.engine.cancel(transferId)
                }
            }

            SectionHeader {
                //: Section heading over texts other devices sent.
                text: qsTr("Received texts")
                visible: page.engine.texts.count > 0
            }

            Repeater {
                model: page.engine.texts
                delegate: ListItem {
                    id: textItem
                    width: column.width
                    contentHeight: textColumn.height + 2 * Theme.paddingMedium
                    onClicked: pageStack.push(Qt.resolvedUrl("TextPage.qml"),
                                              { from: model.from, text: model.text })

                    Column {
                        id: textColumn
                        x: Theme.horizontalPageMargin
                        y: Theme.paddingMedium
                        width: parent.width - 2 * Theme.horizontalPageMargin
                        spacing: Theme.paddingSmall

                        Label {
                            objectName: "textFrom"
                            width: parent.width
                            text: model.from
                            textFormat: Text.PlainText
                            truncationMode: TruncationMode.Fade
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.secondaryHighlightColor
                        }
                        Label {
                            objectName: "textBody"
                            width: parent.width
                            text: model.text
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            maximumLineCount: 4
                            elide: Text.ElideRight
                            color: textItem.highlighted ? Theme.highlightColor : Theme.primaryColor
                        }
                        Button {
                            objectName: "copyButton"
                            //: Copies a received text to the clipboard.
                            text: qsTr("Copy")
                            onClicked: page.copyText(model.text)
                        }
                    }
                }
            }

            Item {
                width: 1
                height: Theme.paddingLarge * 2
                visible: emptyHint.visible
            }

            Label {
                id: emptyHint
                objectName: "emptyHint"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.engine.running && page.engine.transfers.count === 0
                         && page.engine.texts.count === 0
                horizontalAlignment: Text.AlignHCenter
                text: page.engine.receiving
                      //: Empty main page while receiving is on.
                      ? qsTr("Waiting for offers. Nothing is saved until you accept it.")
                      //: Empty main page while receiving is off.
                      : qsTr("Switch on Receive to get files, or pull down to send.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeLarge
                color: Theme.secondaryHighlightColor
            }
        }

        VerticalScrollDecorator {}
    }
}
