// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import Nemo.KeepAlive 1.2
import Nemo.Notifications 1.0
import "engine"
import "pages"
import "cover"

/*
 * The window: the engine, the pages, and the three things that belong to
 * the app rather than to any page -- the consent dialog, which comes up
 * over whatever is showing whenever an offer waits (F-C2); the Share menu,
 * whose files go to the main page's send radar (F-C6); and keeping the CPU
 * awake while, and only while, a transfer runs (spec §2).
 *
 * `bridge` is the C++ Bridge main.cpp puts in the root context.
 */
ApplicationWindow {
    id: appWindow

    initialPage: Component {
        MainPage {
            engine: sukkula
        }
    }
    cover: Component {
        CoverPage {
            engine: sukkula
        }
    }
    allowedOrientations: defaultAllowedOrientations

    Engine {
        id: sukkula
        backend: bridge

        onOfferArrived: appWindow.navigate()
        onTransferEnded: appWindow.transferEnded(direction, state, transferId)
    }

    // Spec §2: the CPU stays awake during an active transfer only.
    KeepAlive {
        id: keepAlive
        objectName: "keepAlive"
        enabled: sukkula.activeTransfers > 0
    }

    /// The consent dialog in the stack, or null. Cleared only once the
    /// dialog has left the stack (consentGone), so nothing -- the next
    /// offer's dialog, a share -- is ever pushed over one.
    property Item consentPage: null
    /// What the Share menu handed over, waiting for the stack.
    property var pendingShare: null

    // Writable only so the host tests can play the app in the background.
    property bool applicationActive: Qt.application.state === Qt.ApplicationActive

    // In front, the dialog speaks for itself.
    onApplicationActiveChanged: {
        if (appWindow.applicationActive) {
            offerNote.close()
        }
    }

    /// Brings up whatever is waiting, once the page stack can take it:
    /// first an offer, which is on a clock (F-C3), then a share.
    function navigate() {
        if (pageStack.busy) {
            navigation.restart()
            return
        }
        if (appWindow.consentPage === null && sukkula.offers.count > 0) {
            var offer = sukkula.firstOffer()
            if (offer === null) {
                return
            }
            appWindow.consentPage = pageStack.push(Qt.resolvedUrl("pages/ConsentDialog.qml"),
                                                   { engine: sukkula, offer: offer })
            if (appWindow.consentPage) {
                appWindow.consentPage.gone.connect(appWindow.consentGone)
            }
            return
        }
        if (appWindow.consentPage === null && appWindow.pendingShare !== null) {
            var main = pageStack.find(function (page) { return page.objectName === "mainPage" })
            if (!main) {
                return
            }
            var items = appWindow.pendingShare
            appWindow.pendingShare = null
            // Whatever was above the main page goes: the share lands at
            // the centre of its send radar.
            if (pageStack.currentPage !== main) {
                pageStack.pop(main)
            }
            main.share(items)
        }
    }

    /// The dialog has left the stack: the next offer or share may come.
    /// Not as soon as it is answered: an answered dialog -- one the engine
    /// closed above all -- is still on the stack until its own way out
    /// (ConsentDialog's closer, or Silica's pop) is done, and a dialog
    /// pushed over it meanwhile left it stranded underneath, a stale offer
    /// with a frozen countdown that came back once the new one was
    /// answered. The next push waits for the timer: this runs inside the
    /// dialog's destruction.
    function consentGone() {
        appWindow.consentPage = null
        navigation.restart()
    }

    function openShare(items) {
        if (!items || items.length === 0) {
            return
        }
        appWindow.pendingShare = items
        appWindow.activate()
        appWindow.navigate()
    }

    Timer {
        id: navigation
        interval: 150
        onTriggered: appWindow.navigate()
    }

    // An offer while the app is in the background: say so, without any
    // peer-supplied words -- the notification shows on the lock screen,
    // and lipstick's rendering of it is not ours to make plain (S2).
    Notification {
        id: offerNote
        objectName: "offerNote"
        appName: "Sukkula"
        appIcon: "harbour-sukkula"
        //: Notification: an offer waits in the app; no names, it may show on the lock screen.
        summary: qsTr("Someone nearby wants to send you files")
        //: Notification body for a waiting offer.
        body: qsTr("Open Sukkula to accept or decline.")
        previewSummary: summary
        previewBody: body
    }

    // A finished incoming transfer: how many files, never their names.
    Notification {
        id: doneNote
        objectName: "doneNote"
        appName: "Sukkula"
        appIcon: "harbour-sukkula"
    }

    function transferEnded(direction, state, transferId) {
        if (direction !== "incoming" || appWindow.applicationActive) {
            return
        }
        var t = sukkula.transfer(transferId)
        if (state === "done") {
            doneNote.summary = t && t.savedCount > 0
                //: Notification: files arrived.
                ? qsTr("%n file(s) received", "", t.savedCount)
                //: Notification: a text arrived.
                : qsTr("Text received")
            doneNote.body = t && t.savedCount > 0
                //: Notification body: where received files are.
                ? qsTr("Saved in Downloads/Sukkula")
                //: Notification body: where a received text is.
                : qsTr("Open Sukkula to read it.")
        } else if (state === "failed") {
            //: Notification: an incoming transfer failed.
            doneNote.summary = qsTr("Receiving failed")
            doneNote.body = sukkula.errorText({ code: t ? t.error : "" })
        } else {
            return
        }
        doneNote.previewSummary = doneNote.summary
        doneNote.previewBody = doneNote.body
        doneNote.publish()
    }

    Connections {
        target: sukkula.offers
        // Qt 5.6 handler syntax.
        onCountChanged: {
            if (sukkula.offers.count > 0 && !appWindow.applicationActive) {
                offerNote.publish()
            } else if (sukkula.offers.count === 0) {
                offerNote.close()
            }
        }
    }

    Loader {
        id: shareTarget
        objectName: "shareTarget"
        source: Qt.resolvedUrl("share/ShareTarget.qml")
        onLoaded: shareTarget.item.shared.connect(appWindow.openShare)
    }
}
