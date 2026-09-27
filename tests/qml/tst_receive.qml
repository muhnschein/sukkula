// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../../qml/engine"
import "helpers"
import "helpers/Events.js" as Ev

/*
 * The Receive tab's radar (F-C1, F-C2, F-C5): a device that offers comes
 * onto the rings with its name as plain text; its accepted transfer takes
 * the offer's place -- whichever of the two events comes first -- draws a
 * line to this phone, fills it and the sender, can be cancelled, and
 * leaves a moment after it ends, or when tapped; a declined offer leaves
 * at once; what comes over the internet comes from its tile through the
 * cloud; and the cloud's tiles open the page to type a code on, for
 * Magic Wormhole or croc, and each goes with its protocol switched off,
 * the cloud with both.
 */
Script {
    id: test

    property Item main: null
    property Item view: null

    ApplicationWindow {
        id: window
    }

    Engine {
        id: engine
        backend: bridge
    }

    function find(name) {
        return probe.find(test.view, name)
    }

    function shown(name) {
        var out = []
        var all = probe.findAll(test.view, name)
        for (var i = 0; i < all.length; i++) {
            if (all[i].visible && (!all[i].parent || all[i].parent.visible)) {
                out.push(all[i])
            }
        }
        return out
    }

    function lastOf(type) {
        var cmds = bridge.parsedCommands()
        for (var i = cmds.length - 1; i >= 0; i--) {
            if (cmds[i].cmd.type === type) {
                return cmds[i].cmd
            }
        }
        return null
    }

    steps: [
        function () {
            test.main = window.pageStack.push(Qt.resolvedUrl("../../qml/pages/MainPage.qml"), { engine: engine })
            test.view = probe.find(test.main, "receiveView")
            test.view.linger = 60
            bridge.emitEvent(Ev.receiving(true))
        },
        function () {
            test.compare(probe.find(test.main, "modePager").currentIndex, 1, "the Receive tab")
            test.verify(test.view.pulsing)
            test.verify(test.find("receiveStatus").visible, "waiting")
            test.compare(test.find("visibleVia").text, "Visible over LocalSend, Quick Share")
            test.verify(test.find("cloud").visible, "the cloud")
            test.compare(test.find("cloudLabel").text, "Receive with a code")
            test.verify(!test.find("wormholeTile").visible)
            test.verify(!test.find("crocTile").visible)
            // An offer: the sender on the rings while the dialog asks.
            bridge.emitEvent(Ev.offer(7, { protocol: "local_send", sender: Ev.EVIL_NAME }))
        },
        function () {
            var offers = test.shown("offerBubble")
            test.compare(offers.length, 1)
            test.compare(offers[0].name, Ev.EVIL_NAME)
            test.compare(offers[0].protocol, "local_send", "with its badge")
            test.compare(test.view.slots[0], "offer:7", "in the first place")
            test.verify(!test.find("receiveStatus").visible, "the waiting line gives way")
            test.verifyPlainText(test.main, "the receive radar with an offer")
            // A second offer, and the first declined: the first place is
            // free again.
            bridge.emitEvent(Ev.offer(6, { protocol: "local_send", sender: Ev.EVIL_MODEL }))
            bridge.emitEvent(Ev.offerClosed(7, "declined"))
        },
        function () {
            test.compare(test.view.slots.slice(0, 2), ["", "offer:6"], "nobody moves")
            // Accepted: its transfer starts before the offer is closed.
            bridge.emitEvent(Ev.transferStarted(20, "incoming", { protocol: "local_send", peer: Ev.EVIL_MODEL }))
            bridge.emitEvent(Ev.offerClosed(6, "accepted"))
            bridge.emitEvent(Ev.progress(20, 500, 2000))
        },
        function () {
            test.compare(test.shown("offerBubble").length, 0, "the offer has gone")
            test.compare(test.view.slots.slice(0, 2), ["", "transfer:20"], "its transfer took its place")
            var bubbles = test.shown("incomingBubble")
            test.compare(bubbles.length, 1)
            test.compare(bubbles[0].name, Ev.EVIL_MODEL)
            test.compare(test.find("incomingFrom").text, Ev.EVIL_MODEL, "who, beside this phone")
            test.verifyPlainText(test.main, "the receive radar with a transfer")
            test.compare(bubbles[0].progress, 0.25, "the sender fills")
            test.verify(test.find("receiveFill").visible, "and this phone")
            test.compare(test.find("incomingPercent").text, "25%")
            test.compare(test.find("incomingStatus").text, "500 B of 2.0 kB")
            test.verify(!test.view.pulsing, "the rings stop while it comes")
            test.find("cancelIncoming").clicked()
            test.compare(test.lastOf("cancel"), { type: "cancel", transfer: 20 })
            // Two more senders; the first one's place is taken.
            bridge.emitEvent(Ev.offer(8, { protocol: "quick_share", sender: "Pixel" }))
            bridge.emitEvent(Ev.offer(5, { protocol: "quick_share", sender: "Tablet" }))
        },
        function () {
            test.compare(test.view.slots.slice(0, 3), ["offer:8", "transfer:20", "offer:5"])
            bridge.emitEvent(Ev.offerClosed(8, "declined"))
            // The offer is closed first this time.
            bridge.emitEvent(Ev.offerClosed(5, "accepted"))
            bridge.emitEvent(Ev.transferStarted(21, "incoming", { protocol: "quick_share", peer: "Tablet" }))
        },
        function () {
            test.compare(test.view.slots.slice(0, 3), ["", "transfer:20", "transfer:21"],
                         "the handover kept its place")
            test.compare(test.shown("incomingBubble").length, 2)
            bridge.emitEvent(Ev.finished(20, "done", ["a.jpg"]))
            bridge.emitEvent(Ev.finished(21, "failed", null, "network"))
        },
        function () {
            test.compare(test.shown("incomingBubble").length, 2, "an ended transfer stays a moment")
            test.compare(test.find("incomingStatus").text, "Saved in Downloads/Sukkula")
            test.shown("incomingBubble")[1].clicked()
            test.compare(test.shown("incomingBubble").length, 1, "or goes when tapped")
            return 200
        },
        function () {
            test.compare(test.shown("incomingBubble").length, 0, "and then leaves")
            test.compare(test.view.slots.join(""), "", "every place free")
            test.verify(test.view.pulsing, "the rings pulse again")
            test.verify(test.find("receiveStatus").visible)
            // A declined offer leaves at once.
            bridge.emitEvent(Ev.offer(9, { protocol: "local_send", sender: "Nope" }))
        },
        function () {
            test.compare(test.shown("offerBubble").length, 1)
            bridge.emitEvent(Ev.offerClosed(9, "declined"))
        },
        function () {
            test.compare(test.shown("offerBubble").length, 0)
            // The cloud's tiles: receiving with a code.
            test.find("cloud").clicked()
            test.verify(test.find("wormholeTile").visible, "Magic Wormhole's tile")
            test.verify(test.find("crocTile").visible, "and croc's")
            test.find("wormholeTile").clicked()
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "wormholeReceivePage")
            test.compare(window.pageStack.currentPage.protocol, "wormhole")
            window.pageStack.pop()
            return 50
        },
        function () {
            test.find("crocTile").clicked()
        },
        function () {
            test.compare(window.pageStack.currentPage.objectName, "wormholeReceivePage")
            test.compare(window.pageStack.currentPage.protocol, "croc", "the same page, for croc")
            window.pageStack.pop()
            // What comes over the internet comes from its tile.
            bridge.emitEvent(Ev.offer(10, { protocol: "wormhole", sender: "" }))
        },
        function () {
            var offers = test.shown("offerBubble")
            test.compare(offers.length, 1)
            test.compare(offers[0].x + offers[0].width / 2, test.view.leftTileX, "on Magic Wormhole's side")
            test.verify(!test.find("wormholeTile").visible, "the tiles give way")
            bridge.emitEvent(Ev.offerClosed(10, "accepted"))
            bridge.emitEvent(Ev.transferStarted(22, "incoming", { protocol: "wormhole", peer: "" }))
            bridge.emitEvent(Ev.progress(22, 100, 1000))
        },
        function () {
            var bubbles = test.shown("incomingBubble")
            test.compare(bubbles.length, 1)
            test.compare(bubbles[0].x + bubbles[0].width / 2, test.view.leftTileX, "from the tile")
            test.compare(test.view.slots.join(""), "", "not on the rings")
            test.compare(test.find("incomingPercent").text, "10%")
            bridge.emitEvent(Ev.finished(22, "done"))
            return 200
        },
        function () {
            test.compare(test.shown("incomingBubble").length, 0)
            test.compare(test.find("cloudLabel").text, "Receive with a code", "the cloud's own again")
            // Magic Wormhole switched off: croc keeps the cloud.
            bridge.emitEvent(Ev.settings({ wormhole: { enabled: false, mailbox_url: null, relay_url: null } }))
        },
        function () {
            test.verify(test.find("cloud").visible, "croc still receives with a code")
            test.verify(!test.find("wormholeTile").visible)
            // And croc: no cloud.
            bridge.emitEvent(Ev.settings({ wormhole: { enabled: false, mailbox_url: null, relay_url: null },
                                           croc: { enabled: false, relay: null, password: null } }))
        },
        function () {
            test.verify(!test.find("cloud").visible, "nothing to receive a code over")
            test.verify(!test.find("wormholeTile").visible)
            test.verify(!test.find("crocTile").visible)
        }
    ]
}
