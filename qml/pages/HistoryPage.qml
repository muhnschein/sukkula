// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * What was sent and received this session: every transfer, with its
 * progress and a way to cancel one still running (F-C5), and the texts
 * other devices sent, each with a Copy button (F-C4).
 *
 * Received texts are shown, never opened: no link in one is clickable,
 * nothing here calls Qt.openUrlExternally, and every label that shows what
 * a peer sent is plain text (S2, S8).
 */
Page {
    id: page
    objectName: "historyPage"

    property QtObject engine

    readonly property bool empty: page.engine.transfers.count === 0 && page.engine.texts.count === 0
    /// Something has finished, to be taken off.
    readonly property bool clearable: page.engine.texts.count > 0
                                      || page.engine.transfers.count > page.engine.activeTransfers

    function copyText(text) {
        Clipboard.text = text
        //: Shown after a received text was copied to the clipboard.
        banner.show(qsTr("Copied"), "info")
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        PullDownMenu {
            visible: page.clearable
            MenuItem {
                objectName: "clearHistory"
                //: Pulley menu: take finished transfers and received texts off the list.
                text: qsTr("Clear list")
                visible: page.clearable
                onClicked: {
                    page.engine.clearFinished()
                    page.engine.clearTexts()
                }
            }
        }

        Column {
            id: column
            width: parent.width

            PageHeader {
                //: Page title: what was sent and received.
                title: qsTr("History")
            }

            SectionHeader {
                //: Section heading over the list of transfers.
                text: qsTr("Transfers")
                visible: page.engine.transfers.count > 0
            }

            Repeater {
                model: page.engine.transfers
                delegate: TransferItem {
                    width: column.width
                    engine: page.engine
                    transferId: model.transferId
                    direction: model.direction
                    protocol: model.protocol
                    peer: model.peer
                    files: model.files
                    fileCount: model.fileCount
                    total: model.total
                    bytes: model.bytes
                    state: model.state
                    error: model.error
                    saved: model.saved
                    savedCount: model.savedCount
                    onCancelRequested: page.engine.cancel(transferId)
                }
            }

            SectionHeader {
                //: Section heading over texts other devices sent.
                text: qsTr("Received texts")
                visible: page.engine.texts.count > 0
            }

            Repeater {
                model: page.engine.texts
                delegate: ListItem {
                    id: textItem
                    width: column.width
                    contentHeight: textColumn.height + 2 * Theme.paddingMedium
                    onClicked: pageStack.push(Qt.resolvedUrl("TextPage.qml"),
                                              { from: model.from, text: model.text })

                    Column {
                        id: textColumn
                        x: Theme.horizontalPageMargin
                        y: Theme.paddingMedium
                        width: parent.width - 2 * Theme.horizontalPageMargin
                        spacing: Theme.paddingSmall

                        Label {
                            objectName: "textFrom"
                            width: parent.width
                            text: model.from
                            textFormat: Text.PlainText
                            truncationMode: TruncationMode.Fade
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.secondaryHighlightColor
                        }
                        Label {
                            objectName: "textBody"
                            width: parent.width
                            text: model.text
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            maximumLineCount: 4
                            elide: Text.ElideRight
                            color: textItem.highlighted ? Theme.highlightColor : Theme.primaryColor
                        }
                        Button {
                            objectName: "copyButton"
                            //: Copies a received text to the clipboard.
                            text: qsTr("Copy")
                            onClicked: page.copyText(model.text)
                        }
                    }
                }
            }

            Item {
                width: 1
                height: Theme.paddingLarge * 2
                visible: emptyHint.visible
            }

            Label {
                id: emptyHint
                objectName: "emptyHint"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.empty
                horizontalAlignment: Text.AlignHCenter
                //: History page with nothing sent or received yet.
                text: qsTr("Nothing sent or received yet.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeLarge
                color: Theme.secondaryHighlightColor
            }
        }

        VerticalScrollDecorator {}
    }

    Banner {
        id: banner
        anchors.top: parent.top
        z: 1
    }
}
