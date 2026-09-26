// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Send | Receive at the foot of the main page (F-C1). The mode is the
 * engine's state, not a view: Receive switches every enabled receiver on,
 * Send switches them off and looks for peers instead. The switch shows
 * what the engine reports, not what was tapped; `busy` says a change is
 * on its way.
 */
Item {
    id: modes

    /// What the engine reports.
    property bool receiving: false
    property bool busy: false

    /// A tap on the mode that is not the current one.
    signal chosen(bool receive)

    implicitHeight: Theme.itemSizeSmall

    Rectangle {
        id: frame
        anchors.centerIn: parent
        width: Math.min(parent.width - 2 * Theme.horizontalPageMargin, Theme.itemSizeExtraLarge * 3.5)
        height: parent.height - Theme.paddingMedium
        radius: Theme.paddingSmall
        color: "transparent"
        border.width: 2
        border.color: Theme.rgba(Theme.primaryColor, modes.enabled ? 0.6 : 0.3)

        Row {
            anchors {
                fill: parent
                margins: Theme.paddingSmall
            }

            Repeater {
                model: 2
                delegate: Item {
                    id: segment
                    objectName: index === 0 ? "modeSend" : "modeReceive"

                    readonly property bool receive: index === 1
                    readonly property bool selected: segment.receive === modes.receiving

                    signal clicked()

                    onClicked: {
                        if (modes.enabled && !modes.busy && !segment.selected) {
                            modes.chosen(segment.receive)
                        }
                    }

                    width: parent.width / 2
                    height: parent.height

                    Rectangle {
                        anchors.fill: parent
                        radius: Theme.paddingSmall / 2
                        visible: segment.selected || area.pressed
                        color: Theme.rgba(Theme.highlightBackgroundColor, segment.selected ? 0.4 : 0.2)
                        border.width: segment.selected ? 2 : 0
                        border.color: Theme.highlightColor
                    }

                    Label {
                        anchors.centerIn: parent
                        text: segment.receive
                              //: The mode switch at the foot of the main page: receive files.
                              ? qsTr("Receive")
                              //: The mode switch at the foot of the main page: send files.
                              : qsTr("Send")
                        textFormat: Text.PlainText
                        font.capitalization: Font.AllUppercase
                        font.pixelSize: Theme.fontSizeSmall
                        color: segment.selected ? Theme.highlightColor : Theme.primaryColor
                        opacity: modes.busy && !segment.selected ? 0.3 : 1
                    }

                    BusyIndicator {
                        anchors.centerIn: parent
                        size: BusyIndicatorSize.Small
                        running: modes.busy && !segment.selected
                        visible: running
                    }

                    MouseArea {
                        id: area
                        anchors.fill: parent
                        onClicked: segment.clicked()
                    }
                }
            }
        }
    }
}
