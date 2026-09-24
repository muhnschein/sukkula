// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * One received text in full (F-C4): plain text, a Copy button, and
 * nothing that opens a link in it (S2, S8).
 */
Page {
    id: page
    objectName: "textPage"

    /// The sender and the text, after S2.
    property string from: ""
    property string text: ""

    function copy() {
        Clipboard.text = page.text
        //: Shown after a received text was copied to the clipboard.
        banner.show(qsTr("Copied"), "info")
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        PullDownMenu {
            MenuItem {
                //: Copies a received text to the clipboard.
                text: qsTr("Copy")
                onClicked: page.copy()
            }
        }

        Column {
            id: column
            width: parent.width
            spacing: Theme.paddingMedium

            PageHeader {
                //: Page title: a text another device sent.
                title: qsTr("Received text")
            }

            Banner {
                id: banner
            }

            Label {
                objectName: "textPageFrom"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                text: page.from
                textFormat: Text.PlainText
                truncationMode: TruncationMode.Fade
                color: Theme.secondaryHighlightColor
            }

            Label {
                objectName: "textPageBody"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                text: page.text
                textFormat: Text.PlainText
                wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                color: Theme.highlightColor
            }

            Button {
                anchors.horizontalCenter: parent.horizontalCenter
                //: Copies a received text to the clipboard.
                text: qsTr("Copy")
                onClicked: page.copy()
            }
        }

        VerticalScrollDecorator {}
    }
}
