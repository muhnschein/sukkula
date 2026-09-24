// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * "Send via…": what to send, and how (F-C6).
 *
 * Files come from the Share menu or the file picker, text from the Share
 * menu or typed here. Then a protocol: LocalSend and Quick Share list the
 * peers discovery finds while this page is open (F-LS1, F-QS1); Magic
 * Wormhole makes a code for one file or one text (F-MW1); Bluetooth lists
 * the paired devices that take Object Push (F-BT1).
 *
 * Peer names and models are the peers' own (after S2) and are shown only
 * in this page's plain-text labels.
 */
Page {
    id: page
    objectName: "sendPage"

    property QtObject engine
    /// What to start with: [{kind: "file", path} | {kind: "text", text}].
    property var items: []

    /// The files chosen so far: [{path, name}].
    property var files: []
    /// Texts from the Share menu. Shown in this page's own plain-text
    /// labels rather than put into the text box, whose rendering is
    /// Silica's: a shared text can be anything, markup included (S2).
    property var texts: []
    property string protocol: ""
    property bool sending: false
    property bool discovering: false
    property bool alive: true
    property bool listingDevices: false

    /// The protocols offered: in this build and switched on in Settings.
    readonly property var available: page.availableProtocols(page.engine.protocols, page.engine.settings)
    readonly property bool lan: page.protocol === "local_send" || page.protocol === "quick_share"
    readonly property int itemCount: page.files.length + page.texts.length + (textArea.text.length > 0 ? 1 : 0)
    readonly property bool hasText: page.texts.length > 0 || textArea.text.length > 0
    // Most files one offer may carry (S6), which the engine checks again.
    readonly property int maxFiles: 500
    // ~/Downloads, the one folder Sailjail lets Sukkula read (spec §2).
    readonly property string downloads: StandardPaths.download

    function availableProtocols() {
        var order = ["local_send", "quick_share", "wormhole", "bluetooth"]
        var out = []
        for (var i = 0; i < order.length; i++) {
            if (page.engine.protocolEnabled(order[i])) {
                out.push(order[i])
            }
        }
        return out
    }

    function basename(path) {
        var parts = String(path).split("/")
        return parts[parts.length - 1]
    }

    function addFile(path) {
        path = String(path)
        if (path.length === 0 || path.charAt(0) !== "/" || page.files.length >= page.maxFiles) {
            return
        }
        for (var i = 0; i < page.files.length; i++) {
            if (page.files[i].path === path) {
                return
            }
        }
        var next = page.files.slice(0)
        next.push({ path: path, name: page.basename(path) })
        page.files = next
    }

    function removeFile(index) {
        var next = page.files.slice(0)
        next.splice(index, 1)
        page.files = next
    }

    function removeText(index) {
        var next = page.texts.slice(0)
        next.splice(index, 1)
        page.texts = next
    }

    function pickFile() {
        var picker = pageStack.push(Qt.resolvedUrl("FilePicker.qml"))
        if (picker) {
            picker.picked.connect(page.addFile)
        }
    }

    function payload() {
        var out = []
        for (var i = 0; i < page.files.length; i++) {
            out.push({ kind: "file", path: page.files[i].path })
        }
        for (var t = 0; t < page.texts.length; t++) {
            out.push({ kind: "text", text: page.texts[t] })
        }
        if (textArea.text.length > 0) {
            out.push({ kind: "text", text: textArea.text })
        }
        return out
    }

    function sendTo(target) {
        if (page.sending || page.itemCount === 0) {
            return
        }
        page.sending = true
        var self = page
        var wormhole = target.protocol === "wormhole"
        page.engine.send(target, page.payload(), function (ok, error, transfer) {
            if (self.alive !== true) {
                return
            }
            self.sending = false
            if (!ok) {
                banner.show(self.engine.errorText(error))
            } else if (wormhole) {
                pageStack.replace(Qt.resolvedUrl("WormholeCodePage.qml"),
                                  { engine: self.engine, transferId: transfer })
            } else {
                // Progress is on the main page, with the other transfers.
                pageStack.pop()
            }
        })
    }

    function listDevices() {
        page.listingDevices = true
        var self = page
        page.engine.listBluetoothDevices(function (ok, error) {
            if (self.alive !== true) {
                return
            }
            self.listingDevices = false
            if (!ok) {
                banner.show(self.engine.errorText(error))
            }
        })
    }

    onProtocolChanged: {
        if (page.protocol === "bluetooth") {
            page.listDevices()
        }
    }

    // Settings can change under the page (the engine reports them late,
    // or another page saved them): never offer a switched-off protocol.
    onAvailableChanged: {
        if (page.available.indexOf(page.protocol) < 0) {
            page.protocol = page.available.length > 0 ? page.available[0] : ""
        }
    }

    onStatusChanged: {
        // Discovery runs while the page exists, not only while it is on
        // top: the file picker comes and goes above it.
        if (page.status === PageStatus.Active && !page.discovering && page.engine.running) {
            page.discovering = true
            page.engine.startDiscovery()
        }
    }

    Component.onCompleted: {
        var list = Array.isArray(page.items) ? page.items : []
        var texts = []
        for (var i = 0; i < list.length; i++) {
            var item = list[i]
            if (!item) {
                continue
            }
            if (item.kind === "file" && typeof item.path === "string") {
                page.addFile(item.path)
            } else if (item.kind === "text" && typeof item.text === "string") {
                texts.push(item.text)
            }
        }
        page.texts = texts
        if (page.available.length > 0) {
            page.protocol = page.available[0]
        }
    }

    Component.onDestruction: {
        page.alive = false
        if (page.discovering && page.engine) {
            page.engine.stopDiscovery()
        }
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height + Theme.paddingLarge

        PullDownMenu {
            MenuItem {
                //: Pulley menu on the send page: choose a file with the file picker.
                text: qsTr("Add file")
                enabled: page.files.length < page.maxFiles
                onClicked: page.pickFile()
            }
        }

        Column {
            id: column
            width: parent.width

            PageHeader {
                //: Page title: choose what to send and how.
                title: qsTr("Send")
            }

            Banner {
                id: banner
            }

            SectionHeader {
                //: Section heading: the files and text to send.
                text: qsTr("What")
            }

            Repeater {
                model: page.files
                delegate: ListItem {
                    id: fileItem
                    width: column.width
                    contentHeight: Math.max(Theme.itemSizeSmall, fileColumn.height + 2 * Theme.paddingSmall)

                    // Sailjail grants Downloads only (spec §2): a file from
                    // anywhere else -- a picture shared from the gallery --
                    // may be out of the app's reach. Said up front; the
                    // engine's bad_file says it again if so.
                    readonly property bool outside: page.downloads.length > 0
                        && modelData.path.indexOf(page.downloads + "/") !== 0

                    Column {
                        id: fileColumn
                        anchors {
                            left: parent.left
                            leftMargin: Theme.horizontalPageMargin
                            right: removeButton.left
                            verticalCenter: parent.verticalCenter
                        }

                        Label {
                            objectName: "sendFileName"
                            width: parent.width
                            text: modelData.name
                            textFormat: Text.PlainText
                            elide: Text.ElideMiddle
                        }
                        Label {
                            objectName: "sendFileOutside"
                            width: parent.width
                            visible: fileItem.outside
                            //: Send page: a file outside ~/Downloads, which the sandbox may not let Sukkula read.
                            text: qsTr("Outside Downloads: Sukkula may not be allowed to read it.")
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            font.pixelSize: Theme.fontSizeExtraSmall
                            color: Theme.secondaryHighlightColor
                        }
                    }
                    IconButton {
                        id: removeButton
                        anchors {
                            right: parent.right
                            rightMargin: Theme.paddingSmall
                            verticalCenter: parent.verticalCenter
                        }
                        icon.source: "image://theme/icon-m-clear"
                        onClicked: page.removeFile(index)
                    }
                }
            }

            Repeater {
                model: page.texts
                delegate: ListItem {
                    width: column.width
                    contentHeight: sharedText.height + 2 * Theme.paddingMedium

                    Label {
                        id: sharedText
                        objectName: "sharedText"
                        anchors {
                            left: parent.left
                            leftMargin: Theme.horizontalPageMargin
                            right: removeTextButton.left
                            verticalCenter: parent.verticalCenter
                        }
                        text: modelData
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        maximumLineCount: 3
                        elide: Text.ElideRight
                        font.pixelSize: Theme.fontSizeSmall
                    }
                    IconButton {
                        id: removeTextButton
                        anchors {
                            right: parent.right
                            rightMargin: Theme.paddingSmall
                            verticalCenter: parent.verticalCenter
                        }
                        icon.source: "image://theme/icon-m-clear"
                        onClicked: page.removeText(index)
                    }
                }
            }

            Button {
                x: Theme.horizontalPageMargin
                //: Opens the file picker.
                text: qsTr("Add file")
                enabled: page.files.length < page.maxFiles
                onClicked: page.pickFile()
            }

            TextArea {
                id: textArea
                objectName: "sendText"
                width: parent.width
                //: Placeholder of the text box on the send page.
                placeholderText: qsTr("Text to send (optional)")
                //: Label of the text box on the send page.
                label: qsTr("Text")
            }

            SectionHeader {
                //: Section heading: the way to send.
                text: qsTr("How")
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.available.length === 0
                //: Send page when every protocol is off.
                text: qsTr("Every way of sending is switched off in Settings.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Theme.secondaryHighlightColor
            }

            ComboBox {
                id: via
                objectName: "protocolChoice"
                width: parent.width
                visible: page.available.length > 0
                //: Choice of protocol on the send page.
                label: qsTr("Send via")
                currentIndex: Math.max(0, page.available.indexOf(page.protocol))
                menu: ContextMenu {
                    Repeater {
                        model: page.available
                        MenuItem {
                            text: page.engine.protocolName(modelData)
                            onClicked: page.protocol = modelData
                        }
                    }
                }
            }

            // LocalSend and Quick Share: the peers discovery has found.
            Column {
                width: parent.width
                visible: page.lan

                Repeater {
                    model: page.lan ? page.engine.peerModel(page.protocol) : null
                    delegate: BackgroundItem {
                        id: peerItem
                        objectName: "peerItem"
                        width: column.width
                        height: Theme.itemSizeMedium
                        enabled: !page.sending && page.itemCount > 0
                        onClicked: page.sendTo({ protocol: model.protocol, peer: model.peerId })

                        Column {
                            anchors {
                                left: parent.left
                                right: parent.right
                                leftMargin: Theme.horizontalPageMargin
                                rightMargin: Theme.horizontalPageMargin
                                verticalCenter: parent.verticalCenter
                            }
                            Label {
                                objectName: "peerName"
                                width: parent.width
                                text: model.name
                                textFormat: Text.PlainText
                                truncationMode: TruncationMode.Fade
                                color: peerItem.highlighted ? Theme.highlightColor : Theme.primaryColor
                            }
                            Label {
                                objectName: "peerModel"
                                width: parent.width
                                visible: text.length > 0
                                text: model.deviceModel
                                textFormat: Text.PlainText
                                truncationMode: TruncationMode.Fade
                                font.pixelSize: Theme.fontSizeExtraSmall
                                color: Theme.secondaryColor
                            }
                        }
                    }
                }

                Row {
                    x: Theme.horizontalPageMargin
                    spacing: Theme.paddingMedium
                    height: Theme.itemSizeSmall

                    BusyIndicator {
                        anchors.verticalCenter: parent.verticalCenter
                        size: BusyIndicatorSize.Small
                        running: page.lan && page.discovering
                    }
                    Label {
                        anchors.verticalCenter: parent.verticalCenter
                        width: column.width - 2 * Theme.horizontalPageMargin - Theme.itemSizeSmall
                        //: Send page: discovery is running.
                        text: qsTr("Looking for devices on this Wi-Fi…")
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.secondaryHighlightColor
                    }
                }
            }

            // Magic Wormhole: one file or one text, to a code (F-MW1).
            Column {
                width: parent.width
                visible: page.protocol === "wormhole"
                spacing: Theme.paddingMedium

                Label {
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    text: page.itemCount === 1
                          //: Send page, Magic Wormhole chosen.
                          ? qsTr("Sukkula makes a code. Read it out to the receiver, who types it into any Magic Wormhole app.")
                          //: Send page, Magic Wormhole chosen with more than one item.
                          : qsTr("Magic Wormhole sends one file or one text at a time.")
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryHighlightColor
                }
                Button {
                    objectName: "wormholeSend"
                    anchors.horizontalCenter: parent.horizontalCenter
                    //: Starts a Magic Wormhole send and shows its code.
                    text: qsTr("Make a code")
                    enabled: page.itemCount === 1 && !page.sending
                    onClicked: page.sendTo({ protocol: "wormhole" })
                }
            }

            // Bluetooth: paired devices only (F-BT1).
            Column {
                width: parent.width
                visible: page.protocol === "bluetooth"

                Repeater {
                    model: page.protocol === "bluetooth" ? page.engine.bluetoothDevices : null
                    delegate: BackgroundItem {
                        id: deviceItem
                        objectName: "bluetoothItem"
                        width: column.width
                        height: Theme.itemSizeMedium
                        enabled: !page.sending && page.files.length > 0 && !page.hasText
                        onClicked: page.sendTo({ protocol: "bluetooth", address: model.address })

                        Column {
                            anchors {
                                left: parent.left
                                right: parent.right
                                leftMargin: Theme.horizontalPageMargin
                                rightMargin: Theme.horizontalPageMargin
                                verticalCenter: parent.verticalCenter
                            }
                            Label {
                                objectName: "bluetoothName"
                                width: parent.width
                                text: model.name.length > 0 ? model.name : model.address
                                textFormat: Text.PlainText
                                truncationMode: TruncationMode.Fade
                                color: deviceItem.highlighted ? Theme.highlightColor : Theme.primaryColor
                            }
                            Label {
                                width: parent.width
                                text: model.address
                                textFormat: Text.PlainText
                                font.pixelSize: Theme.fontSizeExtraSmall
                                color: Theme.secondaryColor
                            }
                        }
                    }
                }

                Label {
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    visible: !page.listingDevices && page.engine.bluetoothDevices.count === 0
                    //: Send page, Bluetooth chosen, nothing paired.
                    text: qsTr("No paired devices. Pair one in the phone's Bluetooth settings first.")
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryHighlightColor
                }
                Label {
                    x: Theme.horizontalPageMargin
                    width: parent.width - 2 * Theme.horizontalPageMargin
                    visible: page.files.length === 0 || page.hasText
                    //: Send page, Bluetooth chosen with no files, or with a text.
                    text: qsTr("Bluetooth sends files only.")
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.secondaryHighlightColor
                }
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - 2 * Theme.horizontalPageMargin
                visible: page.itemCount === 0 && page.available.length > 0
                //: Send page with nothing chosen yet.
                text: qsTr("Add a file or type a text first.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.secondaryColor
            }
        }

        VerticalScrollDecorator {}
    }
}
