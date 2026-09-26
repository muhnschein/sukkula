// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Send | Receive across the top of the main page, the strip the Clock and
 * Settings apps wear: the tab on screen underlined in the highlight
 * colour, sliding across when the other is chosen.
 *
 * Silica's own TabBar is in Sailfish.Silica.private, which Harbour does
 * not allow, and works only inside a TabView; so this is rebuilt from
 * Sailfish.Silica 1.0, with Silica's metrics, as Vuo's ScopeTabBar does.
 * It is not a view: it shows `currentIndex` and says which tab was
 * tapped, and the page owns the mode.
 */
Item {
    id: strip

    /// Translated constants only.
    property var titles: []
    property int currentIndex: 0
    /// The tab being switched to shows a spinner until the engine answers.
    property int busyIndex: -1
    /// The host page, for orientation; may be null.
    property Item hostPage: null

    signal tabClicked(int index)

    readonly property bool portrait: strip.hostPage ? strip.hostPage.isPortrait === true : true
    /// Clear of a display cutout, as PageHeader and TabBar are.
    readonly property real topMargin: strip.portrait && Screen.topCutout.height > 0
                                      ? Math.max(0, Screen.topCutout.height - Theme.paddingLarge) : 0
    readonly property Item currentButton: strip.currentIndex >= 0 && strip.currentIndex < tabs.count
                                          ? (row.children, tabs.itemAt(strip.currentIndex)) : null
    /// Set on the first tap: the underline snaps into place on first
    /// layout and slides only when moved.
    property bool animate: false

    implicitHeight: strip.topMargin + row.height

    Row {
        id: row
        y: strip.topMargin
        anchors.horizontalCenter: parent.horizontalCenter

        Repeater {
            id: tabs
            model: strip.titles.length

            BackgroundItem {
                id: button
                objectName: index === 0 ? "modeSend" : "modeReceive"
                readonly property bool current: index === strip.currentIndex
                property Item labelItem: label

                width: Math.max(strip.width / Math.max(1, strip.titles.length),
                                label.implicitWidth + 2 * Theme.paddingLarge)
                height: Math.max(strip.portrait ? Theme.itemSizeLarge : Theme.itemSizeSmall,
                                 label.implicitHeight
                                 + 2 * (strip.portrait ? Theme.paddingLarge : Theme.paddingMedium))

                onClicked: {
                    strip.animate = true
                    strip.tabClicked(index)
                }

                Label {
                    id: label
                    anchors.centerIn: parent
                    text: strip.titles[index]
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeLarge
                    color: button.highlighted || button.current ? Theme.highlightColor : Theme.primaryColor
                }

                BusyIndicator {
                    anchors {
                        left: label.right
                        leftMargin: Theme.paddingMedium
                        verticalCenter: label.verticalCenter
                    }
                    size: BusyIndicatorSize.ExtraSmall
                    running: strip.busyIndex === index
                    visible: running
                }
            }
        }
    }

    // As wide as the tab's word, under it.
    Rectangle {
        id: underline
        objectName: "modeUnderline"
        readonly property Item label: strip.currentButton ? strip.currentButton.labelItem : null
        x: underline.label ? row.x + strip.currentButton.x + underline.label.x : 0
        y: underline.label ? row.y + underline.label.y + underline.label.height + Theme.paddingMedium : 0
        width: underline.label ? underline.label.width : 0
        height: Math.max(2, Math.round(Theme.paddingSmall / 3))
        color: Theme.highlightColor

        Behavior on x {
            enabled: strip.animate
            SmoothedAnimation { duration: 200; easing.type: Easing.InOutQuad }
        }
        Behavior on width {
            enabled: strip.animate
            SmoothedAnimation { duration: 200; easing.type: Easing.InOutQuad }
        }
    }
}
