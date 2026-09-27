// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One row of the Send and Receive tabs: a line drawing, a title and a
 * line under it, as Silica's own lists lay them out.
 *
 * The title and the line may be a peer's words (a device's name, a file's
 * name) and are shown only in this file's plain-text labels (S2).
 */
ListItem {
    id: item

    /// Glyph.qml's kind; "" for none.
    property string glyph: ""
    property string title: ""
    property string subtitle: ""
    /// The subtitle says something went wrong.
    property bool failed: false
    /// Greyed out and not tappable, while something else goes on.
    property bool dimmed: false

    width: parent ? parent.width : Screen.width
    contentHeight: Theme.itemSizeMedium
    enabled: !item.dimmed
    opacity: item.dimmed ? Theme.opacityLow : 1

    Glyph {
        id: icon
        objectName: "rowGlyph"
        x: Theme.horizontalPageMargin
        anchors.verticalCenter: parent.verticalCenter
        width: item.glyph !== "" ? Theme.iconSizeMedium : 0
        height: width
        visible: item.glyph !== ""
        kind: item.glyph !== "" ? item.glyph : "file"
        color: item.highlighted ? Theme.highlightColor : Theme.primaryColor
    }

    Column {
        anchors {
            left: icon.right
            leftMargin: item.glyph !== "" ? Theme.paddingLarge : 0
            right: parent.right
            rightMargin: Theme.horizontalPageMargin
            verticalCenter: parent.verticalCenter
        }

        Label {
            objectName: "rowTitle"
            width: parent.width
            text: item.title
            textFormat: Text.PlainText
            truncationMode: TruncationMode.Fade
            color: item.highlighted ? Theme.highlightColor : Theme.primaryColor
        }
        Label {
            objectName: "rowSubtitle"
            width: parent.width
            visible: text.length > 0
            text: item.subtitle
            textFormat: Text.PlainText
            truncationMode: TruncationMode.Fade
            font.pixelSize: Theme.fontSizeSmall
            color: item.failed ? Theme.errorColor
                               : item.highlighted ? Theme.secondaryHighlightColor : Theme.secondaryColor
        }
    }
}
