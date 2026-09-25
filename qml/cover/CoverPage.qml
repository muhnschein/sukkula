// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The cover (spec §2): whether Sukkula is receiving, offers waiting for an
 * answer, and how far the running transfers have got. No peer names: the
 * cover is on the home screen for anyone to read.
 */
CoverBackground {
    id: cover

    property QtObject engine

    Column {
        anchors {
            left: parent.left
            right: parent.right
            verticalCenter: parent.verticalCenter
            margins: Theme.paddingLarge
        }
        spacing: Theme.paddingMedium

        Label {
            width: parent.width
            text: "Sukkula"
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            color: Theme.secondaryColor
        }

        Label {
            objectName: "coverState"
            width: parent.width
            text: !cover.engine.running ? "–"
                  //: Cover: receiving is on.
                  : cover.engine.receiving ? qsTr("Receiving")
                  //: Cover: receiving is off.
                  : qsTr("Not receiving")
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeLarge
            color: cover.engine.receiving ? Theme.highlightColor : Theme.primaryColor
        }

        Label {
            objectName: "coverOffers"
            width: parent.width
            visible: cover.engine.offers.count > 0
            //: Cover: offers waiting for Accept or Decline.
            text: qsTr("%n offer(s) waiting", "", cover.engine.offers.count)
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.highlightColor
        }

        Label {
            objectName: "coverTransfers"
            width: parent.width
            visible: cover.engine.activeTransfers > 0
            //: Cover: transfers running; %1 is the percentage done.
            text: qsTr("%n transfer(s), %1%", "", cover.engine.activeTransfers)
                  .arg(cover.engine.activeTotal > 0
                       ? Math.floor(100 * cover.engine.activeBytes / cover.engine.activeTotal) : 0)
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.primaryColor
        }

        ProgressLine {
            width: parent.width
            visible: cover.engine.activeTransfers > 0
            value: cover.engine.activeTotal > 0 ? cover.engine.activeBytes / cover.engine.activeTotal : 0
        }
    }

    CoverActionList {
        enabled: cover.engine.running
        CoverAction {
            iconSource: cover.engine.receiving ? "image://theme/icon-cover-pause"
                                               : "image://theme/icon-cover-play"
            onTriggered: cover.engine.setReceiving(!cover.engine.receiving)
        }
    }
}
