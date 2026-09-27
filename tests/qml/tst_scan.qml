// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * Scanning a code (spec v0.6, F-MW2, F-CR2): the receive page's Scan
 * button opens the camera page, which hands the viewfinder to the scanner
 * a frame at a time, and only while it is on top, the app in front and
 * the camera there; a frame with nothing in it, JSON that is not the
 * engine's, and a QR code with anything but a code in it keep it
 * scanning, the last said as no code and never shown; the first code
 * read goes back to the receive page, which receives with it -- over the
 * protocol the code is for, with the mailbox server its QR code named as
 * long as the code in the field is still the one scanned -- and a code for
 * a protocol switched off is refused (F-C1). Without a scanner there is no
 * Scan button.
 */
Script {
    id: test

    property Item page: null
    property Item scan: null
    // The runner's `scanner`, under a name the Engine's own property does
    // not hide.
    readonly property QtObject fake: scanner

    ApplicationWindow {
        id: window
    }

    Engine {
        id: engine
        backend: bridge
        scanner: test.fake
    }

    // No backend either: one engine on the fake bridge is enough.
    Engine {
        id: noScanner
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

    function openScan(protocol) {
        test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/WormholeReceivePage.qml"),
                                          { engine: engine, protocol: protocol })
    }

    function hint() {
        return probe.find(test.scan, "scanHint").text
    }

    steps: [
        function () {
            window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.openScan("wormhole")
        },
        function () {
            var button = probe.find(test.page, "scanButton")
            test.verify(button.visible && button.enabled, "the Scan button")
            button.clicked()
            return 50
        },
        function () {
            test.scan = window.pageStack.currentPage
            test.compare(test.scan.objectName, "scanPage")
            test.scan.foreground = true
            test.compare(test.hint(), "Point the camera at the QR code on the sender's screen.")
            return 400
        },
        function () {
            test.compare(test.fake.scans, 1, "one frame in hand, and no second one asked for")
            test.compare(test.fake.lastItem.objectName, "viewfinder", "the viewfinder is what is scanned")
            test.compare(probe.find(test.scan, "camera").cameraState, 2, "the camera on")
            // Nothing read.
            test.fake.answer("")
            return 400
        },
        function () {
            test.verify(test.fake.scans >= 2, "the next frame")
            // JSON that is no scan result: nothing happens.
            test.fake.answer('{"found":"croc","code":"x"}')
            return 50
        },
        function () {
            test.verify(window.pageStack.currentPage === test.scan, "still scanning")
            test.compare(test.hint(), "Point the camera at the QR code on the sender's screen.")
            return 400
        },
        function () {
            // A QR code with a web address in it: said to be no code, and
            // what it holds is shown nowhere.
            test.fake.answer('{"found":"other"}')
            return 50
        },
        function () {
            test.verify(window.pageStack.currentPage === test.scan, "still scanning")
            test.compare(test.hint(), "That QR code holds no Magic Wormhole or croc code.")
            test.compare(test.commandsOfType("receive_wormhole").length, 0)
            // In the background: the camera off, no frames.
            test.scan.foreground = false
            return 50
        },
        function () {
            test.compare(probe.find(test.scan, "camera").cameraState, 0, "the camera off in the background")
            test.test_scans = test.fake.scans
            if (test.fake.busy) {
                test.fake.answer("")
            }
            return 600
        },
        function () {
            test.compare(test.fake.scans, test.test_scans, "no frame while in the background")
            test.scan.foreground = true
            return 400
        },
        function () {
            test.verify(test.fake.scans > test.test_scans, "scanning again in front")
            // A Magic Wormhole code, from a QR code naming its mailbox.
            test.fake.answer('{"found":"wormhole","code":"7-guitarist-revenge","mailbox_url":"wss://mailbox.example/v1"}')
            return 100
        },
        function () {
            test.compare(test.commandsOfType("receive_wormhole"),
                         [{ type: "receive_wormhole", code: "7-guitarist-revenge",
                            mailbox_url: "wss://mailbox.example/v1" }])
            test.compare(window.pageStack.currentPage.objectName, "mainPage",
                         "the camera page gone, the receive page after it")
            // A croc code scanned on Magic Wormhole's page receives over croc.
            bridge.clearCommands()
            test.openScan("wormhole")
        },
        function () {
            probe.find(test.page, "scanButton").clicked()
            return 50
        },
        function () {
            test.scan = window.pageStack.currentPage
            test.scan.foreground = true
            return 400
        },
        function () {
            if (!test.fake.busy) {
                test.fail("no frame in hand")
            }
            test.fake.answer('{"found":"croc","code":"gala-tulip-acorn"}')
            return 100
        },
        function () {
            test.compare(test.commandsOfType("receive_croc"),
                         [{ type: "receive_croc", code: "gala-tulip-acorn" }])
            test.compare(test.commandsOfType("receive_wormhole").length, 0)
            test.compare(window.pageStack.currentPage.objectName, "mainPage")
            // croc switched off: its code is refused, with a word why.
            bridge.clearCommands()
            bridge.emitEvent(Ev.settings({ croc: { enabled: false, relay: null, password: null } }))
            test.openScan("wormhole")
        },
        function () {
            probe.find(test.page, "scanButton").clicked()
            return 50
        },
        function () {
            test.scan = window.pageStack.currentPage
            test.scan.foreground = true
            return 400
        },
        function () {
            test.fake.answer('{"found":"croc","code":"gala-tulip-acorn"}')
            return 100
        },
        function () {
            test.compare(test.commandsOfType("receive_croc").length, 0, "croc is off (F-C1)")
            test.verify(window.pageStack.currentPage === test.page, "back on the receive page")
            test.verify(probe.texts(test.page).join("\n").indexOf("croc is switched off in Settings") >= 0,
                        "said why")
            test.compare(probe.find(test.page, "codeField").text, "", "the code not taken")
            bridge.emitEvent(Ev.settings({}))
            // A scanned mailbox goes with the scanned code only: the send
            // fails, the user edits the code, and it goes without.
            bridge.commandResult = -1
            probe.find(test.page, "scanButton").clicked()
            return 50
        },
        function () {
            test.scan = window.pageStack.currentPage
            test.scan.foreground = true
            return 400
        },
        function () {
            test.fake.answer('{"found":"wormhole","code":"7-guitarist-revenge","mailbox_url":"wss://mailbox.example/v1"}')
            return 100
        },
        function () {
            test.verify(window.pageStack.currentPage === test.page, "the command failed: still here")
            test.compare(test.commandsOfType("receive_wormhole")[0].mailbox_url, "wss://mailbox.example/v1")
            bridge.commandResult = 0
            bridge.clearCommands()
            probe.find(test.page, "codeField").text = "8-guitarist-revenge"
            probe.find(test.page, "receiveButton").clicked()
            test.compare(test.commandsOfType("receive_wormhole"),
                         [{ type: "receive_wormhole", code: "8-guitarist-revenge" }], "no mailbox for another code")
            return 100
        },
        function () {
            // The camera missing: said, and nothing scanned.
            test.openScan("croc")
        },
        function () {
            probe.find(test.page, "scanButton").clicked()
            return 50
        },
        function () {
            test.scan = window.pageStack.currentPage
            test.scan.foreground = true
            if (test.fake.busy) {
                test.fake.answer("")
            }
            probe.find(test.scan, "camera").availability = 1
            test.test_scans = test.fake.scans
            return 600
        },
        function () {
            test.compare(test.hint(), "The camera is not available.")
            test.compare(test.fake.scans, test.test_scans, "nothing scanned without a camera")
            window.pageStack.pop()
            window.pageStack.pop()
            return 50
        },
        function () {
            // No scanner: no Scan button.
            test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/WormholeReceivePage.qml"),
                                              { engine: noScanner })
        },
        function () {
            test.verify(!probe.find(test.page, "scanButton").visible, "nothing to scan with")
        }
    ]

    property int test_scans: 0
}
