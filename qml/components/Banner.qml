// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A strip saying what just happened -- a failure, or a confirmation -- that
 * clears itself after a while. Only ever shows Sukkula's own translated
 * sentences, and shows them as plain text all the same (S2).
 */
Item {
    id: banner

    property string text: ""
    /// "error" or "info".
    property string tone: "error"
    /// Seconds before it clears; 0 keeps it.
    property int timeout: 6

    function show(message, tone) {
        banner.tone = tone === "info" ? "info" : "error"
        banner.text = message
        if (banner.text.length > 0 && banner.timeout > 0) {
            fade.restart()
        }
    }

    width: parent ? parent.width : 0
    height: banner.text.length > 0 ? strip.height : 0
    visible: banner.text.length > 0

    Rectangle {
        id: strip
        width: parent.width
        height: label.implicitHeight + 2 * Theme.paddingMedium
        color: Theme.rgba(banner.tone === "error" ? Theme.errorColor : Theme.highlightColor, 0.15)

        Label {
            id: label
            objectName: "bannerLabel"
            anchors {
                left: parent.left
                right: parent.right
                leftMargin: Theme.horizontalPageMargin
                rightMargin: Theme.horizontalPageMargin
                verticalCenter: parent.verticalCenter
            }
            text: banner.text
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: banner.tone === "error" ? Theme.errorColor : Theme.highlightColor
        }
    }

    Timer {
        id: fade
        interval: banner.timeout * 1000
        onTriggered: banner.text = ""
    }
}
