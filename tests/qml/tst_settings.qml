// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * Settings (F-C1, F-C7, F-LS4, F-QS4, F-QS2, F-MW4, F-CR3, S9): loaded
 * from the engine, each way named by who it reaches, how each way of
 * receiving nearby is doing under its switch -- waiting for the Receive
 * tab, ready, or why it could not start, in red with the engine's detail
 * -- an option under a switch greyed while the switch is off, one's own
 * servers folded away until set or wrong, each field saying which server
 * it is even while empty; checked as sukkula-core checks them, saved when
 * the page is left -- not when a page is pushed over it -- and only when
 * something changed, with every field the page does not know kept. Then
 * the About page. Receiving by code is tst_scan.qml's.
 */
Script {
    id: test

    property Item page: null

    ApplicationWindow {
        id: window
    }

    Engine {
        id: engine
        backend: bridge
    }

    function field(name) {
        return probe.find(test.page, name)
    }

    function commandsOfType(type) {
        var out = []
        var cmds = bridge.parsedCommands()
        for (var i = 0; i < cmds.length; i++) {
            if (cmds[i].cmd.type === type) {
                out.push(cmds[i].cmd)
            }
        }
        return out
    }

    steps: [
        function () {
            window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
        },
        function () {
            test.compare(test.field("deviceNameField").text, "")
            test.compare(test.field("deviceNameField").placeholderText, "Jolla Phone", "the model shows through (F-C7)")
            test.compare(test.field("pinField").text, "", "no PIN by default (F-LS4)")
            test.compare(test.field("visibilityBox").currentIndex, 0, "Everyone by default")
            test.compare(test.field("loggingSwitch").checked, false, "logging off by default (S9)")
            test.compare(test.field("wormholeSwitch").checked, true, "Magic Wormhole on by default (F-C1)")
            test.compare(test.field("crocSwitch").checked, true, "croc too")
            test.compare(test.field("crocRelayField").text, "", "croc's own relay by default (F-CR3)")
            test.compare(test.field("crocPasswordField").text, "")
            test.compare(test.field("nudgeSwitch").description,
                         "Makes Android phones nearby show up while you send.",
                         "the nudge said as what it is: a sending aid (F-QS2)")
            test.compare(test.field("quickShareSwitch").text, "Android phones", "Quick Share by who it reaches")
            test.compare(test.field("localSendSwitch").text, "Computers and other phones", "LocalSend too")
            test.compare(test.field("localSendSwitch").description,
                         "LocalSend, on the same Wi-Fi\nReceives while the Receive tab is open",
                         "the protocol only in the grey line, with how it is doing")
            test.compare(test.field("quickShareSwitch").description,
                         "Quick Share, on the same Wi-Fi\nReceives while the Receive tab is open")
            test.compare(probe.findAll(test.page, "protocolFailed").length, 0, "nothing failed")
            // An option under a switch is greyed while the switch is off.
            test.verify(test.field("pinField").enabled && test.field("nudgeSwitch").enabled)
            test.field("localSendSwitch").click()
            test.field("quickShareSwitch").click()
            test.verify(!test.field("pinField").enabled, "no PIN without LocalSend")
            test.verify(!test.field("nudgeSwitch").enabled && !test.field("visibilityBox").enabled,
                        "no nudge or visibility without Quick Share")
            test.field("localSendSwitch").click()
            test.field("quickShareSwitch").click()
            // One's own servers are folded away until asked for.
            test.verify(!test.page.serversOpen && !test.field("mailboxField").visible
                        && !test.field("crocRelayField").visible, "the servers folded away")
            probe.find(test.page, "serversToggle").clicked()
            test.verify(test.field("mailboxField").visible && test.field("relayField").visible
                        && test.field("crocRelayField").visible && test.field("crocPasswordField").visible,
                        "and unfolded with a tap")
            test.verify(test.field("serversNote").visible, "saying what an empty field means, once")
            // Each empty field says which server it is: Silica hides the
            // label of an empty field.
            test.compare([test.field("mailboxField").placeholderText, test.field("relayField").placeholderText,
                          test.field("crocRelayField").placeholderText,
                          test.field("crocPasswordField").placeholderText],
                         ["Magic Wormhole mailbox server", "Magic Wormhole transit relay", "croc relay",
                          "croc relay password"])
            // Leaving unchanged saves nothing.
            window.pageStack.pop()
            return 50
        },
        function () {
            test.compare(test.commandsOfType("set_settings").length, 0)
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
        },
        function () {
            // What sukkula-core refuses, refused here first.
            test.field("pinField").text = "12 34"
            test.verify(!test.page.valid, "a PIN with a space")
            test.verify(!test.page.backNavigation, "cannot leave with it")
            test.field("pinField").text = "12345678901234567"
            test.compare(test.field("pinField").text.length, 16, "the field holds 16 at most")
            test.field("pinField").text = "4711"
            test.verify(test.page.pinValid, "four digits")
            test.field("mailboxField").text = "http://evil.example"
            test.verify(!test.page.mailboxValid, "http is not a mailbox scheme")
            test.field("mailboxField").text = "wss://"
            test.verify(!test.page.mailboxValid, "a scheme alone")
            test.field("mailboxField").text = "wss://relay.example/v1 x"
            test.verify(!test.page.mailboxValid, "a space")
            test.field("mailboxField").text = "  WSS://relay.example/v1 "
            test.verify(test.page.mailboxValid, "case and outer spaces are fine")
            test.field("relayField").text = "tcp://relay.example:4001"
            test.verify(test.page.relayValid)
            test.field("relayField").text = "udp://x"
            test.verify(!test.page.relayValid, "tcp:// only")
            test.field("relayField").text = ""
            test.verify(test.page.valid, "all good again")
            // croc's relay: host, host:port, [v6] or [v6]:port.
            var relays = { "croc.example.org": true, "croc.example.org:9009": true, "[::1]:9009": true,
                           "[2001:db8::1]": true, " 10.0.0.2:9009 ": true, "croc.example.org:": false,
                           "croc.example.org:0": false, "croc.example.org:65536": false,
                           "tcp://croc.example.org": false, "bad host": false, "a/b": false,
                           "host:9009:1": false, "[::1]x": false }
            for (var r in relays) {
                test.field("crocRelayField").text = r
                test.compare(test.page.crocRelayValid, relays[r], "relay " + r)
            }
            test.field("crocRelayField").text = "croc.example.org:9009"
            test.field("crocPasswordField").text = "pass 123"
            test.verify(test.page.crocPasswordValid, "printable, spaces too")
            test.field("crocPasswordField").text = "pässword"
            test.verify(!test.page.crocPasswordValid, "ASCII only")
            test.verify(!test.page.valid)
            test.field("crocPasswordField").text = " s3cret "
            test.verify(test.page.valid)
            test.field("deviceNameField").text = "  Pekka  "
            test.field("loggingSwitch").click()
            test.field("wormholeSwitch").click()
            test.field("visibilityBox").choose(1)
            // A page over this one is not leaving it: nothing is saved --
            // the consent dialog comes up over Settings the same way.
            window.pageStack.push(Qt.resolvedUrl("../../qml/pages/AboutPage.qml"), { engine: engine })
            return 50
        },
        function () {
            test.compare(test.commandsOfType("set_settings").length, 0, "covered is not left")
            window.pageStack.pop()
            return 50
        },
        function () {
            test.verify(window.pageStack.currentPage === test.page, "Settings again")
            test.compare(test.commandsOfType("set_settings").length, 0, "uncovered is not left either")
            window.pageStack.pop()
            return 50
        },
        function () {
            var saved = test.commandsOfType("set_settings")
            test.compare(saved.length, 1, "saved once, on the way out")
            test.compare(saved[0].settings, {
                device_name: "Pekka",
                localsend: { enabled: true, pin: "4711" },
                quickshare: { enabled: true, visibility: "hidden", ble_nudge: true },
                wormhole: { enabled: false, mailbox_url: "WSS://relay.example/v1", relay_url: null },
                croc: { enabled: true, relay: "croc.example.org:9009", password: "s3cret" },
                bluetooth: { enabled: true },
                logging: true
            })
            // Fields this page does not know are kept, not reset.
            test.page = null
        },
        function () {
            // A server set is shown: the fields come unfolded.
            test.verify(test.page === null)
            var s = JSON.parse(JSON.stringify(engine.settings))
            test.compare(s.croc.relay, null, "the engine's copy: nothing saved went back to it")
            bridge.emitEvent(Ev.settings({ croc: { enabled: true, relay: "croc.example.org:9009", password: null } }))
        },
        function () {
            var page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
            test.verify(page.serversOpen && probe.find(page, "crocRelayField").visible, "a relay set is in view")
            window.pageStack.pop()
            bridge.emitEvent(Ev.settings({ future_field: { x: 1 } }))
            return 50
        },
        function () {
            test.compare(test.commandsOfType("set_settings").length, 1, "an unchanged page saved nothing")
            // A wrong server unfolds them, so it can be seen.
            var page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
            test.verify(!page.serversOpen)
            probe.find(page, "mailboxField").text = "http://x"
            test.verify(page.serversOpen, "a wrong server unfolds them")
            probe.find(page, "mailboxField").text = ""
            test.page = page
            test.field("deviceNameField").text = "Other"
            window.pageStack.pop()
            return 50
        },
        function () {
            var saved = test.commandsOfType("set_settings")
            test.compare(saved[saved.length - 1].settings.future_field, { x: 1 })
            test.compare(saved[saved.length - 1].settings.device_name, "Other")
            // A refusal from the engine reaches the main page's banner.
            bridge.autoReply = false
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
            test.field("deviceNameField").text = "Third"
            window.pageStack.pop()
            return 50
        },
        function () {
            var cmds = bridge.parsedCommands()
            test.compare(cmds[cmds.length - 1].cmd.type, "set_settings")
            bridge.emitEvent(Ev.reply(cmds[cmds.length - 1].id, false, "bad_settings"))
        },
        function () {
            var main = window.pageStack.currentPage
            test.compare(probe.find(main, "bannerLabel").text, "A setting is not valid.")
            bridge.autoReply = true
            // While receiving: how each way is doing, under its switch; a
            // way that failed says why, in red, with the engine's detail.
            bridge.emitEvent(Ev.receiving(true, true))
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
        },
        function () {
            test.compare(test.field("quickShareSwitch").description, "Quick Share, on the same Wi-Fi\nReady to receive")
            test.compare(test.field("localSendSwitch").description, "LocalSend, on the same Wi-Fi",
                         "a failure is not a grey word")
            var failed = probe.findAll(test.page, "protocolFailed")
            test.compare(failed.length, 1, "LocalSend's, only")
            var line = probe.find(failed[0], "protocolFailedLine")
            test.compare(line.text, "Could not start: The connection failed or timed out.")
            test.verify(line.color === Theme.errorColor, "in red")
            test.verify(probe.texts(failed[0]).join("\n").indexOf("port 53317 in use") >= 0, "the detail line")
            var order = failed[0].parent.children
            var at = function (item) {
                for (var i = 0; i < order.length; i++) {
                    if (order[i] === item) {
                        return i
                    }
                }
                return -1
            }
            test.verify(at(failed[0]) > at(test.field("localSendSwitch")) && at(failed[0]) < at(test.field("pinField")),
                        "under its switch, over its options")
            // Switched off here: no status for it any more.
            test.field("localSendSwitch").click()
            test.compare(probe.findAll(test.page, "protocolFailed").length, 0)
            test.compare(test.field("localSendSwitch").description, "LocalSend, on the same Wi-Fi")
            test.field("localSendSwitch").click()
            window.pageStack.pop()
            // About.
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/AboutPage.qml"), { engine: engine })
        },
        function () {
            test.compare(test.field("aboutVersion").text, "Version 9.9.9-fake")
            var all = probe.texts(test.page).join("\n")
            test.verify(all.indexOf("GPL-3.0-or-later") >= 0, "the licence")
            test.verify(all.indexOf("LocalSend") >= 0 && all.indexOf("magic-wormhole") >= 0
                        && all.indexOf("open-quickshare") >= 0 && all.indexOf("croc") >= 0, "the upstreams")
            window.pageStack.pop()
        }
    ]
}
