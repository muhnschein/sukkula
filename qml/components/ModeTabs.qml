// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Send | Receive across the top of the main page, the strip the Clock and
 * Settings apps wear: the words side by side in the middle of the screen,
 * the one on screen in the highlight colour with a thin line under it
 * that slides across when the other is chosen.
 *
 * Silica's own TabBar is in Sailfish.Silica.private, which Harbour does
 * not allow, and works only inside a TabView; so this is rebuilt from
 * Sailfish.Silica 1.0 with TabBar's geometry, as Vuo's ScopeTabBar does:
 * each tab is its word plus Theme.paddingLarge either side, and the slack
 * left over goes to the outer edges of the first and last tab, whose words
 * hug the inside -- so the words sit together in the middle rather than
 * each in the middle of its half. A strip too wide for the screen drops
 * one font size, as TabBar does.
 *
 * It is not a view: it shows `currentIndex` and says which tab was
 * tapped, and the page owns the mode. It paints nothing behind the words:
 * the page clips its lists below it instead (MainPage.qml).
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

    // Laid out by hand rather than by a Row, whose positions only follow
    // a width change at the next frame: each tab starts where the one
    // before it ends.
    Item {
        id: row
        y: strip.topMargin
        width: strip.width
        height: (tabs.count, tabs.count > 0 && tabs.itemAt(0) ? tabs.itemAt(0).height : 0)

        /// The slack either side of the words, given to the outer tabs.
        /// Read off `tabContentWidth`, which does not depend on it.
        readonly property real extraMargin: {
            var total = 0
            for (var i = 0; i < row.children.length; i++) {
                total += row.children[i].tabContentWidth || 0
            }
            return Math.max(0, strip.width - total) / 2
        }
        /// One size down when the words do not fit. Measured, not read
        /// back off the tabs, whose widths depend on it.
        readonly property real titleFontSize: {
            var total = 0
            for (var i = 0; i < strip.titles.length; i++) {
                total += largeMetrics.advanceWidth(strip.titles[i]) + 2 * Theme.paddingLarge
            }
            return total > strip.width ? Theme.fontSizeMedium : Theme.fontSizeLarge
        }

        Repeater {
            id: tabs
            model: strip.titles.length

            BackgroundItem {
                id: button
                objectName: index === 0 ? "modeSend" : "modeReceive"
                readonly property bool current: index === strip.currentIndex
                readonly property bool first: index === 0 && tabs.count > 1
                readonly property bool last: index === tabs.count - 1 && tabs.count > 1
                readonly property real tabContentWidth: label.implicitWidth + 2 * Theme.paddingLarge
                readonly property Item before: index > 0 ? (tabs.count, tabs.itemAt(index - 1)) : null
                property Item labelItem: label

                x: button.before ? button.before.x + button.before.width : 0
                width: button.tabContentWidth + (index === 0 ? row.extraMargin : 0)
                       + (index === tabs.count - 1 ? row.extraMargin : 0)
                height: Math.max(strip.portrait ? Theme.itemSizeLarge : Theme.itemSizeSmall,
                                 label.implicitHeight
                                 + 2 * (strip.portrait ? Theme.paddingLarge : Theme.paddingMedium))

                onClicked: {
                    strip.animate = true
                    strip.tabClicked(index)
                }

                Label {
                    id: label
                    // The outer tabs' words hug the inside edge.
                    x: button.first ? button.width - label.width - Theme.paddingMedium
                       : button.last ? Theme.paddingMedium
                       : (button.width - label.width) / 2
                    anchors.verticalCenter: parent.verticalCenter
                    text: strip.titles[index]
                    textFormat: Text.PlainText
                    font.pixelSize: row.titleFontSize
                    color: button.highlighted || button.current ? Theme.highlightColor : Theme.primaryColor
                }

                // Outside the word, away from the other tab.
                BusyIndicator {
                    objectName: "modeBusy"
                    x: button.first ? label.x - width - Theme.paddingMedium
                                    : label.x + label.width + Theme.paddingMedium
                    anchors.verticalCenter: label.verticalCenter
                    size: BusyIndicatorSize.ExtraSmall
                    running: strip.busyIndex === index
                    visible: running
                }
            }
        }
    }

    // As wide as the tab's word, a hairline under it.
    Rectangle {
        id: underline
        objectName: "modeUnderline"
        readonly property Item label: strip.currentButton ? strip.currentButton.labelItem : null
        x: underline.label ? row.x + strip.currentButton.x + underline.label.x : 0
        y: underline.label ? row.y + underline.label.y + underline.label.height + Theme.paddingMedium : 0
        width: underline.label ? underline.label.width : 0
        height: Theme._lineWidth
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

    FontMetrics {
        id: largeMetrics
        font.pixelSize: Theme.fontSizeLarge
    }
}
