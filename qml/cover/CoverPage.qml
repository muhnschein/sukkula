// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The cover (spec §2, v0.7). No peer's words: the cover is on the home
 * screen for anyone to read.
 *
 * Nothing going on: Send and Receive, each over its own cover action,
 * which opens the app on that tab. While this shows, Sukkula announces
 * nothing and listens for nothing (MainPage.qml stops both after a short
 * while in the background).
 *
 * An offer waiting: that it waits, and for how long more. No action:
 * accepting always goes through the consent dialog with the file list
 * (F-C2), so tapping the cover opens the app on it.
 *
 * A transfer running: how far, as one ring with the percentage in it,
 * which way it goes, and how many files. It runs on in the background.
 */
CoverBackground {
    id: cover

    property QtObject engine

    /// A cover action was triggered: open the app on the Send tab (0) or
    /// the Receive tab (1).
    signal openTab(int index)

    readonly property bool offerWaiting: cover.engine.offers.count > 0
    /// The newest transfer running, or null.
    readonly property var running: (cover.engine.activeTransfers, cover.engine.activeBytes, cover.newestActive())
    readonly property bool idle: !cover.offerWaiting && cover.running === null
    /// Seconds before the first offer is declined on its own.
    property int remaining: 0

    function newestActive() {
        for (var i = 0; i < cover.engine.transfers.count; i++) {
            var t = cover.engine.transfers.get(i)
            if (t.state === "active") {
                return cover.engine.transfer(t.transferId)
            }
        }
        return null
    }

    function tickOffer() {
        var first = cover.engine.offers.count > 0 ? cover.engine.offers.get(0) : null
        cover.remaining = first ? Math.max(0, Math.ceil((first.expiresAt - Date.now()) / 1000)) : 0
    }

    Timer {
        interval: 1000
        repeat: true
        running: cover.offerWaiting
        triggeredOnStart: true
        onTriggered: cover.tickOffer()
    }

    // ---- Nothing going on ---------------------------------------------------

    Item {
        objectName: "coverIdle"
        anchors {
            fill: parent
            bottomMargin: Theme.itemSizeSmall
        }
        visible: cover.idle

        Rectangle {
            anchors {
                horizontalCenter: parent.horizontalCenter
                top: parent.top
                topMargin: Theme.paddingLarge
                bottom: parent.bottom
            }
            width: 1
            color: Theme.rgba(Theme.primaryColor, 0.15)
        }
        Label {
            objectName: "coverSend"
            anchors.centerIn: parent
            anchors.horizontalCenterOffset: -parent.width / 4
            //: Cover, with nothing going on: over the action that opens the Send tab.
            text: qsTr("Send")
            textFormat: Text.PlainText
            font.pixelSize: Theme.fontSizeLarge
        }
        Label {
            objectName: "coverReceive"
            anchors.centerIn: parent
            anchors.horizontalCenterOffset: parent.width / 4
            //: Cover, with nothing going on: over the action that opens the Receive tab.
            text: qsTr("Receive")
            textFormat: Text.PlainText
            font.pixelSize: Theme.fontSizeLarge
        }
    }

    // ---- An offer waiting -----------------------------------------------------

    Column {
        objectName: "coverOffer"
        anchors {
            left: parent.left
            right: parent.right
            verticalCenter: parent.verticalCenter
            margins: Theme.paddingLarge
        }
        visible: cover.offerWaiting
        spacing: Theme.paddingSmall

        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            //: Cover: someone offers files and waits for Accept or Decline.
            text: qsTr("Offer waiting")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeLarge
            color: Theme.highlightColor
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            //: Cover, under "Offer waiting": tapping opens the offer to accept or decline.
            text: qsTr("Tap to answer")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }
        Label {
            objectName: "coverCountdown"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            //: Cover: time left before a waiting offer is declined on its own.
            text: qsTr("Declined in %n s", "", cover.remaining)
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }
    }

    // ---- A transfer running ---------------------------------------------------

    Column {
        objectName: "coverTransfer"
        anchors {
            left: parent.left
            right: parent.right
            verticalCenter: parent.verticalCenter
            margins: Theme.paddingLarge
        }
        visible: !cover.offerWaiting && cover.running !== null
        spacing: Theme.paddingMedium

        Item {
            anchors.horizontalCenter: parent.horizontalCenter
            width: parent.width * 0.7
            height: width

            ProgressCircle {
                objectName: "coverRing"
                anchors.fill: parent
                progressColor: Theme.highlightColor
                backgroundColor: Theme.rgba(Theme.highlightColor, 0.2)
                value: cover.running && cover.running.total > 0
                       ? Math.min(1, cover.running.bytes / cover.running.total) : 0
            }
            Label {
                objectName: "coverPercent"
                anchors.centerIn: parent
                text: Math.floor(100 * (cover.running && cover.running.total > 0
                                        ? Math.min(1, cover.running.bytes / cover.running.total) : 0)) + "%"
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeLarge
                color: Theme.highlightColor
            }
        }
        Label {
            objectName: "coverDirection"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: cover.running && cover.running.direction === "incoming"
                  //: Cover: files are coming in.
                  ? qsTr("Receiving")
                  //: Cover: files are going out.
                  : qsTr("Sending")
            textFormat: Text.PlainText
            font.pixelSize: Theme.fontSizeSmall
        }
        // How many, in Sukkula's words: never a file's name.
        Label {
            objectName: "coverWhat"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: {
                var t = cover.running
                if (!t) {
                    return ""
                }
                var names = t.files.length > 0 ? t.files.split("\n") : []
                return cover.engine.countWords(cover.engine.commonKind(names, t.fileCount), t.fileCount)
            }
            textFormat: Text.PlainText
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }
    }

    CoverActionList {
        enabled: cover.idle && cover.engine.running
        CoverAction {
            objectName: "coverActionSend"
            iconSource: "image://theme/icon-m-up"
            onTriggered: cover.openTab(0)
        }
        CoverAction {
            objectName: "coverActionReceive"
            iconSource: "image://theme/icon-m-down"
            onTriggered: cover.openTab(1)
        }
    }
}
