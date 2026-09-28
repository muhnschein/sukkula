// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * Receiving with a code (spec v0.6, F-MW2, F-CR2): the scan page the
 * Receive tab's QR code opens. The viewfinder is handed to the scanner a
 * frame at a time, and only while the page is on top, the app in front
 * and the camera there, with the camera asked for a big enough picture
 * and to focus; a frame with nothing in it, JSON that is not the
 * engine's, and a QR code of anything else keep it reading, the last
 * said as no code and never shown. The first code read is received over
 * the protocol it is for, with the mailbox its QR code named, and the
 * page goes once the offer is answered; a code for a protocol switched
 * off is refused with a word why (F-C1). Without a camera, the page
 * leads to the one for typing the code in, in its place. That page has
 * the keyboard up and the clipboard in the field when it holds a code,
 * and the engine tells the typed code's protocol (receive_code); a failed
 * receive says why and the code stays. Without a scanner, typing still
 * works.
 */
Script {
    id: test

    property Item page: null
    property Item view: null
    property int scansBefore: 0
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

    function open(withEngine) {
        test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/ScanPage.qml"),
                                          { engine: withEngine ? withEngine : engine })
    }

    function openTyping(withEngine) {
        test.page = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/TypeCodePage.qml"),
                                          { engine: withEngine ? withEngine : engine })
    }

    function find(name) {
        return probe.find(test.page, name)
    }

    function hint() {
        return test.find("scanHint").text
    }

    /// Answers a frame in hand, if there is one.
    function drain() {
        if (test.fake.busy) {
            test.fake.answer("")
        }
    }

    steps: [
        function () {
            window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.open()
        },
        function () {
            test.page.foreground = true
            test.view = test.find("scanLoader").item
            test.verify(test.view !== null, "the viewfinder loaded")
            test.compare(test.hint(), "Point the camera at the sender's QR code")
            test.compare(probe.find(test.page, "pageHeaderTitle").text, "Scan a QR code")
            test.verify(!test.find("typeInstead").visible, "typing is a page of its own")
            return 400
        },
        function () {
            var camera = test.find("camera")
            test.compare(camera.cameraState, 2, "the camera on")
            test.compare(camera.captureMode, 2, "in video mode, where autofocus runs")
            test.compare(camera.viewfinder.resolution, Qt.size(1280, 720), "the smallest picture big enough")
            test.verify(camera.searches > 0, "asked to focus")
            test.compare(test.fake.scans, 1, "one frame in hand, and no second one asked for")
            test.compare(test.fake.lastItem.objectName, "viewfinder", "the viewfinder is what is scanned")
            // Nothing read.
            test.fake.answer("")
            return 400
        },
        function () {
            test.verify(test.fake.scans >= 2, "the next frame")
            test.verify(test.find("looking").running, "looking, and saying so")
            // JSON that is no scan result: nothing happens.
            test.fake.answer('{"found":"croc","code":"x"}')
            return 400
        },
        function () {
            test.verify(window.pageStack.currentPage === test.page, "still scanning")
            // A QR code with a web address in it: said to be no code, and
            // what it holds is shown nowhere.
            test.fake.answer('{"found":"other"}')
            return 50
        },
        function () {
            test.verify(window.pageStack.currentPage === test.page, "still scanning")
            test.compare(test.hint(), "That QR code holds no Magic Wormhole or croc code.")
            test.compare(test.commandsOfType("receive_wormhole").length, 0, "nothing received")
            // In the background: the camera off, no frames.
            test.page.foreground = false
            return 50
        },
        function () {
            test.compare(test.find("camera").cameraState, 0, "the camera off in the background")
            test.drain()
            test.scansBefore = test.fake.scans
            return 600
        },
        function () {
            test.compare(test.fake.scans, test.scansBefore, "no frame while in the background")
            test.page.foreground = true
            return 400
        },
        function () {
            test.verify(test.fake.scans > test.scansBefore, "reading again in front")
            // A Magic Wormhole code, from a QR code naming its mailbox.
            test.fake.answer('{"found":"wormhole","code":"7-guitarist-revenge","mailbox_url":"wss://mailbox.example/v1"}')
            return 150
        },
        function () {
            test.compare(test.commandsOfType("receive_wormhole"),
                         [{ type: "receive_wormhole", code: "7-guitarist-revenge",
                            mailbox_url: "wss://mailbox.example/v1" }])
            test.compare(window.pageStack.currentPage.objectName, "mainPage",
                         "the page gone once the receive was answered")
            // A croc code: over croc, nobody asked which.
            bridge.clearCommands()
            test.open()
        },
        function () {
            test.page.foreground = true
            return 400
        },
        function () {
            test.verify(test.fake.busy, "a frame in hand")
            test.fake.answer('{"found":"croc","code":"gala-tulip-acorn"}')
            return 150
        },
        function () {
            test.compare(test.commandsOfType("receive_croc"), [{ type: "receive_croc", code: "gala-tulip-acorn" }])
            test.compare(test.commandsOfType("receive_wormhole").length, 0)
            test.compare(window.pageStack.currentPage.objectName, "mainPage")
            // croc switched off: its code is refused, with a word why.
            bridge.clearCommands()
            bridge.emitEvent(Ev.settings({ croc: { enabled: false, relay: null, password: null } }))
            test.open()
        },
        function () {
            test.page.foreground = true
            return 400
        },
        function () {
            test.fake.answer('{"found":"croc","code":"gala-tulip-acorn"}')
            return 150
        },
        function () {
            test.compare(test.commandsOfType("receive_croc").length, 0, "croc is off (F-C1)")
            test.verify(window.pageStack.currentPage === test.page, "still here")
            test.verify(probe.texts(test.page).join("\n").indexOf("croc is switched off in Settings") >= 0,
                        "said why")
            test.verify(!test.find("scanLoader").item.done, "and reading again")
            bridge.emitEvent(Ev.settings({}))
            window.pageStack.pop()
            // The code typed in: the clipboard offered when it holds a
            // code, and not when it holds anything else.
            Clipboard.text = "milk, eggs, bread"
            test.openTyping()
        },
        function () {
            test.compare(test.page.objectName, "typeCodePage")
            test.compare(probe.find(test.page, "pageHeaderTitle").text, "Type in a code")
            test.compare(test.find("codeField").text, "", "a shopping list is not pasted")
            test.compare(test.find("codeField").label, "Code")
            test.verify(test.page.focused, "the keyboard asked for as the page comes")
            test.verify(!test.find("receiveButton").enabled, "nothing to receive with yet")
            test.find("codeField").text = "  7 Guitarist revenge "
            test.verify(test.find("receiveButton").enabled)
            // A receive that fails says why, and the code stays.
            bridge.commandResult = -1
            test.find("receiveButton").clicked()
            return 100
        },
        function () {
            test.compare(test.commandsOfType("receive_code"), [{ type: "receive_code", code: "7 Guitarist revenge" }],
                         "the code as typed: the engine tells its protocol")
            test.verify(window.pageStack.currentPage === test.page, "failed: still here")
            test.compare(test.find("codeField").text, "  7 Guitarist revenge ", "the code stays")
            test.verify(probe.texts(test.page).join("\n").indexOf("Not available") >= 0, "said why")
            bridge.commandResult = 0
            test.find("receiveButton").clicked()
            return 150
        },
        function () {
            test.compare(test.commandsOfType("receive_code").length, 2)
            test.compare(window.pageStack.currentPage.objectName, "mainPage")
            Clipboard.text = " Gala tulip acorn\n"
            test.openTyping()
        },
        function () {
            test.compare(test.find("codeField").text, "Gala tulip acorn", "a code on the clipboard, offered")
            test.compare(test.find("codeField").label, "Code, from the clipboard")
            test.find("codeField").text = "Gala tulip acorns"
            test.compare(test.find("codeField").label, "Code", "changed: no longer the clipboard's")
            window.pageStack.pop()
            return 50
        },
        function () {
            // The camera missing: said, and nothing scanned.
            test.open()
        },
        function () {
            test.page.foreground = true
            test.drain()
            test.find("camera").availability = 1
            test.scansBefore = test.fake.scans
            return 600
        },
        function () {
            test.compare(test.hint(), "The camera is not available.")
            test.compare(test.fake.scans, test.scansBefore, "nothing scanned without a camera")
            test.verify(test.find("typeInstead").visible, "the code can still be typed in")
            test.find("typeInstead").clicked()
            return 50
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "typeCodePage", "in the scan page's place")
            test.compare(window.pageStack.previousPage(window.pageStack.currentPage).objectName, "mainPage")
            window.pageStack.pop()
            return 50
        },
        function () {
            // No scanner: the code can still be typed in.
            test.openTyping(noScanner)
        },
        function () {
            test.verify(test.find("codeField").visible, "typing without a scanner")
        }
    ]
}
