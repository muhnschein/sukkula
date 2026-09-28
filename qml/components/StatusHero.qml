// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The top of the Send tab, the Receive tab and the code page (spec v0.7):
 * the anchor, then a title, a grey line, a line of text, and the buttons
 * for the step, in that order and at the same place on each.
 *
 * The anchor sits at a fifth of the page's height from its top, whatever
 * is above it -- the tabs, or a page header -- so it stays put when the
 * page changes as well as when the step does. Each line keeps its height
 * while it is empty, so what comes under the hero moves only when a long
 * title or a long reason takes a second line.
 *
 * The title and the lines may be a peer's words (a device's name) and
 * are shown only in this file's plain-text labels (S2).
 */
Item {
    id: hero

    /// How far down the page the hero starts: the tabs', or the header's,
    /// height.
    property real topInset: 0
    /// The page's height, which places the anchor.
    property real pageHeight: 0

    property alias mode: anchorItem.mode
    property alias glyph: anchorItem.glyph
    property alias value: anchorItem.value
    property alias running: anchorItem.running
    /// The anchor gives its place to something else: the code page's QR
    /// code, in `face`.
    property bool faceShown: false

    property string title: ""
    /// The step went wrong: the title and the grey line in red.
    property bool failed: false
    /// The title may break anywhere, not only between words: a code.
    property bool titleWraps: false
    property string subtitle: ""
    property string line: ""

    /// Where the anchor is, for what takes its place.
    readonly property alias face: anchorArea
    readonly property real anchorTop: Math.max(Theme.paddingLarge,
                                               Math.round(hero.pageHeight * 0.2) - hero.topInset)
    /// The buttons for the step.
    default property alias buttons: slot.data
    /// The width for each of two buttons side by side.
    readonly property real pairWidth: (hero.width - 2 * Theme.horizontalPageMargin - slot.spacing) / 2

    width: parent ? parent.width : Screen.width
    height: column.height + Theme.paddingLarge

    FontMetrics {
        id: small
        font.pixelSize: Theme.fontSizeSmall
    }
    FontMetrics {
        id: medium
        font.pixelSize: Theme.fontSizeMedium
    }

    Column {
        id: column
        width: parent.width
        spacing: Theme.paddingSmall

        Item {
            width: 1
            height: hero.anchorTop - column.spacing
        }

        Item {
            id: anchorArea
            objectName: "anchorArea"
            anchors.horizontalCenter: parent.horizontalCenter
            width: Math.min(anchorItem.implicitWidth, (hero.width - 2 * Theme.horizontalPageMargin) / 1.15)
            height: width

            Anchor {
                id: anchorItem
                objectName: "anchor"
                anchors.fill: parent
                visible: !hero.faceShown
            }
        }

        Item {
            width: 1
            height: Theme.paddingMedium
        }

        Label {
            objectName: "heroTitle"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            horizontalAlignment: Text.AlignHCenter
            text: hero.title
            textFormat: Text.PlainText
            wrapMode: hero.titleWraps ? Text.WrapAnywhere : Text.Wrap
            maximumLineCount: 2
            truncationMode: TruncationMode.Elide
            font.pixelSize: Theme.fontSizeExtraLarge
            color: hero.failed ? Theme.errorColor : Theme.highlightColor
        }
        Label {
            objectName: "heroSubtitle"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            height: Math.max(implicitHeight, small.height)
            horizontalAlignment: Text.AlignHCenter
            text: hero.subtitle
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: hero.failed ? Theme.errorColor : Theme.secondaryColor
        }
        Label {
            objectName: "heroLine"
            x: Theme.horizontalPageMargin
            width: parent.width - 2 * Theme.horizontalPageMargin
            height: medium.height
            horizontalAlignment: Text.AlignHCenter
            text: hero.line
            textFormat: Text.PlainText
            truncationMode: TruncationMode.Fade
            font.pixelSize: Theme.fontSizeMedium
        }

        Item {
            width: 1
            height: Theme.paddingMedium
        }

        // The buttons, or whatever the step puts here instead; the room
        // is kept while there is none.
        Item {
            width: parent.width
            height: Math.max(Theme.itemSizeExtraSmall, slot.height)

            Row {
                id: slot
                objectName: "heroButtons"
                anchors.horizontalCenter: parent.horizontalCenter
                spacing: Theme.paddingLarge
            }
        }
    }
}
