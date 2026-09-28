// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Files that came together, from the Receive tab's "Received today": who
 * sent them, each file by the name it was saved under, and where they
 * are. Nothing here opens a file or draws one: a file's kind is told by
 * its name alone (S2, S8).
 *
 * The header says how many, in Sukkula's words only; the sender's and the
 * files' names are the peer's (after S1, S2) and are shown only in this
 * page's plain-text labels.
 */
Page {
    id: page
    objectName: "receivedPage"

    property QtObject engine
    property var transferId: -1

    readonly property var transfer: page.engine.transfer(page.transferId)
    readonly property var names: !page.transfer ? []
                                 : page.transfer.savedCount > 0 ? page.transfer.saved.split("\n")
                                 : page.transfer.files.length > 0 ? page.transfer.files.split("\n") : []
    /// The offered sizes, where they line up with the names.
    readonly property var sizes: {
        if (!page.transfer || page.transfer.sizes.length === 0) {
            return []
        }
        var list = page.transfer.sizes.split("\n")
        return list.length === page.names.length ? list : []
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width

            PageHeader {
                title: page.engine.countWords(page.engine.commonKind(page.names, page.names.length), page.names.length)
            }

            Label {
                objectName: "receivedFrom"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                horizontalAlignment: Text.AlignRight
                //: A page of received files, under its title: who sent them, and how much; %1 is the formatted size, %2 the sender's name.
                text: page.transfer ? qsTr("From %2 · %1").arg(Format.formatFileSize(page.transfer.total))
                                                          .arg(page.transfer.peer) : ""
                textFormat: Text.PlainText
                truncationMode: TruncationMode.Fade
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }

            Item {
                width: 1
                height: Theme.paddingLarge
            }

            Repeater {
                model: page.names
                delegate: Item {
                    width: column.width
                    height: Theme.itemSizeMedium

                    Glyph {
                        id: icon
                        x: Theme.horizontalPageMargin
                        anchors.verticalCenter: parent.verticalCenter
                        kind: page.engine.kindOf(modelData)
                        color: Theme.primaryColor
                    }
                    Column {
                        anchors {
                            left: icon.right
                            leftMargin: Theme.paddingLarge
                            right: parent.right
                            rightMargin: Theme.horizontalPageMargin
                            verticalCenter: parent.verticalCenter
                        }
                        Label {
                            objectName: "receivedName"
                            width: parent.width
                            text: modelData
                            textFormat: Text.PlainText
                            // The middle goes, so both the start and the
                            // extension stay visible.
                            elide: Text.ElideMiddle
                        }
                        Label {
                            width: parent.width
                            visible: page.sizes.length > 0
                            text: page.sizes.length > 0 ? Format.formatFileSize(Number(page.sizes[index])) : ""
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.secondaryColor
                        }
                    }
                }
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                topPadding: Theme.paddingLarge
                //: Where received files are.
                text: qsTr("Saved in Downloads › Sukkula")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
        }

        VerticalScrollDecorator {}
    }
}
