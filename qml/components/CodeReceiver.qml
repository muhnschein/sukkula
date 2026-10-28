// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A receive with a code, for the page that took the code: ScanPage, which
 * read it off the sender's screen, or TypeCodePage, where it was typed in
 * (spec v0.6, F-MW2, F-CR2). The offer comes up in the consent dialog like
 * any other, before any data flows, and the reply to the receive comes
 * once it has been answered.
 *
 * Then the page goes back to the main page -- the page only, and from the
 * top only: the reply comes while the consent dialog is over the page,
 * and a bare pop() took the dialog, declining its offer unanswered. A
 * receive that failed says why on `failed`, and the page stays.
 */
Item {
    id: receiver

    /// The page that took the code. Not called `page`: that name would
    /// hide the caller's `id: page` in `page: page`.
    property Item host
    property QtObject engine
    /// From the code to the answered offer.
    property bool busy: false
    /// The code was taken: the page goes once it is on top.
    property bool leaving: false
    property bool alive: true

    /// What went wrong, in words.
    signal failed(string message)

    visible: false

    Component.onDestruction: receiver.alive = false

    /// `send` sends the receive command with the reply callback it is
    /// given.
    function receive(send) {
        receiver.busy = true
        var self = receiver
        send(function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.busy = false
            if (ok) {
                self.leaving = true
                self.leave()
            } else {
                self.failed(self.engine.errorText(error))
            }
        })
    }

    function leave() {
        if (!receiver.leaving || receiver.alive !== true || !receiver.host) {
            return
        }
        if (pageStack.currentPage !== receiver.host || receiver.host.status !== PageStatus.Active) {
            return
        }
        if (pageStack.busy) {
            leaveLater.restart()
            return
        }
        receiver.leaving = false
        pageStack.pop(pageStack.previousPage(receiver.host))
    }

    Connections {
        target: receiver.host
        // Qt 5.6 handler syntax.
        onStatusChanged: {
            if (receiver.host.status === PageStatus.Active) {
                receiver.leave()
            }
        }
    }

    Timer {
        id: leaveLater
        interval: 100
        onTriggered: receiver.leave()
    }
}
