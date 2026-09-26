// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Every peer the send radar knows, as a list: for when there are more
 * than its rings hold. Tapping one sends to it, as on the radar.
 *
 * Peer names and models are the peers' own (after S2) and are shown only
 * in this page's plain-text labels.
 */
Page {
    id: page
    objectName: "peerListPage"

    property QtObject engine
    /// The SendView to send with.
    property Item view

    function choose(peer) {
        var view = page.view
        pageStack.pop()
        if (view) {
            view.choose(peer, view.slots.indexOf(peer.key))
        }
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width

            PageHeader {
                //: Page title: every device the send screen found.
                title: qsTr("Devices nearby")
            }

            Repeater {
                model: [
                    { protocol: "local_send", rows: page.engine.localSendPeers },
                    { protocol: "quick_share", rows: page.engine.quickSharePeers },
                    { protocol: "bluetooth", rows: page.engine.bluetoothDevices }
                ]
                delegate: Column {
                    id: group
                    width: column.width
                    visible: page.engine.protocolEnabled(modelData.protocol)

                    readonly property string protocol: modelData.protocol

                    Repeater {
                        model: group.visible ? modelData.rows : null
                        delegate: BackgroundItem {
                            id: row
                            objectName: "peerRow"
                            width: column.width
                            height: Theme.itemSizeMedium
                            onClicked: page.choose(group.protocol === "bluetooth"
                                                   ? page.view.bluetoothPeer(model)
                                                   : page.view.lanPeer(model))

                            PeerBubble {
                                id: avatar
                                anchors {
                                    left: parent.left
                                    leftMargin: Theme.horizontalPageMargin
                                    verticalCenter: parent.verticalCenter
                                }
                                size: Theme.itemSizeSmall * 0.8
                                showName: false
                                protocol: group.protocol
                                enabled: false
                            }

                            Column {
                                anchors {
                                    left: avatar.right
                                    right: parent.right
                                    leftMargin: Theme.paddingLarge
                                    rightMargin: Theme.horizontalPageMargin
                                    verticalCenter: parent.verticalCenter
                                }
                                Label {
                                    objectName: "peerRowName"
                                    width: parent.width
                                    text: group.protocol === "bluetooth" && model.name.length === 0
                                          ? model.address : model.name
                                    textFormat: Text.PlainText
                                    truncationMode: TruncationMode.Fade
                                    color: row.highlighted ? Theme.highlightColor : Theme.primaryColor
                                }
                                Label {
                                    objectName: "peerRowDetail"
                                    readonly property string detail: group.protocol === "bluetooth"
                                                                     ? model.address : model.deviceModel
                                    width: parent.width
                                    text: detail.length === 0 ? page.engine.protocolName(group.protocol)
                                          //: A peer in the device list: its protocol (%1), then its model or address (%2).
                                          : qsTr("%1 · %2").arg(page.engine.protocolName(group.protocol)).arg(detail)
                                    textFormat: Text.PlainText
                                    truncationMode: TruncationMode.Fade
                                    font.pixelSize: Theme.fontSizeExtraSmall
                                    color: Theme.secondaryColor
                                }
                            }
                        }
                    }
                }
            }
        }

        VerticalScrollDecorator {}
    }
}
