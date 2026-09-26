// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The whole window while pages come and go over each other: what the page
 * stack lets happen out of order, each of which once went wrong.
 *
 *  - A consent dialog pushed over Settings saves nothing (F-C2): a save
 *    restarts the receivers, which withdrew the offer being shown, and
 *    made a half-typed value live. Leaving Settings saves what it holds.
 *  - A share that arrives while a page is over the main one lands at the
 *    radar's centre with discovery kept running and the peers kept (F-LS1,
 *    F-QS1, F-C6); Receive mode gives discovery back and forgets them.
 *  - A reply to a send never moves a consent dialog that came up over the
 *    main page (F-C2, S5), and a reply to a wormhole code pops its own
 *    page only, once it is on top again.
 *  - An offer closed while its dialog cannot leave yet is never left in
 *    the stack under the next offer's dialog (F-C2, F-C3), and whatever
 *    closes is not answered.
 */
Script {
    id: test

    property Item win: app.item
    property Item stack: app.item ? app.item.pageStack : null
    property QtObject engine: null
    property var pendingId: -1

    Loader {
        id: app
        source: "../../qml/harbour-sukkula.qml"
    }

    function find(name) {
        return probe.find(app, name)
    }

    function top() {
        return test.stack.currentPage ? test.stack.currentPage.objectName : ""
    }

    /// The stack, bottom first, by objectName.
    function names() {
        var out = []
        for (var i = 0; i < test.stack.pages.length; i++) {
            out.push(test.stack.pages[i].objectName)
        }
        return out
    }

    function commands(type) {
        var out = []
        var cmds = bridge.parsedCommands()
        for (var i = 0; i < cmds.length; i++) {
            if (cmds[i].cmd.type === type) {
                out.push(cmds[i])
            }
        }
        return out
    }

    function lastId(type) {
        var all = test.commands(type)
        return all.length > 0 ? all[all.length - 1].id : -1
    }

    function answers() {
        var out = []
        var all = test.commands("answer")
        for (var i = 0; i < all.length; i++) {
            out.push([all[i].cmd.offer, all[i].cmd.accept])
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

    steps: [
        function () {
            test.compare(test.top(), "mainPage")
            test.engine = test.find("mainPage").engine
            test.verify(test.engine !== null, "the engine, through the main page")
            // Settings, with something typed and a switch changed.
            test.stack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: test.engine })
            test.find("deviceNameField").text = "Pek"
            test.find("loggingSwitch").click()
            bridge.emitEvent(Ev.offer(1, {}))
            return 100
        },
        function () {
            test.compare(test.names(), ["mainPage", "settingsPage", "consentDialog"], "the dialog over Settings")
            test.compare(test.commands("set_settings").length, 0,
                         "a dialog over Settings saves nothing: that would restart the receivers")
            test.stack.currentPage.reject()
            return 400
        },
        function () {
            test.compare(test.top(), "settingsPage", "back to Settings")
            test.compare(test.answers(), [[1, false]])
            test.compare(test.commands("set_settings").length, 0, "still nothing saved")
            test.find("deviceNameField").text = "Pekka"
            test.stack.pop()
            return 100
        },
        function () {
            var saved = test.commands("set_settings")
            test.compare(saved.length, 1, "saved once, on leaving")
            test.compare(saved[0].cmd.settings.device_name, "Pekka", "what was typed in the end")
            test.compare(saved[0].cmd.settings.logging, true)
            test.compare(test.top(), "mainPage")
            // Send mode holds discovery.
            test.compare(test.engine.discoveryUsers, 1)
            bridge.emitEvent(Ev.peerFound("p1", "local_send"))
            bridge.emitEvent(Ev.peerFound("q1", "quick_share", "Android"))
            return 50
        },
        function () {
            test.compare(test.engine.localSendPeers.count, 1)
            // A share while the "What to send" page is over the main page.
            probe.find(test.find("mainPage"), "origin").clicked()
            test.compare(test.top(), "payloadPage")
            test.find("shareText").triggered([{ data: "second" }])
            return 200
        },
        function () {
            var main = test.find("mainPage")
            test.compare(test.names(), ["mainPage"], "the share goes to the main page")
            test.compare(main.payload.texts, ["second"])
            var d = test.discovery()
            test.compare(d, ["start_discovery"], "discovery kept running: " + d)
            test.compare(test.engine.discoveryUsers, 1)
            test.compare(test.engine.localSendPeers.count, 1, "the peers found are still listed")
            test.compare(test.engine.quickSharePeers.count, 1)
            // A send whose reply comes while an offer's dialog is up.
            bridge.autoReply = false
            var bubbles = probe.findAll(main, "peerBubble")
            for (var i = 0; i < bubbles.length; i++) {
                if (bubbles[i].visible && bubbles[i].name === "Android") {
                    bubbles[i].clicked()
                }
            }
            test.pendingId = test.lastId("send")
            test.verify(test.pendingId > 0, "the send went out")
            bridge.emitEvent(Ev.offer(2, {}))
            return 100
        },
        function () {
            test.compare(test.top(), "consentDialog")
            bridge.emitEvent(Ev.reply(test.pendingId, true, "", 40))
            return 200
        },
        function () {
            test.compare(test.names(), ["mainPage", "consentDialog"], "the send's reply leaves the dialog alone")
            test.compare(test.stack.currentPage.offer.offerId, 2)
            test.compare(test.answers(), [[1, false]], "and nothing answers the offer for the user")
            test.compare(probe.find(test.find("mainPage"), "sendView").outgoing.transferId, 40,
                         "the send is on the radar underneath")
            test.stack.currentPage.reject()
            return 400
        },
        function () {
            test.compare(test.answers(), [[1, false], [2, false]])
            test.compare(test.names(), ["mainPage"])
            // Receive mode gives discovery back.
            bridge.emitEvent(Ev.receiving(true))
            return 50
        },
        function () {
            var d = test.discovery()
            test.compare(d[d.length - 1], "stop_discovery", "Receive mode stops discovery")
            test.compare(test.engine.discoveryUsers, 0)
            test.compare(test.engine.localSendPeers.count, 0, "and the peers are forgotten")
            // Receiving with a code: its reply pops its own page only.
            test.stack.push(Qt.resolvedUrl("../../qml/pages/WormholeReceivePage.qml"), { engine: test.engine })
            test.find("codeField").text = "7-guitarist-revenge"
            test.find("receiveButton").clicked()
            test.pendingId = test.lastId("receive_wormhole")
            bridge.emitEvent(Ev.offer(4, {}))
            return 100
        },
        function () {
            test.compare(test.top(), "consentDialog")
            bridge.emitEvent(Ev.reply(test.pendingId, true))
            return 200
        },
        function () {
            test.compare(test.names(), ["mainPage", "wormholeReceivePage", "consentDialog"],
                         "the code's reply leaves the dialog alone")
            test.stack.currentPage.reject()
            return 400
        },
        function () {
            test.compare(test.answers(), [[1, false], [2, false], [4, false]])
            test.compare(test.names(), ["mainPage"], "the code page went once it was on top")
            bridge.autoReply = true
            // Two offers; the first is withdrawn while its dialog is still
            // arriving, and the stack comes free between the dialog's own
            // way out (100 ms ticks) and the window's next look (150 ms).
            bridge.emitEvent(Ev.offer(5, {}))
            bridge.emitEvent(Ev.offer(6, { sender: "Sixth EVIL" }))
            return 300
        },
        function () {
            test.compare(test.names(), ["mainPage", "consentDialog"])
            test.compare(test.stack.currentPage.offer.offerId, 5)
            test.stack.holdBusy = true
            bridge.emitEvent(Ev.offerClosed(5, "withdrawn"))
            return 125
        },
        function () {
            test.stack.holdBusy = false
            return 600
        },
        function () {
            test.compare(test.names(), ["mainPage", "consentDialog"], "one dialog, never one over another")
            test.compare(test.stack.currentPage.offer.offerId, 6, "the next offer's")
            test.compare(test.answers().length, 3, "the withdrawn offer is not answered")
            test.stack.currentPage.accept()
            return 400
        },
        function () {
            test.compare(test.answers()[3], [6, true])
            test.compare(test.names(), ["mainPage"], "and no stale dialog comes back")
            // An offer that times out while the stack is busy: declined by
            // the dialog's countdown, and gone before the next one shows.
            bridge.emitEvent(Ev.offer(7, { expires_in: 1 }))
            bridge.emitEvent(Ev.offer(8, {}))
            return 100
        },
        function () {
            test.compare(test.stack.currentPage.offer.offerId, 7)
            test.stack.holdBusy = true
            // Its countdown's second tick, or at worst its third.
            return 2200
        },
        function () {
            test.compare(test.answers()[4], [7, false], "the countdown declined it")
            test.compare(test.names(), ["mainPage", "consentDialog"], "nothing pushed while busy")
            test.stack.holdBusy = false
            return 600
        },
        function () {
            test.compare(test.names(), ["mainPage", "consentDialog"])
            test.compare(test.stack.currentPage.offer.offerId, 8)
            // The engine closes it as the user reaches for Accept: the
            // closed offer is not answered, and the dialog goes.
            test.stack.holdBusy = true
            bridge.emitEvent(Ev.offerClosed(8, "timed_out"))
            return 50
        },
        function () {
            test.verify(!test.stack.currentPage.canAccept, "a closed offer's dialog accepts nothing")
            test.stack.currentPage.accept()
            test.compare(test.answers().length, 5, "no answer for a closed offer")
            test.stack.holdBusy = false
            return 400
        },
        function () {
            test.compare(test.names(), ["mainPage"])
            test.compare(test.engine.answer(8, true), 0, "the engine answers no closed offer either")
            test.compare(test.answers().length, 5)
        }
    ]
}
