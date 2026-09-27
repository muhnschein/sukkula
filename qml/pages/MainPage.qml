// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The one screen: Send and Receive as two tabs, tapped at the top or
 * swiped between (F-C1).
 *
 * The tab is the engine's mode. Receive switches every enabled receiver
 * on and shows the receive radar (ReceiveView); Send switches them off
 * and shows the send radar (SendView). Discovery runs in Send mode while
 * the app is in front, whether or not anything is chosen yet, so the
 * peers are there as soon as the files are. Both radars are drawn for
 * portrait, and the page stays in it.
 *
 * What was sent and received is on the History page; the texts there are
 * shown, never opened (S2, S8).
 */
Page {
    id: page
    objectName: "mainPage"

    property QtObject engine
    property bool switching: false
    /// The mode a switch on its way asked for.
    property bool switchingTo: false
    property bool alive: true
    /// Discovery is held by this page (Engine.qml counts who asked).
    property bool discovering: false
    /// The paired Bluetooth devices were asked for, this time round.
    property bool devicesListed: false
    /// The app is in front: the window's to say.
    property bool foreground: true
    /// How long the app may be in the background, in ms, before discovery
    /// is paused: a glance at the Events view or the top menu keeps the
    /// peers on the radar.
    property int backgroundGrace: 5000
    /// In front, or not for long.
    property bool awake: true
    /// What to send: the send radar's centre.
    property alias payload: payloadStore
    property Item sendView: null
    property Item receiveView: null

    readonly property bool receiveMode: page.engine.receiving
    readonly property bool fatal: page.engine.fatalCode !== ""
    readonly property bool wantDiscovery: page.alive && page.engine.running && !page.engine.receiving
                                          && page.awake
    readonly property bool bluetoothOn: page.engine.protocolEnabled("bluetooth")
    /// How far the tab on screen is pulled down (negative) or scrolled.
    readonly property real yOffset: pager.currentItem && pager.currentItem.yOffset !== undefined
                                    ? pager.currentItem.yOffset : 0
    /// The band the tabs take at the top.
    readonly property real stripBand: tabs.height

    allowedOrientations: Orientation.Portrait

    Component.onDestruction: {
        page.alive = false
        if (page.discovering && page.engine) {
            page.discovering = false
            page.engine.stopDiscovery()
        }
    }

    Component.onCompleted: {
        if (!page.foreground) {
            sleep.restart()
        }
        page.showTab(page.engine.receiving ? 1 : 0)
        page.syncDiscovery()
    }
    onWantDiscoveryChanged: page.syncDiscovery()

    onForegroundChanged: {
        if (page.foreground) {
            sleep.stop()
            page.awake = true
        } else {
            sleep.restart()
        }
    }

    // In the background for a while: discovery pauses (and the peers are
    // forgotten), and comes back when the app does.
    Timer {
        id: sleep
        interval: page.backgroundGrace
        onTriggered: page.awake = false
    }

    onBluetoothOnChanged: page.listDevices()

    // The engine changed mode on its own (the cover's action): the tab
    // follows.
    onReceiveModeChanged: {
        if (!page.switching) {
            page.showTab(page.receiveMode ? 1 : 0)
        }
    }

    /// Discovery runs in Send mode only, and the paired Bluetooth devices
    /// are listed as it starts (F-LS1, F-QS1, F-BT1).
    function syncDiscovery() {
        if (page.wantDiscovery && !page.discovering) {
            page.discovering = true
            page.engine.startDiscovery()
            page.listDevices()
        } else if (!page.wantDiscovery && page.discovering) {
            page.discovering = false
            page.devicesListed = false
            page.engine.stopDiscovery()
        }
    }

    /// Once per turn in Send mode, and again when Bluetooth is switched
    /// back on. No banner for a Bluetooth that is off: the radar just has
    /// no Bluetooth devices on it.
    function listDevices() {
        if (!page.bluetoothOn) {
            page.devicesListed = false
            return
        }
        if (!page.discovering || page.devicesListed) {
            return
        }
        page.devicesListed = true
        page.engine.listBluetoothDevices(function () {})
    }

    function showTab(index) {
        if (pager.currentIndex !== index) {
            pager.moveTo(index)
        }
    }

    /// Send or Receive: one set_receiving. A tab swiped to while a switch
    /// is on its way is switched to once it has landed -- from the mode
    /// that switch set, `from`, whether or not the engine's event saying
    /// so has come yet. A refused switch takes the tab back.
    function setMode(receive, from) {
        var now = from !== undefined ? from : page.engine.receiving
        if (page.switching || receive === now || !page.engine.running) {
            return
        }
        page.switching = true
        page.switchingTo = receive
        var self = page
        page.engine.setReceiving(receive, function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.switching = false
            if (!ok) {
                pageBanner.show(self.engine.errorText(error))
                self.showTab(self.engine.receiving ? 1 : 0)
                return
            }
            var wanted = pager.currentIndex === 1
            if (wanted !== receive) {
                self.setMode(wanted, receive)
            }
        })
    }

    /// What the Share menu handed over: to the send radar's centre, in
    /// Send mode. Nothing is sent until a peer is tapped.
    function share(items) {
        payloadStore.load(items)
        if (page.sendView) {
            page.sendView.dismiss()
        }
        page.showTab(0)
        page.setMode(false)
    }

    function openHistory() {
        pageStack.push(Qt.resolvedUrl("HistoryPage.qml"), { engine: page.engine })
    }
    function openSettings() {
        pageStack.push(Qt.resolvedUrl("SettingsPage.qml"), { engine: page.engine })
    }

    Payload {
        id: payloadStore
    }

    Connections {
        target: page.engine
        // Qt 5.6 handler syntax.
        onFailed: pageBanner.show(message)
    }

    PagedView {
        id: pager
        objectName: "modePager"
        anchors.fill: parent
        visible: !page.fatal
        model: 2
        cacheSize: 2
        interactive: page.engine.running
        onCurrentIndexChanged: page.setMode(pager.currentIndex === 1)

        delegate: Loader {
            readonly property real yOffset: item ? item.contentY - item.originY : 0
            width: pager.width
            height: pager.height
            sourceComponent: index === 0 ? sendTab : receiveTab
        }
    }

    Component {
        id: sendTab

        SilicaFlickable {
            contentHeight: height

            PullDownMenu {
                MenuItem {
                    //: Pulley menu.
                    text: qsTr("Settings")
                    onClicked: page.openSettings()
                }
                MenuItem {
                    objectName: "openHistory"
                    //: Pulley menu: the page with what was sent and received.
                    text: qsTr("History")
                    onClicked: page.openHistory()
                }
                MenuItem {
                    objectName: "cancelSending"
                    //: Pulley menu in Send mode: stop the send that is running.
                    text: qsTr("Cancel sending")
                    visible: send.outgoingState === "active"
                    onClicked: send.cancelSend()
                }
            }

            SendView {
                id: send
                objectName: "sendView"
                width: parent.width
                height: parent.height
                topInset: page.stripBand
                engine: page.engine
                payload: page.payload
                banner: pageBanner
                discovering: page.discovering
                current: pager.currentIndex === 0
                Component.onCompleted: page.sendView = send
            }
        }
    }

    Component {
        id: receiveTab

        SilicaFlickable {
            contentHeight: height

            PullDownMenu {
                MenuItem {
                    //: Pulley menu.
                    text: qsTr("Settings")
                    onClicked: page.openSettings()
                }
                MenuItem {
                    objectName: "openHistory"
                    //: Pulley menu: the page with what was sent and received.
                    text: qsTr("History")
                    onClicked: page.openHistory()
                }
            }

            ReceiveView {
                id: receive
                objectName: "receiveView"
                width: parent.width
                height: parent.height
                topInset: page.stripBand
                engine: page.engine
                banner: pageBanner
                current: pager.currentIndex === 1
                Component.onCompleted: page.receiveView = receive
            }
        }
    }

    // The tabs, over the radars. They ride down with a pulley being
    // pulled and drop behind the pager meanwhile, so the opened menu is
    // drawn over them (Silica's TabView does the same).
    ModeTabs {
        id: tabs
        objectName: "modeTabs"
        anchors {
            left: parent.left
            right: parent.right
        }
        y: Math.max(0, -page.yOffset)
        z: page.yOffset < 0 ? -1 : 1
        height: implicitHeight
        visible: !page.fatal
        hostPage: page
        //: The main page's tabs.
        titles: [qsTr("Send"), qsTr("Receive")]
        currentIndex: pager.currentIndex
        busyIndex: page.switching ? (page.switchingTo ? 1 : 0) : -1
        onTabClicked: page.showTab(index)
    }

    // The engine could not start: nothing else would work.
    SilicaFlickable {
        anchors.fill: parent
        visible: page.fatal
        contentHeight: fatalColumn.height

        PullDownMenu {
            MenuItem {
                //: Pulley menu.
                text: qsTr("Settings")
                onClicked: page.openSettings()
            }
        }

        Column {
            id: fatalColumn
            width: parent.width
            spacing: Theme.paddingSmall

            PageHeader {
                title: "Sukkula"
            }
            Label {
                objectName: "fatalLabel"
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                //: The engine failed to start; %1 says why.
                text: qsTr("Sukkula could not start: %1").arg(page.engine.errorText({ code: page.engine.fatalCode }))
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.errorColor
            }
            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                text: page.engine.fatalDetail
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                visible: text.length > 0
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
            }
        }
    }

    // Over either tab: what just went wrong, or right.
    Banner {
        id: pageBanner
        y: page.fatal ? 0 : page.stripBand
        z: 2
    }
}
