// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Version, licence and the upstream projects Sukkula is built on. Plain
 * text throughout, and no links: nothing in this app opens a URL (S8).
 */
Page {
    id: page
    objectName: "aboutPage"

    property QtObject engine

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width
            spacing: Theme.paddingMedium

            PageHeader {
                //: Page title.
                title: qsTr("About Sukkula")
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: About page: what the app does.
                text: qsTr("Sends and receives files and texts over LocalSend, Quick Share, Magic Wormhole and Bluetooth.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.highlightColor
            }

            Label {
                objectName: "aboutVersion"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: About page: %1 is the version number.
                text: qsTr("Version %1").arg(page.engine.version.length > 0 ? page.engine.version : "–")
                textFormat: Text.PlainText
                color: Theme.secondaryHighlightColor
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: About page: the licence. Keep the SPDX identifier as it is.
                text: qsTr("Free software under the GNU General Public License, version 3 or later (GPL-3.0-or-later).")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }

            SectionHeader {
                //: About page: section of the upstream projects.
                text: qsTr("Built on")
            }

            Repeater {
                model: [
                    "LocalSend core – Apache-2.0",
                    "open-quickshare (rqs_lib) – GPL-3.0",
                    "magic-wormhole.rs – EUPL-1.2",
                    "dbus-rs – MIT / Apache-2.0",
                    "tokio, serde – MIT / Apache-2.0",
                    "Qt – LGPL-3.0, Sailfish Silica"
                ]
                delegate: Label {
                    x: Theme.horizontalPageMargin
                    width: column.width - 2 * Theme.horizontalPageMargin
                    text: modelData
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryColor
                }
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: About page: the name.
                text: qsTr("Sukkula is Finnish for “shuttle”: it carries things back and forth.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
        }

        VerticalScrollDecorator {}
    }
}
