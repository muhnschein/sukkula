// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * An incoming offer, and the only way anything is received (F-C2, S5).
 *
 * Shows the sender's name and model, the protocol, the PIN when Quick
 * Share has one (F-QS3), the files with their sizes -- the first 50, then
 * "and N more" -- and the total, with a countdown to the automatic decline
 * (F-C3). Accept or decline; there is no "always accept", and nothing here
 * remembers a sender.
 *
 * Every value from the offer was chosen by the other device. The engine
 * has cleaned it (S1, S2); this page still shows it only in its own labels
 * with `textFormat: Text.PlainText`, never in a Silica header or button,
 * and a peer-supplied value is always the last `.arg()` of a string so a
 * "%2" inside it stays text.
 *
 * Fail closed: leaving the dialog any way but Accept declines, and so does
 * the countdown reaching zero.
 */
Dialog {
    id: dialog
    objectName: "consentDialog"

    property QtObject engine
    /// Engine.offer(): offerId, protocol, sender, model, files [{name,
    /// size}], moreFiles, fileCount, totalBytes, hasText, pin, expiresAt.
    property var offer: null

    readonly property var offerId: dialog.offer ? dialog.offer.offerId : -1
    property int remaining: dialog.secondsLeft()
    property bool answered: false
    /// The engine closed the offer (timed out, withdrawn): nothing to answer.
    property bool closedByEngine: false

    signal finished()

    function secondsLeft() {
        if (!dialog.offer) {
            return 0
        }
        return Math.max(0, Math.ceil((dialog.offer.expiresAt - Date.now()) / 1000))
    }

    function answer(accept) {
        if (dialog.answered) {
            return
        }
        dialog.answered = true
        if (!dialog.closedByEngine && dialog.offerId >= 0 && dialog.engine) {
            dialog.engine.answer(dialog.offerId, accept)
        }
        dialog.finished()
    }

    /// Closes the dialog from code, answering no if nobody has.
    function dismiss() {
        dialog.answer(false)
        closer.start()
    }

    onAccepted: dialog.answer(true)
    onRejected: dialog.answer(false)
    // Popped some other way -- the stack cleared, the app closing: no.
    Component.onDestruction: dialog.answer(false)

    Connections {
        target: dialog.engine
        // Qt 5.6 handler syntax.
        onOfferClosed: {
            if (offerId === dialog.offerId && !dialog.answered) {
                dialog.closedByEngine = true
                dialog.dismiss()
            }
        }
    }

    // F-C3: counted down here, declined here at zero -- whether or not the
    // engine's own timeout has already said so.
    Timer {
        id: countdown
        interval: 1000
        repeat: true
        running: !dialog.answered
        triggeredOnStart: true
        onTriggered: {
            dialog.remaining = dialog.secondsLeft()
            if (dialog.remaining <= 0) {
                dialog.dismiss()
            }
        }
    }

    // A dialog can only leave the stack once it has finished arriving.
    Timer {
        id: closer
        interval: 100
        repeat: true
        onTriggered: {
            if (dialog.pageStack && dialog.pageStack.busy) {
                return
            }
            stop()
            if (dialog.status === PageStatus.Active || dialog.status === PageStatus.Activating) {
                dialog.reject()
            }
        }
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        Column {
            id: column
            width: parent.width
            spacing: Theme.paddingMedium

            DialogHeader {
                //: Consent dialog: receive the offered files.
                acceptText: qsTr("Accept")
                //: Consent dialog: refuse the offered files.
                cancelText: qsTr("Decline")
            }

            Label {
                objectName: "consentSender"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                text: dialog.offer ? dialog.offer.sender : ""
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                font.pixelSize: Theme.fontSizeExtraLarge
                color: Theme.highlightColor
            }

            Label {
                objectName: "consentModel"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: text.length > 0
                text: dialog.offer ? dialog.offer.model : ""
                textFormat: Text.PlainText
                truncationMode: TruncationMode.Fade
                color: Theme.secondaryHighlightColor
            }

            Label {
                objectName: "consentProtocol"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: Consent dialog: how the offer came, e.g. "wants to send you files over LocalSend".
                text: qsTr("wants to send you files over %1")
                      .arg(dialog.offer ? dialog.engine.protocolName(dialog.offer.protocol) : "")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.secondaryHighlightColor
            }

            // F-QS3: the PIN both screens show.
            Column {
                width: parent.width
                visible: dialog.offer !== null && dialog.offer.pin.length > 0
                spacing: Theme.paddingSmall

                Label {
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    //: Consent dialog: above the Quick Share PIN.
                    text: qsTr("Check that the other device shows this PIN:")
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryColor
                }
                Label {
                    objectName: "consentPin"
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    text: dialog.offer ? dialog.offer.pin : ""
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeHuge
                    font.letterSpacing: Theme.paddingSmall
                    color: Theme.highlightColor
                }
            }

            Label {
                objectName: "consentHasText"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: dialog.offer !== null && dialog.offer.hasText
                //: Consent dialog: the offer carries a text message besides any files.
                text: qsTr("Includes a text message.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.secondaryHighlightColor
            }

            Column {
                width: parent.width
                visible: dialog.offer !== null && dialog.offer.files.length > 0

                Repeater {
                    model: dialog.offer ? dialog.offer.files : []
                    delegate: Item {
                        width: column.width
                        height: Math.max(nameLabel.height, sizeLabel.height) + Theme.paddingSmall

                        Label {
                            id: nameLabel
                            objectName: "consentFileName"
                            anchors {
                                left: parent.left
                                leftMargin: Theme.horizontalPageMargin
                                right: sizeLabel.left
                                rightMargin: Theme.paddingMedium
                            }
                            text: modelData.name
                            textFormat: Text.PlainText
                            // The middle goes, so both the start and the
                            // extension stay visible (S1 keeps the latter).
                            elide: Text.ElideMiddle
                            font.pixelSize: Theme.fontSizeSmall
                        }
                        Label {
                            id: sizeLabel
                            anchors {
                                right: parent.right
                                rightMargin: Theme.horizontalPageMargin
                            }
                            text: Format.formatFileSize(modelData.size)
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.secondaryColor
                        }
                    }
                }
            }

            Label {
                objectName: "consentMore"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: dialog.offer !== null && dialog.offer.moreFiles > 0
                //: Consent dialog: files not listed by name.
                text: qsTr("and %n more file(s)", "", dialog.offer ? dialog.offer.moreFiles : 0)
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }

            Label {
                objectName: "consentTotal"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: dialog.offer !== null && dialog.offer.fileCount > 0
                //: Consent dialog: how many files and how much in all; %1 is the formatted size.
                text: qsTr("%n file(s), %1 in all", "", dialog.offer ? dialog.offer.fileCount : 0)
                      .arg(Format.formatFileSize(dialog.offer ? dialog.offer.totalBytes : 0))
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.highlightColor
            }

            Label {
                objectName: "consentCountdown"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: Consent dialog: time left before the offer is declined on its own.
                text: qsTr("Declined automatically in %n second(s).", "", dialog.remaining)
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: Consent dialog: what accepting does.
                text: qsTr("Nothing is saved unless you accept. Files go to Downloads/Sukkula.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
            }
        }

        VerticalScrollDecorator {}
    }
}
