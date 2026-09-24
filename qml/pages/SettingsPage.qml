// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * Settings, saved when the page is left: the device name (F-C7), each
 * protocol on or off (F-C1), the LocalSend PIN (F-LS4), Quick Share
 * visibility and the BLE nudge (F-QS4, F-QS2), the wormhole servers
 * (F-MW4), and debug logging, off by default (S9).
 *
 * The fields are checked here the way sukkula-core checks them
 * (config.rs), so a bad value is caught while it can still be fixed; the
 * engine checks again and has the last word. There is no auto-accept
 * setting, and never will be (F-C2).
 */
Page {
    id: page
    objectName: "settingsPage"

    property QtObject engine
    property bool loaded: false

    readonly property bool pinValid: /^[A-Za-z0-9]{0,16}$/.test(pinField.text)
    readonly property bool mailboxValid: page.urlValid(mailboxField.text, ["ws://", "wss://"])
    readonly property bool relayValid: page.urlValid(relayField.text, ["tcp://"])
    readonly property bool valid: page.pinValid && page.mailboxValid && page.relayValid

    // A bad value keeps the page open, highlighted, rather than being
    // thrown away on the way out.
    backNavigation: page.valid

    /// Empty, or one of `schemes` followed by something, at most 256
    /// printable ASCII characters with no spaces (config.rs, check_url).
    function urlValid(text, schemes) {
        var url = text.replace(/^\s+|\s+$/g, "")
        if (url.length === 0) {
            return true
        }
        if (url.length > 256 || !/^[\x21-\x7e]+$/.test(url)) {
            return false
        }
        var lower = url.toLowerCase()
        for (var i = 0; i < schemes.length; i++) {
            if (lower.indexOf(schemes[i]) === 0 && url.length > schemes[i].length) {
                return true
            }
        }
        return false
    }

    function trimmed(text) {
        return text.replace(/^\s+|\s+$/g, "")
    }

    function load() {
        var s = page.engine.settings
        var ls = s.localsend || {}
        var qs = s.quickshare || {}
        var mw = s.wormhole || {}
        var bt = s.bluetooth || {}
        nameField.text = typeof s.device_name === "string" ? s.device_name : ""
        localSendSwitch.checked = ls.enabled !== false
        pinField.text = typeof ls.pin === "string" ? ls.pin : ""
        quickShareSwitch.checked = qs.enabled !== false
        visibilityBox.currentIndex = qs.visibility === "hidden" ? 1 : 0
        nudgeSwitch.checked = qs.ble_nudge !== false
        mailboxField.text = typeof mw.mailbox_url === "string" ? mw.mailbox_url : ""
        relayField.text = typeof mw.relay_url === "string" ? mw.relay_url : ""
        bluetoothSwitch.checked = bt.enabled !== false
        loggingSwitch.checked = s.logging === true
        page.loaded = true
    }

    /// The engine's settings with this page's fields written over them.
    /// Starting from the engine's copy keeps any field this page does not
    /// know about, which the engine would otherwise reset.
    function collect() {
        var s = JSON.parse(JSON.stringify(page.engine.settings))
        if (!s.localsend) { s.localsend = {} }
        if (!s.quickshare) { s.quickshare = {} }
        if (!s.wormhole) { s.wormhole = {} }
        if (!s.bluetooth) { s.bluetooth = {} }
        s.device_name = page.trimmed(nameField.text)
        s.localsend.enabled = localSendSwitch.checked
        s.localsend.pin = pinField.text.length > 0 ? pinField.text : null
        s.quickshare.enabled = quickShareSwitch.checked
        s.quickshare.visibility = visibilityBox.currentIndex === 1 ? "hidden" : "everyone"
        s.quickshare.ble_nudge = nudgeSwitch.checked
        var mailbox = page.trimmed(mailboxField.text)
        var relay = page.trimmed(relayField.text)
        s.wormhole.mailbox_url = mailbox.length > 0 ? mailbox : null
        s.wormhole.relay_url = relay.length > 0 ? relay : null
        s.bluetooth.enabled = bluetoothSwitch.checked
        s.logging = loggingSwitch.checked
        return s
    }

    function save() {
        if (!page.loaded || !page.valid || !page.engine.settingsKnown) {
            return
        }
        var next = page.collect()
        if (JSON.stringify(next) === JSON.stringify(page.engine.settings)) {
            return
        }
        // No callback: a refusal reaches the main page's banner through
        // Engine.failed, since this page is on its way out.
        page.engine.setSettings(next)
    }

    Component.onCompleted: page.load()

    onStatusChanged: {
        if (page.status === PageStatus.Deactivating) {
            page.save()
        }
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        PullDownMenu {
            MenuItem {
                //: Pulley menu: the page with the version and licence.
                text: qsTr("About Sukkula")
                onClicked: pageStack.push(Qt.resolvedUrl("AboutPage.qml"), { engine: page.engine })
            }
        }

        Column {
            id: column
            width: parent.width

            PageHeader {
                //: Page title.
                title: qsTr("Settings")
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: !page.valid
                //: Settings page: a field is highlighted as wrong.
                text: qsTr("Correct the highlighted field to save.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.errorColor
            }

            // What failed to start, with the engine's detail line.
            Repeater {
                model: page.engine.protocolStatuses
                delegate: Column {
                    width: column.width
                    visible: modelData.state === "failed"

                    Label {
                        x: Theme.horizontalPageMargin
                        width: parent.width - 2 * Theme.horizontalPageMargin
                        //: A protocol could not start; %1 is its name, %2 why.
                        text: qsTr("%1 could not start: %2").arg(page.engine.protocolName(modelData.protocol))
                                                          .arg(page.engine.errorText({ code: modelData.errorCode }))
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.errorColor
                    }
                    Label {
                        x: Theme.horizontalPageMargin
                        width: parent.width - 2 * Theme.horizontalPageMargin
                        visible: text.length > 0
                        text: modelData.errorDetail
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.secondaryColor
                    }
                }
            }

            TextField {
                id: nameField
                objectName: "deviceNameField"
                width: parent.width
                //: Settings: the name other devices see (F-C7).
                label: qsTr("Device name")
                // The name used when this is empty: the phone's model.
                placeholderText: page.engine.effectiveDeviceName
                maximumLength: 64
            }

            SectionHeader {
                text: "LocalSend"
                visible: page.engine.hasProtocol("local_send")
            }
            TextSwitch {
                id: localSendSwitch
                visible: page.engine.hasProtocol("local_send")
                //: Settings: switch a protocol on or off.
                text: qsTr("Use LocalSend")
                //: Settings: what the LocalSend switch covers.
                description: qsTr("Send to and receive from LocalSend apps on the same Wi-Fi.")
            }
            TextField {
                id: pinField
                objectName: "pinField"
                width: parent.width
                visible: page.engine.hasProtocol("local_send")
                //: Settings: the PIN LocalSend senders must type (F-LS4).
                label: page.pinValid
                       ? qsTr("Receive PIN (optional)")
                       //: Settings: the PIN field holds something else than 1 to 16 letters and digits.
                       : qsTr("Up to 16 letters and digits")
                //: Settings: the PIN field when no PIN is set.
                placeholderText: qsTr("No PIN")
                inputMethodHints: Qt.ImhNoPredictiveText | Qt.ImhNoAutoUppercase
                maximumLength: 16
                errorHighlight: !page.pinValid
            }

            SectionHeader {
                text: "Quick Share"
                visible: page.engine.hasProtocol("quick_share")
            }
            TextSwitch {
                id: quickShareSwitch
                visible: page.engine.hasProtocol("quick_share")
                //: Settings: switch a protocol on or off.
                text: qsTr("Use Quick Share")
                //: Settings: what the Quick Share switch covers.
                description: qsTr("Send to and receive from Android phones on the same Wi-Fi.")
            }
            ComboBox {
                id: visibilityBox
                objectName: "visibilityBox"
                width: parent.width
                visible: page.engine.hasProtocol("quick_share")
                //: Settings: who can see this phone over Quick Share (F-QS4).
                label: qsTr("Visible to")
                //: Settings: Quick Share visibility; contacts-only is impossible without a Google account.
                description: qsTr("Contacts only needs a Google account, so it is not offered.")
                menu: ContextMenu {
                    MenuItem {
                        //: Quick Share visibility: anyone nearby while receiving is on.
                        text: qsTr("Everyone")
                    }
                    MenuItem {
                        //: Quick Share visibility: nobody can find this phone.
                        text: qsTr("Hidden")
                    }
                }
            }
            TextSwitch {
                id: nudgeSwitch
                visible: page.engine.hasProtocol("quick_share")
                //: Settings: advertise over Bluetooth LE so Android phones look for this one (F-QS2).
                text: qsTr("Bluetooth nudge")
                //: Settings: what the Bluetooth nudge does.
                description: qsTr("Announce over Bluetooth that this phone is receiving, so Android phones look for it.")
            }

            SectionHeader {
                text: "Magic Wormhole"
                visible: page.engine.hasProtocol("wormhole")
            }
            TextField {
                id: mailboxField
                objectName: "mailboxField"
                width: parent.width
                visible: page.engine.hasProtocol("wormhole")
                //: Settings: the Magic Wormhole mailbox server (F-MW4).
                label: page.mailboxValid ? qsTr("Mailbox server")
                                         //: Settings: the mailbox URL is not usable.
                                         : qsTr("Must start with ws:// or wss://")
                //: Settings: an empty server field means the built-in default.
                placeholderText: qsTr("Default server")
                inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoPredictiveText | Qt.ImhNoAutoUppercase
                maximumLength: 256
                errorHighlight: !page.mailboxValid
            }
            TextField {
                id: relayField
                objectName: "relayField"
                width: parent.width
                visible: page.engine.hasProtocol("wormhole")
                //: Settings: the Magic Wormhole transit relay (F-MW4).
                label: page.relayValid ? qsTr("Transit relay")
                                       //: Settings: the relay URL is not usable.
                                       : qsTr("Must look like tcp://host:port")
                //: Settings: an empty server field means the built-in default.
                placeholderText: qsTr("Default server")
                inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoPredictiveText | Qt.ImhNoAutoUppercase
                maximumLength: 256
                errorHighlight: !page.relayValid
            }

            SectionHeader {
                text: "Bluetooth"
                visible: page.engine.hasProtocol("bluetooth")
            }
            TextSwitch {
                id: bluetoothSwitch
                visible: page.engine.hasProtocol("bluetooth")
                //: Settings: switch a protocol on or off.
                text: qsTr("Send over Bluetooth")
                //: Settings: why Bluetooth only sends (F-BT2).
                description: qsTr("To paired devices. Receiving over Bluetooth is up to the phone's own Bluetooth settings.")
            }

            SectionHeader {
                //: Settings section: troubleshooting.
                text: qsTr("Troubleshooting")
            }
            TextSwitch {
                id: loggingSwitch
                objectName: "loggingSwitch"
                //: Settings: debug logging (S9).
                text: qsTr("Debug logging")
                //: Settings: what debug logging does.
                description: qsTr("Only for finding faults. Leave it off otherwise.")
            }
        }

        VerticalScrollDecorator {}
    }
}
