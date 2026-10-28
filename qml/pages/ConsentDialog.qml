// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * An incoming offer, and the only way anything is received (F-C2, S5).
 *
 * Shows the sender's name, what they send and over which protocol, the
 * PIN when Quick Share has one (F-QS3), the files with their sizes -- the
 * first 50, then "and N more" -- and the total, with a countdown to the
 * automatic decline (F-C3). A file's kind is told by its name alone:
 * nothing offered is ever drawn. Accept or decline; there is no "always
 * accept", and nothing here remembers a sender.
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

    /// The dialog has left the stack for good. The window shows the next
    /// offer only then -- not when this one is answered, while it may
    /// still be on the stack -- so a dialog is never pushed over another.
    signal gone()

    function secondsLeft() {
        if (!dialog.offer) {
            return 0
        }
        return Math.max(0, Math.ceil((dialog.offer.expiresAt - Date.now()) / 1000))
    }

    /// The one answer this dialog gives. The engine answers only an offer
    /// that still waits (Engine.answer), so a closed one gets nothing.
    function answer(accept) {
        if (dialog.answered) {
            return
        }
        dialog.answered = true
        if (!dialog.closedByEngine && dialog.offerId >= 0 && dialog.engine) {
            dialog.engine.answer(dialog.offerId, accept)
        }
    }

    /// Closes the dialog from code, answering no if nobody has. Until it
    /// is off the stack its Accept does nothing, rather than look as if it
    /// could still say yes to an offer that is over. (Only here: the
    /// user's own Accept must not change canAccept under Silica's feet.)
    function dismiss() {
        dialog.answer(false)
        dialog.canAccept = false
        closer.start()
    }

    onAccepted: dialog.answer(true)
    onRejected: dialog.answer(false)
    // Popped some other way -- the stack cleared, the app closing: no.
    Component.onDestruction: {
        dialog.answer(false)
        dialog.gone()
    }

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

    // A dialog can only leave the stack once it has finished arriving, and
    // only from the top: what Silica's reject() does to a covered dialog
    // is not ours to rely on. So this keeps trying until the dialog is on
    // top and the stack still, and never gives up -- giving up once left a
    // closed dialog in the stack, a stale offer with a frozen countdown
    // under the next one. The window pushes nothing over a dialog, so the
    // wait is for a transition to end.
    Timer {
        id: closer
        interval: 100
        repeat: true
        onTriggered: {
            if (pageStack.busy) {
                return
            }
            if (dialog.status === PageStatus.Active || dialog.status === PageStatus.Activating) {
                stop()
                dialog.reject()
            }
        }
    }

    /// "3 photos", "a file": what is offered, in Sukkula's words only.
    function whatText() {
        if (!dialog.offer) {
            return ""
        }
        if (dialog.offer.fileCount === 0) {
            //: Consent dialog: an offer of a text message only, as in "wants to send you a message".
            return qsTr("a message")
        }
        if (dialog.offer.fileCount === 1) {
            //: Consent dialog: an offer of one file, as in "wants to send you a file".
            return qsTr("a file")
        }
        var names = []
        for (var i = 0; i < dialog.offer.files.length; i++) {
            names.push(dialog.offer.files[i].name)
        }
        return dialog.engine.countWords(dialog.engine.commonKind(names, dialog.offer.fileCount),
                                        dialog.offer.fileCount)
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

            // F-C3: how long is left, as a line running down, and in words.
            Column {
                width: parent.width
                spacing: Theme.paddingSmall

                ProgressLine {
                    objectName: "consentClock"
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    color: Theme.secondaryColor
                    value: dialog.remaining / dialog.engine.offerTimeoutSeconds
                }
                Label {
                    objectName: "consentCountdown"
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    horizontalAlignment: Text.AlignRight
                    //: Consent dialog: time left before the offer is declined on its own.
                    text: qsTr("Declined in %n s", "", dialog.remaining)
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryColor
                }
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
                objectName: "consentProtocol"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: Consent dialog, under the sender's name: what and how, e.g. "wants to send you 3 photos over Quick Share"; %1 is what, %2 the protocol.
                text: qsTr("wants to send you %1 over %2").arg(dialog.whatText())
                      .arg(dialog.offer ? dialog.engine.protocolName(dialog.offer.protocol) : "")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
            }

            Label {
                objectName: "consentHasText"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: dialog.offer !== null && dialog.offer.hasText && dialog.offer.fileCount > 0
                //: Consent dialog: the offer carries a text message besides any files.
                text: qsTr("Includes a text message.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }

            // F-QS3: the PIN both screens show.
            Item {
                width: parent.width
                height: pinBox.height
                visible: dialog.offer !== null && dialog.offer.pin.length > 0

                Rectangle {
                    id: pinBox
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    height: pinColumn.height + 2 * Theme.paddingMedium
                    radius: Theme.paddingSmall
                    color: Theme.rgba(Theme.highlightBackgroundColor, 0.1)

                    Column {
                        id: pinColumn
                        x: Theme.paddingLarge
                        y: Theme.paddingMedium
                        width: parent.width - 2 * Theme.paddingLarge

                        Label {
                            width: parent.width
                            //: Consent dialog: above the Quick Share PIN; %1 is the sender's name.
                            text: qsTr("Check that %1 shows the same number:").arg(dialog.offer ? dialog.offer.sender : "")
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.secondaryColor
                        }
                        Label {
                            objectName: "consentPin"
                            width: parent.width
                            text: dialog.offer ? dialog.offer.pin : ""
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.fontSizeExtraLarge
                            font.letterSpacing: Theme.paddingMedium
                            color: Theme.highlightColor
                        }
                    }
                }
            }

            // One file: big, by itself.
            Item {
                width: parent.width
                height: Theme.itemSizeLarge
                visible: dialog.offer !== null && dialog.offer.fileCount === 1 && dialog.offer.files.length === 1

                Glyph {
                    id: oneGlyph
                    x: Theme.horizontalPageMargin
                    anchors.verticalCenter: parent.verticalCenter
                    width: Theme.iconSizeLarge
                    kind: dialog.offer && dialog.offer.files.length === 1
                          ? dialog.engine.kindOf(dialog.offer.files[0].name) : "file"
                    color: Theme.primaryColor
                }
                Column {
                    anchors {
                        left: oneGlyph.right
                        leftMargin: Theme.paddingLarge
                        right: parent.right
                        rightMargin: Theme.horizontalPageMargin
                        verticalCenter: parent.verticalCenter
                    }
                    Label {
                        objectName: "consentFileName"
                        width: parent.width
                        text: dialog.offer && dialog.offer.files.length === 1 ? dialog.offer.files[0].name : ""
                        textFormat: Text.PlainText
                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                        maximumLineCount: 3
                        elide: Text.ElideMiddle
                    }
                    Label {
                        width: parent.width
                        text: Format.formatFileSize(dialog.offer && dialog.offer.files.length === 1
                                                    ? dialog.offer.files[0].size : 0)
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.secondaryColor
                    }
                }
            }

            // Several: how many and how much, then each.
            Column {
                width: parent.width
                visible: dialog.offer !== null && dialog.offer.fileCount > 1

                SectionHeader {
                    objectName: "consentTotal"
                    //: Consent dialog: how many files and how much in total; %1 is the formatted size.
                    text: qsTr("%n file(s), %1 in total", "", dialog.offer ? dialog.offer.fileCount : 0)
                          .arg(Format.formatFileSize(dialog.offer ? dialog.offer.totalBytes : 0))
                }

                Repeater {
                    model: dialog.offer && dialog.offer.fileCount > 1 ? dialog.offer.files : []
                    delegate: Item {
                        width: column.width
                        height: Theme.itemSizeSmall

                        Glyph {
                            id: fileGlyph
                            x: Theme.horizontalPageMargin
                            anchors.verticalCenter: parent.verticalCenter
                            kind: dialog.engine.kindOf(modelData.name)
                            color: Theme.primaryColor
                        }
                        Label {
                            objectName: "consentFileName"
                            anchors {
                                left: fileGlyph.right
                                leftMargin: Theme.paddingMedium
                                right: sizeLabel.left
                                rightMargin: Theme.paddingMedium
                                verticalCenter: parent.verticalCenter
                            }
                            text: modelData.name
                            textFormat: Text.PlainText
                            // The middle goes, so both the start and the
                            // extension stay visible (S1 keeps the latter).
                            elide: Text.ElideMiddle
                        }
                        Label {
                            id: sizeLabel
                            anchors {
                                right: parent.right
                                rightMargin: Theme.horizontalPageMargin
                                verticalCenter: parent.verticalCenter
                            }
                            text: Format.formatFileSize(modelData.size)
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.secondaryColor
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
            }
        }

        VerticalScrollDecorator {}
    }
}
