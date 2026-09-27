// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The whole window, qml/harbour-sukkula.qml, with `bridge` from the root
 * context as main.cpp provides it: the engine started once, the consent
 * dialog brought up over whatever is showing and one offer at a time
 * (F-C2, F-C3), the stack's transitions waited out, Send mode's discovery
 * paused while the app stays in the background, the Share menu's files
 * reaching the Send tab, its texts not taken (F-C6, spec v0.7), the
 * cover's actions opening the app on their tab, KeepAlive held during
 * transfers only (§2), and notifications that name no one.
 */
Script {
    id: test

    property Item win: app.item
    property Item stack: app.item ? app.item.pageStack : null

    Loader {
        id: app
        source: "../../qml/harbour-sukkula.qml"
    }

    function find(name) {
        return probe.find(app, name)
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

    /// The discovery commands, in order.
    function discovery() {
        var out = []
        var cmds = bridge.parsedCommands()
        for (var i = 0; i < cmds.length; i++) {
            var t = cmds[i].cmd.type
            if (t === "start_discovery" || t === "stop_discovery") {
                out.push(t)
            }
        }
        return out
    }

    function top() {
        return test.stack.currentPage ? test.stack.currentPage.objectName : ""
    }

    steps: [
        function () {
            test.verify(test.win !== null, "the window loads")
            test.compare(bridge.startCount, 1, "the engine is started once")
            test.compare(test.top(), "mainPage")
            test.verify(test.win.coverItem !== null, "the cover is made")
            test.verify(probe.find(test.win.coverItem, "coverIdle").visible, "the cover: Send and Receive")
            test.compare(test.find("keepAlive").enabled, false, "no KeepAlive while idle")
            // In front, as the phone starts it (the runner's window is not).
            test.win.applicationActive = true
            test.compare(test.discovery(), ["start_discovery"], "Send mode looks for peers")
            // A glance away keeps discovery...
            test.win.applicationActive = false
            return 100
        },
        function () {
            test.win.applicationActive = true
            test.compare(test.discovery(), ["start_discovery"], "a glance away changes nothing")
            // ...a longer stay in the background pauses it.
            test.find("mainPage").backgroundGrace = 50
            test.win.applicationActive = false
            return 200
        },
        function () {
            test.compare(test.discovery(), ["start_discovery", "stop_discovery"], "paused in the background")
            test.compare(test.find("mainPage").engine.discoveryUsers, 0)
            test.win.applicationActive = true
            test.compare(test.discovery(), ["start_discovery", "stop_discovery", "start_discovery"],
                         "and back with the app")
            test.find("mainPage").backgroundGrace = 5000
        },
        function () {
            // An offer comes up over the main page (F-C2).
            bridge.emitEvent(Ev.offer(1, {}))
            bridge.emitEvent(Ev.offer(2, { sender: "Second EVIL" }))
            return 100
        },
        function () {
            test.compare(test.top(), "consentDialog")
            test.compare(test.stack.depth, 2, "one dialog, however many offers wait")
            test.compare(test.stack.currentPage.offer.offerId, 1, "the oldest first")
            test.verifyPlainText(test.stack.currentPage, "the first dialog")
            test.stack.currentPage.accept()
            return 400
        },
        function () {
            test.compare(test.answers(), [[1, true]])
            test.compare(test.top(), "consentDialog", "the next offer follows")
            test.compare(test.stack.currentPage.offer.offerId, 2)
            test.compare(probe.find(test.stack.currentPage, "consentSender").text, "Second EVIL")
            test.stack.currentPage.reject()
            return 400
        },
        function () {
            test.compare(test.answers(), [[1, true], [2, false]])
            test.compare(test.top(), "mainPage", "nothing left waiting")
            // A transition in progress is waited out, not raced.
            test.stack.holdBusy = true
            bridge.emitEvent(Ev.offer(3, {}))
            return 400
        },
        function () {
            test.compare(test.top(), "mainPage", "not pushed while the stack is busy")
            test.stack.holdBusy = false
            return 400
        },
        function () {
            test.compare(test.top(), "consentDialog", "pushed once it is not")
            // The engine withdraws it: the dialog goes, unanswered.
            bridge.emitEvent(Ev.offerClosed(3, "withdrawn"))
            return 400
        },
        function () {
            test.compare(test.top(), "mainPage")
            test.compare(test.answers().length, 2, "no answer for a withdrawn offer")
            // In the background: an offer is announced without a name.
            test.win.applicationActive = false
            bridge.emitEvent(Ev.offer(4, {}))
            return 300
        },
        function () {
            var note = test.find("offerNote")
            test.verify(note.isPublished, "announced while in the background")
            var said = [note.summary, note.body, note.previewSummary, note.previewBody].join(" ")
            test.verify(said.indexOf("EVIL") < 0, "no peer data in a notification: " + said)
            test.win.applicationActive = true
            test.verify(!note.isPublished, "taken down once the app is in front")
            test.stack.currentPage.reject()
            return 300
        },
        function () {
            // KeepAlive during a transfer, and only then (§2).
            bridge.emitEvent(Ev.transferStarted(10, "incoming", {}))
        },
        function () {
            test.compare(test.find("keepAlive").enabled, true, "held during a transfer")
            test.win.applicationActive = false
            bridge.emitEvent(Ev.finished(10, "done", [Ev.EVIL_FILE, "b.pdf"]))
        },
        function () {
            test.compare(test.find("keepAlive").enabled, false, "released after it")
            var note = test.find("doneNote")
            test.compare(note.publishCount, 1)
            test.compare(note.summary, "2 files received")
            test.compare(note.body, "Saved in Downloads/Sukkula")
            test.verify(note.remoteActions.length === 0, "no action that opens anything")
            // An outgoing one is not announced.
            bridge.emitEvent(Ev.transferStarted(11, "outgoing", {}))
            bridge.emitEvent(Ev.finished(11, "done"))
        },
        function () {
            test.compare(test.find("doneNote").publishCount, 1, "outgoing: no notification")
            bridge.emitEvent(Ev.transferStarted(12, "incoming", {}))
            bridge.emitEvent(Ev.finished(12, "failed", null, "network"))
        },
        function () {
            var note = test.find("doneNote")
            test.compare(note.summary, "Receiving failed")
            test.compare(note.body, "Network error or timeout.")
            test.win.applicationActive = true
            // The Share menu (F-C6), while receiving and with a page over
            // the main one.
            test.find("modeTabs").tabClicked(1)
            bridge.emitEvent(Ev.receiving(true))
            return 50
        },
        function () {
            test.compare(test.find("modePager").currentIndex, 1, "receiving")
            test.stack.push(Qt.resolvedUrl("../../qml/pages/AboutPage.qml"), { engine: test.find("mainPage").engine })
            return 50
        },
        function () {
            var provider = test.find("shareFiles")
            test.verify(provider !== null && provider.registerName === true, "a provider that owns the D-Bus name")
            test.compare(provider.method, "files")
            provider.triggered([
                { filePath: "/home/defaultuser/Downloads/x.jpg" },
                { url: "file:///home/defaultuser/Downloads/y%20z.png" },
                { url: "https://evil.example/not-a-file" },
                { filePath: "relative.txt" },
                { data: "shared EVIL text" }
            ])
            return 200
        },
        function () {
            test.compare(test.win.activateCount, 1, "the window comes forward")
            test.compare(test.top(), "mainPage", "the main page, whatever was over it")
            test.compare(test.stack.depth, 1)
            var page = test.stack.currentPage
            test.compare(page.payload.files.length, 2, "chosen on the Send tab; the text not taken")
            test.compare(page.payload.files[1].path, "/home/defaultuser/Downloads/y z.png")
            test.compare(test.find("modePager").currentIndex, 0, "the Send tab")
            var cmds = bridge.parsedCommands()
            test.compare(cmds[cmds.length - 1].cmd, { type: "set_receiving", on: false }, "and Send mode")
            bridge.emitEvent(Ev.receiving(false))
            // A share while a dialog is up waits for the dialog.
            bridge.emitEvent(Ev.offer(5, {}))
            return 300
        },
        function () {
            test.compare(test.top(), "consentDialog")
            test.verify(test.find("shareText") === null, "no share method for texts")
            test.find("shareFiles").triggered([{ filePath: "/home/defaultuser/Documents/second.pdf" }])
            return 300
        },
        function () {
            test.compare(test.top(), "consentDialog", "the share does not push the dialog away")
            test.stack.currentPage.reject()
            return 500
        },
        function () {
            test.compare(test.top(), "mainPage", "then the share")
            test.compare(test.stack.depth, 1)
            test.compare(test.stack.currentPage.payload.files,
                         [{ path: "/home/defaultuser/Documents/second.pdf", name: "second.pdf", size: -1 }],
                         "in place of the first")
            // Nothing was sent by sharing alone.
            var cmds = bridge.parsedCommands()
            for (var i = 0; i < cmds.length; i++) {
                test.verify(cmds[i].cmd.type !== "send", "no send without a target chosen")
            }
            // The cover's actions open the app on their tab, whatever
            // was over the main page.
            bridge.emitEvent(Ev.receiving(false))
            test.win.applicationActive = false
            test.stack.push(Qt.resolvedUrl("../../qml/pages/AboutPage.qml"), { engine: test.find("mainPage").engine })
            return 50
        },
        function () {
            var before = test.win.activateCount
            probe.find(test.win.coverItem, "coverActionReceive").triggered()
            test.compare(test.win.activateCount, before + 1, "the window comes forward")
            test.compare(test.top(), "mainPage")
            test.compare(test.find("modePager").currentIndex, 1, "on the Receive tab")
            var cmds = bridge.parsedCommands()
            var last = null
            for (var i = 0; i < cmds.length; i++) {
                if (cmds[i].cmd.type === "set_receiving") {
                    last = cmds[i].cmd
                }
            }
            test.compare(last, { type: "set_receiving", on: true }, "receiving")
            probe.find(test.win.coverItem, "coverActionSend").triggered()
            test.compare(test.find("modePager").currentIndex, 0, "and on the Send tab")
        }
    ]
}
