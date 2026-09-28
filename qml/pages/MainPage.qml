// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The one screen: Send and Receive as two tabs, tapped at the top or
 * swiped between (F-C1).
 *
 * The tab is the engine's mode while the app is in front. Receive
 * switches every enabled receiver on (ReceiveView); Send switches them
 * off and runs discovery instead (SendView), whether or not anything is
 * chosen yet, so the devices are there as soon as the files are. After 5 s
 * in the background both stop (spec v0.7): nothing announces this phone
 * or listens for it while the app is not in use, save while an offer
 * waits for an answer or a transfer comes in; back in front, the tab on
 * screen starts its half again. The page stays in portrait.
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
    /// and receiving stop: a glance at the Events view or the top menu
    /// keeps both going.
    property int backgroundGrace: 5000
    /// In front, or not for long.
    property bool awake: true
    /// What to send.
    property alias payload: payloadStore
    property Item sendView: null
    property Item receiveView: null

    readonly property bool fatal: page.engine.fatalCode !== ""
    /// The Receive tab is the one on screen.
    readonly property bool receiveTab: pager.currentIndex === 1
    /// Something incoming is running.
    readonly property bool incoming: page.engine.activeIncoming > 0
    /// Receiving is wanted: the Receive tab, in front or not for long, or
    /// kept on for an offer waiting or a transfer coming.
    readonly property bool wantReceiving: page.alive && page.engine.running && page.receiveTab
                                          && (page.awake || page.engine.offers.count > 0 || page.incoming)
    readonly property bool wantDiscovery: page.alive && page.engine.running && !page.receiveTab
                                          && !page.engine.receiving && page.awake
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
    onWantReceivingChanged: page.setMode(page.wantReceiving)

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

    /// The cover's actions: the app opens on this tab.
    function openTab(index) {
        page.showTab(index)
    }

    function showTab(index) {
        if (pager.currentIndex !== index) {
            pager.moveTo(index)
        }
    }

    /// A tab was tapped: the tab on screen goes back to its top, as in
    /// Silica's TabView; the other one comes.
    function tapTab(index) {
        if (pager.currentIndex === index) {
            if (pager.currentItem && pager.currentItem.item) {
                pager.currentItem.item.scrollToTop()
            }
            return
        }
        page.showTab(index)
    }

    /// Receiving on or off: one set_receiving. What is wanted by the time
    /// a switch has landed is switched to next -- from the mode that
    /// switch set, `from`, whether or not the engine's event saying so has
    /// come yet. A refused switch takes the tab back to the engine's mode.
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
            if (self.wantReceiving !== receive) {
                self.setMode(self.wantReceiving, receive)
            }
        })
    }

    /// What the Share menu handed over: chosen on the Send tab. Nothing
    /// is sent until a device is tapped.
    function share(items) {
        payloadStore.load(items)
        if (page.sendView) {
            page.sendView.dismiss()
        }
        page.showTab(0)
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

    // What the tabs are seen through: the page less the tabs' band, clipped
    // while a list is scrolled, so a row scrolled up is cut at the tabs'
    // lower edge instead of passing behind their words. The pager is moved
    // back up by the band, so a list still starts at the top of the screen,
    // where its pulley menu comes down from; and nothing is clipped while
    // at rest or pulled, when the band holds only the pulley's indicator
    // or the menu (Vuo's EntryListPage, after Silica's TabItem).
    Item {
        id: viewport
        objectName: "modeViewport"
        y: page.fatal ? 0 : page.stripBand
        width: page.width
        height: page.height - viewport.y
        clip: page.yOffset > 0
        visible: !page.fatal

        PagedView {
            id: pager
            objectName: "modePager"
            y: -viewport.y
            width: viewport.width
            height: page.height
            model: 2
            cacheSize: 2
            interactive: page.engine.running

            delegate: Loader {
                readonly property real yOffset: item ? item.contentY - item.originY : 0
                width: pager.width
                height: pager.height
                sourceComponent: index === 0 ? sendTab : receiveTab
            }
        }
    }

    Component {
        id: sendTab

        SilicaFlickable {
            contentHeight: Math.max(height, send.implicitHeight)

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
                    objectName: "addMore"
                    //: Pulley menu on the Send tab: choose more files.
                    text: qsTr("Add more")
                    visible: send.hasPayload
                    onClicked: send.pick(send.payloadKind)
                }
                MenuItem {
                    objectName: "startOver"
                    //: Pulley menu on the Send tab: clear what is chosen.
                    text: qsTr("Start over")
                    visible: send.hasPayload && !(send.hasOutgoing && !send.outgoingEnded)
                    onClicked: send.clearPayload()
                }
            }

            SendView {
                id: send
                objectName: "sendView"
                width: parent.width
                topInset: page.stripBand
                engine: page.engine
                payload: page.payload
                banner: pageBanner
                remorse: pageRemorse
                discovering: page.discovering
                current: pager.currentIndex === 0
                Component.onCompleted: page.sendView = send
            }

            VerticalScrollDecorator {}
        }
    }

    Component {
        id: receiveTab

        SilicaFlickable {
            id: receiveList
            contentHeight: Math.max(height, receive.implicitHeight)

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
                topInset: page.stripBand
                viewHeight: receiveList.height
                engine: page.engine
                banner: pageBanner
                current: pager.currentIndex === 1
                foreground: page.foreground
                Component.onCompleted: page.receiveView = receive
            }

            VerticalScrollDecorator {}
        }
    }

    // The tabs, over the lists. They ride down with a pulley being
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
        busyIndex: page.switching && page.awake ? (page.switchingTo ? 1 : 0) : -1
        onTabClicked: page.tapTab(index)
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

    // Over the tabs: the chosen files about to be cleared.
    RemorsePopup {
        id: pageRemorse
        objectName: "clearRemorse"
        z: 3
    }

    // Over either tab: what just went wrong, or right.
    Banner {
        id: pageBanner
        y: page.fatal ? 0 : page.stripBand
        z: 2
    }
}
