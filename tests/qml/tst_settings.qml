// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * Settings (F-C7, F-LS4, F-QS4, F-QS2, F-MW4, S9): loaded from the engine,
 * checked as sukkula-core checks them, saved on the way out only when
 * something changed, with every field the page does not know kept. Then
 * the About page and receiving by wormhole code (F-MW2).
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
            // Leaving unchanged saves nothing.
            window.pageStack.pop()
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
            test.field("deviceNameField").text = "  Pekka  "
            test.field("loggingSwitch").click()
            test.field("visibilityBox").choose(1)
            window.pageStack.pop()
        },
        function () {
            var saved = test.commandsOfType("set_settings")
            test.compare(saved.length, 1, "saved once, on the way out")
            test.compare(saved[0].settings, {
                device_name: "Pekka",
                localsend: { enabled: true, pin: "4711" },
                quickshare: { enabled: true, visibility: "hidden", ble_nudge: true },
                wormhole: { mailbox_url: "WSS://relay.example/v1", relay_url: null },
                bluetooth: { enabled: true },
                logging: true
            })
            // Fields this page does not know are kept, not reset.
            bridge.emitEvent(Ev.settings({ future_field: { x: 1 } }))
            test.page = null
        },
        function () {
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
            test.field("deviceNameField").text = "Other"
            window.pageStack.pop()
            var saved = test.commandsOfType("set_settings")
            test.compare(saved[saved.length - 1].settings.future_field, { x: 1 })
            test.compare(saved[saved.length - 1].settings.device_name, "Other")
            // A refusal from the engine reaches the main page's banner.
            var cmds = bridge.parsedCommands()
            bridge.autoReply = false
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
            test.field("deviceNameField").text = "Third"
            window.pageStack.pop()
            cmds = bridge.parsedCommands()
            bridge.emitEvent(Ev.reply(cmds[cmds.length - 1].id, false, "bad_settings"))
        },
        function () {
            var main = window.pageStack.currentPage
            test.compare(probe.find(main, "bannerLabel").text, "A setting could not be used.")
            bridge.autoReply = true
            // A protocol that failed shows why, with the engine's detail.
            bridge.emitEvent(Ev.receiving(true, true))
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/SettingsPage.qml"), { engine: engine })
        },
        function () {
            var all = probe.texts(test.page).join("\n")
            test.verify(all.indexOf("LocalSend could not start: Network error or timeout.") >= 0, all)
            test.verify(all.indexOf("port 53317 in use") >= 0, "the detail line")
            window.pageStack.pop()
            // About.
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/AboutPage.qml"), { engine: engine })
        },
        function () {
            test.compare(test.field("aboutVersion").text, "Version 9.9.9-fake")
            var all = probe.texts(test.page).join("\n")
            test.verify(all.indexOf("GPL-3.0-or-later") >= 0, "the licence")
            test.verify(all.indexOf("LocalSend") >= 0 && all.indexOf("magic-wormhole") >= 0
                        && all.indexOf("open-quickshare") >= 0, "the upstreams")
            window.pageStack.pop()
            // Receiving by code (F-MW2).
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/WormholeReceivePage.qml"),
                                              { engine: engine })
        },
        function () {
            var field = test.field("codeField")
            var button = test.field("receiveButton")
            test.verify(!button.enabled, "nothing typed")
            var bad = ["../../etc/passwd", "guitarist-revenge", "7", "7-", "7--a", "-7-a", "7-a/b", "7-ää"]
            for (var i = 0; i < bad.length; i++) {
                field.text = bad[i]
                test.verify(!test.page.valid, "refused: " + bad[i])
            }
            field.text = "  7 Guitarist Revenge "
            test.verify(test.page.valid, "spoken form")
            test.compare(test.page.code, "7-guitarist-revenge")
            button.clicked()
            test.compare(test.commandsOfType("receive_wormhole"),
                         [{ type: "receive_wormhole", code: "7-guitarist-revenge" }])
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "mainPage",
                         "back to the main page; the consent dialog does the rest")
        }
    ]
}
