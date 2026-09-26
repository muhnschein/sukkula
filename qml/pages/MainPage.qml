// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The one screen: Send or Receive, switched at its foot (F-C1).
 *
 * The mode is the engine's state. Receive switches every enabled receiver
 * on and shows how each protocol is doing, the transfers with their
 * progress and a way to cancel (F-C5), and received texts with a Copy
 * button (F-C4). Send switches the receivers off and shows the send radar
 * (SendView): discovery runs while this page is in Send mode, and what to
 * send waits at the radar's centre, filled from the Share menu (F-C6).
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
    /// Discovery is held by this page (Engine.qml counts who asked).
    property bool discovering: false
    /// Send mode was asked for while a switch was on its way.
    property bool sendNext: false
    /// The paired Bluetooth devices were asked for, this time round.
    property bool devicesListed: false
    /// What to send: the radar's centre.
    property alias payload: payload
    property alias sendView: sendView

    readonly property bool receiveMode: page.engine.receiving
    /// Send mode shows the radar; an engine that could not start shows
    /// why, in the receive column, instead.
    readonly property bool showSend: !page.receiveMode && page.engine.fatalCode === ""
    readonly property bool wantDiscovery: page.alive && page.engine.running && !page.engine.receiving
    readonly property bool bluetoothOn: page.engine.protocolEnabled("bluetooth")

    Component.onDestruction: {
        page.alive = false
        if (page.discovering && page.engine) {
            page.discovering = false
            page.engine.stopDiscovery()
        }
    }

    Component.onCompleted: page.syncDiscovery()
    onWantDiscoveryChanged: page.syncDiscovery()

    onBluetoothOnChanged: page.listDevices()

    onStatusChanged: {
        if (page.status === PageStatus.Active) {
            sendView.returned()
        }
    }

    /// Discovery runs in Send mode only, and the paired Bluetooth devices
    /// are listed as it starts (F-LS1, F-QS1, F-BT1).
    function syncDiscovery() {
        if (page.wantDiscovery && !page.discovering) {
            page.discovering = true
            page.engine.startDiscovery()
            page.listDevices()
        } else if (!page.wantDiscovery && page.discovering) {
            page.discovering = false
            page.devicesListed = false
            page.engine.stopDiscovery()
        }
    }

    /// Once per turn in Send mode, and again when Bluetooth is switched
    /// back on. No banner for a Bluetooth that is off: the radar just has
    /// no Bluetooth devices on it.
    function listDevices() {
        if (!page.bluetoothOn) {
            page.devicesListed = false
            return
        }
        if (!page.discovering || page.devicesListed) {
            return
        }
        page.devicesListed = true
        page.engine.listBluetoothDevices(function () {})
    }

    /// Send or Receive: one set_receiving; the switch follows the engine.
    function setMode(receive) {
        if (page.switching || receive === page.engine.receiving || !page.engine.running) {
            return
        }
        page.switching = true
        var self = page
        page.engine.setReceiving(receive, function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.switching = false
            if (!ok) {
                banner.show(self.engine.errorText(error))
            }
            if (self.sendNext) {
                self.sendNext = false
                self.setMode(false)
            }
        })
    }

    /// What the Share menu handed over: to the radar's centre, in Send
    /// mode. Nothing is sent until a peer is tapped.
    function share(items) {
        payload.load(items)
        sendView.dismiss()
        if (page.switching) {
            page.sendNext = true
        } else if (page.engine.receiving) {
            page.setMode(false)
        }
    }

    function copyText(text) {
        Clipboard.text = text
        //: Shown after a received text was copied to the clipboard.
        banner.show(qsTr("Copied"), "info")
    }

    Payload {
        id: payload
    }

    Connections {
        target: page.engine
        // Qt 5.6 handler syntax.
        onFailed: banner.show(message)
    }

    SilicaFlickable {
        id: flickable
        anchors {
            left: parent.left
            right: parent.right
            top: parent.top
            bottom: modeSwitch.top
        }
        clip: true
        contentHeight: page.showSend ? flickable.height : column.height + Theme.paddingLarge

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
                visible: page.receiveMode && (page.engine.texts.count > 0
                         || page.engine.transfers.count > page.engine.activeTransfers)
                onClicked: {
                    page.engine.clearFinished()
                    page.engine.clearTexts()
                }
            }
            MenuItem {
                objectName: "receiveWithCode"
                //: Pulley menu: receive over Magic Wormhole by typing the sender's code.
                text: qsTr("Receive with a code")
                // Receive mode only, and gone with Magic Wormhole switched
                // off in Settings (F-C1).
                visible: page.receiveMode && page.engine.protocolEnabled("wormhole")
                enabled: page.engine.running
                onClicked: pageStack.push(Qt.resolvedUrl("WormholeReceivePage.qml"),
                                          { engine: page.engine })
            }
            MenuItem {
                objectName: "cancelSending"
                //: Pulley menu in Send mode: stop the send that is running.
                text: qsTr("Cancel sending")
                visible: page.showSend && sendView.outgoingState === "active"
                onClicked: sendView.cancelSend()
            }
            MenuItem {
                objectName: "choosePayload"
                //: Pulley menu in Send mode: choose the files or text to send.
                text: qsTr("Choose what to send")
                visible: page.showSend && !sendView.hasOutgoing
                onClicked: sendView.editPayload()
            }
        }

        // Send mode: the radar, as tall as the screen above the switch.
        SendView {
            id: sendView
            objectName: "sendView"
            width: flickable.width
            height: flickable.height
            visible: page.showSend
            engine: page.engine
            payload: payload
            banner: banner
            discovering: page.discovering
            pageStack: page.pageStack
        }

        // Receive mode, and anything that keeps the engine from starting.
        Column {
            id: column
            width: parent.width
            visible: !page.showSend

            PageHeader {
                title: "Sukkula"
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

            Label {
                objectName: "receiveState"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.receiveMode
                //: Receive mode, at the top: what receiving means.
                text: qsTr("Nearby devices can offer you files")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.highlightColor
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
                visible: page.receiveMode && page.engine.running && page.engine.transfers.count === 0
                         && page.engine.texts.count === 0
                horizontalAlignment: Text.AlignHCenter
                //: Receive mode with nothing received yet.
                text: qsTr("Waiting for offers. Nothing is saved until you accept it.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeLarge
                color: Theme.secondaryHighlightColor
            }
        }

        VerticalScrollDecorator {}
    }

    // Over the top of either mode: what just went wrong, or right.
    Banner {
        id: banner
        anchors.top: parent.top
        z: 1
    }

    ModeSwitch {
        id: modeSwitch
        objectName: "modeSwitch"
        anchors {
            left: parent.left
            right: parent.right
            bottom: parent.bottom
            bottomMargin: Theme.paddingSmall
        }
        height: Theme.itemSizeSmall
        receiving: page.engine.receiving
        busy: page.switching
        enabled: page.engine.running
        onChosen: page.setMode(receive)
    }
}
