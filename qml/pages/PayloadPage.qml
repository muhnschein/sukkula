// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * What to send (F-C6): the files and texts at the send radar's centre.
 *
 * Files come from the Share menu or the file picker, text from the Share
 * menu or typed here. A shared text is shown in this page's own plain-text
 * labels rather than put into the text box, whose rendering is Silica's: a
 * shared text can be anything, markup included (S2).
 */
Page {
    id: page
    objectName: "payloadPage"

    /// A Payload, the main page's.
    property QtObject payload

    // ~/Downloads, the one folder Sailjail lets Sukkula read (spec §2).
    readonly property string downloads: StandardPaths.download

    function pickFile() {
        var picker = pageStack.push(Qt.resolvedUrl("FilePicker.qml"))
        if (picker) {
            picker.picked.connect(page.payload.addFile)
        }
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        PullDownMenu {
            MenuItem {
                //: Pulley menu on the "What to send" page: take everything off it.
                text: qsTr("Clear")
                visible: page.payload.itemCount > 0
                onClicked: {
                    page.payload.clear()
                    textArea.text = ""
                }
            }
            MenuItem {
                //: Pulley menu on the "What to send" page: choose a file with the file picker.
                text: qsTr("Add file")
                enabled: page.payload.files.length < page.payload.maxFiles
                onClicked: page.pickFile()
            }
        }

        Column {
            id: column
            width: parent.width

            PageHeader {
                //: Page title: choose the files and text to send.
                title: qsTr("What to send")
            }

            Repeater {
                model: page.payload.files
                delegate: ListItem {
                    id: fileItem
                    width: column.width
                    contentHeight: Math.max(Theme.itemSizeSmall, fileColumn.height + 2 * Theme.paddingSmall)

                    // Sailjail grants Downloads only (spec §2): a file from
                    // anywhere else -- a picture shared from the gallery --
                    // may be out of the app's reach. Said up front; the
                    // engine's bad_file says it again if so.
                    readonly property bool outside: page.downloads.length > 0
                        && modelData.path.indexOf(page.downloads + "/") !== 0

                    Column {
                        id: fileColumn
                        anchors {
                            left: parent.left
                            leftMargin: Theme.horizontalPageMargin
                            right: removeButton.left
                            verticalCenter: parent.verticalCenter
                        }

                        Label {
                            objectName: "sendFileName"
                            width: parent.width
                            text: modelData.name
                            textFormat: Text.PlainText
                            elide: Text.ElideMiddle
                        }
                        Label {
                            objectName: "sendFileOutside"
                            width: parent.width
                            visible: fileItem.outside
                            //: "What to send" page: a file outside ~/Downloads, which the sandbox may not let Sukkula read.
                            text: qsTr("Outside Downloads: Sukkula may not be allowed to read it.")
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            font.pixelSize: Theme.fontSizeExtraSmall
                            color: Theme.secondaryHighlightColor
                        }
                    }
                    IconButton {
                        id: removeButton
                        objectName: "removeFile"
                        anchors {
                            right: parent.right
                            rightMargin: Theme.paddingSmall
                            verticalCenter: parent.verticalCenter
                        }
                        icon.source: "image://theme/icon-m-clear"
                        onClicked: page.payload.removeFile(index)
                    }
                }
            }

            Repeater {
                model: page.payload.texts
                delegate: ListItem {
                    width: column.width
                    contentHeight: sharedText.height + 2 * Theme.paddingMedium

                    Label {
                        id: sharedText
                        objectName: "sharedText"
                        anchors {
                            left: parent.left
                            leftMargin: Theme.horizontalPageMargin
                            right: removeTextButton.left
                            verticalCenter: parent.verticalCenter
                        }
                        text: modelData
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        maximumLineCount: 3
                        elide: Text.ElideRight
                        font.pixelSize: Theme.fontSizeSmall
                    }
                    IconButton {
                        id: removeTextButton
                        objectName: "removeText"
                        anchors {
                            right: parent.right
                            rightMargin: Theme.paddingSmall
                            verticalCenter: parent.verticalCenter
                        }
                        icon.source: "image://theme/icon-m-clear"
                        onClicked: page.payload.removeText(index)
                    }
                }
            }

            Button {
                objectName: "addFile"
                x: Theme.horizontalPageMargin
                //: Opens the file picker.
                text: qsTr("Add file")
                enabled: page.payload.files.length < page.payload.maxFiles
                onClicked: page.pickFile()
            }

            TextArea {
                id: textArea
                objectName: "sendText"
                width: parent.width
                //: Placeholder of the text box on the "What to send" page.
                placeholderText: qsTr("Text to send (optional)")
                //: Label of the text box on the "What to send" page.
                label: qsTr("Text")
                // Set once, then written back: a binding would be broken
                // by the first key press anyway.
                Component.onCompleted: textArea.text = page.payload.typed
                onTextChanged: page.payload.typed = textArea.text
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: "What to send" page: how to go on once something is chosen.
                text: qsTr("Then go back and tap who to send it to.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryHighlightColor
            }
        }

        VerticalScrollDecorator {}
    }
}
