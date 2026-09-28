// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * About this device: what Sukkula knows of a device on the Send tab, for
 * whoever wants to be sure where their files go. One section per way it
 * was found -- Quick Share, LocalSend, Bluetooth -- with what that way
 * says: its kind, its model, where it is, and for LocalSend the
 * certificate every send to it is pinned to.
 *
 * The name and the model are the device's own words (after S2) and are
 * shown only in this page's plain-text labels, never in its header.
 */
Page {
    id: page
    objectName: "devicePage"

    property QtObject engine
    /// SendView's device: {name, deviceType, found, peers: [{protocol,
    /// deviceModel, deviceType, address, fingerprint}]}.
    property var device: null

    readonly property var peers: page.device ? page.device.peers : []
    readonly property bool hasLocalSend: {
        for (var i = 0; i < page.peers.length; i++) {
            if (page.peers[i].protocol === "local_send") {
                return true
            }
        }
        return false
    }

    /// "3FA2 910C …": a fingerprint in groups of four.
    function grouped(hex) {
        var out = []
        for (var i = 0; i < hex.length; i += 4) {
            out.push(hex.substring(i, i + 4))
        }
        return out.join(" ")
    }

    /// What one way of finding the device says, as [{label, value}].
    function details(peer) {
        var out = []
        if (peer.deviceModel.length > 0) {
            //: About this device: the model it says it is.
            out.push({ label: qsTr("Model"), value: peer.deviceModel })
        }
        var kind = page.engine.deviceTypeText(peer.deviceType)
        if (kind.length > 0) {
            //: About this device: phone, tablet or computer, as it says.
            out.push({ label: qsTr("Type"), value: kind })
        }
        if (peer.address.length > 0) {
            //: About this device: its network address, or its Bluetooth address.
            out.push({ label: qsTr("Address"), value: peer.address })
        }
        if (peer.fingerprint.length > 0) {
            //: About this device: the fingerprint of its LocalSend certificate.
            out.push({ label: qsTr("Certificate"), value: page.grouped(peer.fingerprint) })
        }
        return out
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width

            PageHeader {
                //: Page title: what Sukkula knows about a device to send to.
                title: qsTr("About this device")
            }

            Item {
                width: parent.width
                height: Math.max(icon.height, nameColumn.height) + Theme.paddingLarge

                Glyph {
                    id: icon
                    x: Theme.horizontalPageMargin
                    kind: !page.device || !page.device.found ? "bluetooth"
                          : page.device.deviceType === "computer" ? "computer"
                          : page.device.deviceType === "tablet" ? "tablet" : "phone"
                    color: Theme.highlightColor
                }
                Column {
                    id: nameColumn
                    anchors {
                        left: icon.right
                        leftMargin: Theme.paddingLarge
                        right: parent.right
                        rightMargin: Theme.horizontalPageMargin
                        verticalCenter: icon.verticalCenter
                    }
                    Label {
                        objectName: "deviceName"
                        width: parent.width
                        text: page.device ? page.device.name : ""
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        maximumLineCount: 2
                        elide: Text.ElideRight
                        font.pixelSize: Theme.fontSizeExtraLarge
                        color: Theme.highlightColor
                    }
                }
            }

            Repeater {
                model: page.peers
                delegate: Column {
                    width: column.width

                    SectionHeader {
                        text: page.engine.protocolName(modelData.protocol)
                    }
                    Repeater {
                        model: page.details(modelData)
                        delegate: Item {
                            width: column.width
                            height: Math.max(labelText.height, valueText.height) + Theme.paddingSmall

                            Label {
                                id: labelText
                                anchors {
                                    left: parent.left
                                    leftMargin: Theme.horizontalPageMargin
                                    right: parent.horizontalCenter
                                    rightMargin: Theme.paddingSmall
                                }
                                horizontalAlignment: Text.AlignRight
                                text: modelData.label
                                textFormat: Text.PlainText
                                wrapMode: Text.Wrap
                                font.pixelSize: Theme.fontSizeSmall
                                color: Theme.secondaryColor
                            }
                            Label {
                                id: valueText
                                objectName: "detailValue"
                                anchors {
                                    left: parent.horizontalCenter
                                    leftMargin: Theme.paddingSmall
                                    right: parent.right
                                    rightMargin: Theme.horizontalPageMargin
                                }
                                text: modelData.value
                                textFormat: Text.PlainText
                                wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                                font.pixelSize: Theme.fontSizeSmall
                                color: Theme.highlightColor
                            }
                        }
                    }
                }
            }

            Label {
                objectName: "deviceNote"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                topPadding: Theme.paddingLarge
                text: page.hasLocalSend
                      //: About this device, at the foot, when it was found over LocalSend.
                      ? qsTr("Sukkula checks this certificate before every send over LocalSend, and stops if it has changed. The name and model are reported by the device itself.")
                      //: About this device, at the foot.
                      : qsTr("The name and model are reported by the device itself.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
        }

        VerticalScrollDecorator {}
    }
}
