// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The consent dialog (F-C2, F-C3, F-QS3, S5): it shows the offer's values
 * exactly as the engine sent them, as plain text, and the only way to a
 * "yes" is Accept. Declining, leaving, the countdown running out and the
 * engine closing the offer all end in no -- and the last one sends
 * nothing at all.
 */
Script {
    id: test

    property Item dialog: null

    ApplicationWindow {
        id: window
    }

    Engine {
        id: engine
        backend: bridge
    }

    function open(offerId) {
        test.dialog = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/ConsentDialog.qml"),
                                            { engine: engine, offer: engine.offer(offerId) })
        test.verify(test.dialog !== null, "the dialog opened for " + offerId)
    }

    function answers() {
        var out = []
        var cmds = bridge.parsedCommands()
        for (var i = 0; i < cmds.length; i++) {
            if (cmds[i].cmd.type === "answer") {
                out.push([cmds[i].cmd.offer, cmds[i].cmd.accept])
            }
        }
        return out
    }

    function label(name) {
        var item = probe.find(test.dialog, name)
        test.verify(item !== null, "a label called " + name)
        return item
    }

    steps: [
        function () {
            bridge.emitEvent(Ev.offer(7, {}))
        },
        function () {
            test.open(7)
            var sender = test.label("consentSender")
            test.compare(sender.text, Ev.EVIL_NAME, "the sender verbatim, %2 and all")
            test.compare(sender.textFormat, Text.PlainText)
            test.compare(test.label("consentModel").text, Ev.EVIL_MODEL)
            test.compare(test.label("consentProtocol").text, "wants to send you files over Quick Share")
            test.compare(test.label("consentPin").text, "4821")
            test.verify(test.label("consentPin").visible, "the PIN is shown (F-QS3)")
            test.verify(test.label("consentHasText").visible, "the text is announced")
            var names = probe.findAll(test.dialog, "consentFileName")
            test.compare(names.length, 2)
            test.compare(names[0].text, Ev.EVIL_FILE)
            test.compare(names[1].text, "notes.txt")
            test.compare(test.label("consentMore").text, "and 3 more files")
            test.compare(test.label("consentTotal").text, "5 files, 2.6 MB in all")
            test.verify(/^Declined automatically in (59|60) seconds\.$/.test(test.label("consentCountdown").text),
                        "a countdown: " + test.label("consentCountdown").text)
            // Nothing the peer sent reached a Silica-owned text item.
            test.verifyPlainText(test.dialog, "the consent dialog")
            // Nothing is answered while the user looks at it.
            test.compare(test.answers(), [])
            // There is no "always accept": no switch at all on the dialog.
            var texts = probe.texts(test.dialog).join("|").toLowerCase()
            test.verify(texts.indexOf("always") < 0 && texts.indexOf("trust") < 0, "no auto-accept")
        },
        function () {
            test.dialog.accept()
            test.compare(test.answers(), [[7, true]], "Accept answers yes")
            test.compare(window.pageStack.depth, 0, "and the dialog is gone")
            test.compare(engine.offers.count, 0, "and the offer is off the queue")
            bridge.emitEvent(Ev.offer(8, { pin: null, model: null, has_text: false, more_files: 0,
                                           file_count: 1, total_bytes: 10,
                                           files: [{ name: "a.txt", size: 10 }] }))
        },
        function () {
            test.open(8)
            test.verify(!probe.find(test.dialog, "consentPin").parent.visible, "no PIN, no PIN block")
            test.verify(!test.label("consentModel").visible, "no model line")
            test.verify(!test.label("consentMore").visible, "nothing more")
            test.verify(!test.label("consentHasText").visible, "no text line")
            test.compare(test.label("consentTotal").text, "1 file, 10 B in all")
            test.dialog.reject()
            test.compare(test.answers(), [[7, true], [8, false]], "Decline answers no")
            bridge.emitEvent(Ev.offer(9, {}))
        },
        function () {
            // Leaving without answering -- back, or the stack cleared -- is no.
            test.open(9)
            window.pageStack.clear()
        },
        function () {
            test.compare(test.answers(), [[7, true], [8, false], [9, false]], "leaving answers no")
            bridge.emitEvent(Ev.offer(10, { expires_in: 1 }))
        },
        function () {
            test.open(10)
            return 1600
        },
        function () {
            // F-C3: declined here when the countdown ends.
            test.compare(test.answers(), [[7, true], [8, false], [9, false], [10, false]], "the countdown answers no")
            test.compare(window.pageStack.depth, 0, "and closes the dialog")
            bridge.emitEvent(Ev.offer(11, {}))
        },
        function () {
            test.open(11)
            bridge.emitEvent(Ev.offerClosed(11, "withdrawn"))
            return 300
        },
        function () {
            // The engine closed it: the dialog goes, and says nothing back.
            test.compare(window.pageStack.depth, 0, "a withdrawn offer closes its dialog")
            test.compare(test.answers().length, 4, "without an answer")
            // Another offer's closing is not this dialog's business.
            bridge.emitEvent(Ev.offer(12, {}))
            bridge.emitEvent(Ev.offer(13, {}))
        },
        function () {
            test.open(12)
            bridge.emitEvent(Ev.offerClosed(13, "timed_out"))
            return 300
        },
        function () {
            test.compare(window.pageStack.depth, 1, "still open for offer 12")
            test.dialog.accept()
            // Accepting twice, or after the fact, sends one answer.
            test.compare(test.answers(), [[7, true], [8, false], [9, false], [10, false], [12, true]])
        }
    ]
}
