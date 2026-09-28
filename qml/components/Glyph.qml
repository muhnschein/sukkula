// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One of the ambience's own icons, by what it stands for: the devices (a
 * phone or tablet, a computer, a paired Bluetooth device), the kinds of
 * file (a photo, a video, music, a document, any file), a folder, a text
 * message, the camera and the keyboard for a code, a code sent over the
 * internet, and this phone receiving. A file's kind is told by its name
 * alone: no picture a peer sent is ever drawn.
 *
 * From the theme's image provider, tinted with `color` the way Silica's
 * own lists tint theme icons ("image://theme/<name>?<colour>"), at the
 * size the theme draws it: the large icon where there is one and the item
 * is that big. Should a name be missing from a release's theme, the next
 * one is tried, down to icons every release so far has had.
 */
Image {
    id: glyph

    /// "phone", "tablet", "computer", "bluetooth", "file", "photo",
    /// "video", "music", "document", "folder", "text", "camera",
    /// "keyboard", "code" or "receive".
    property string kind: "file"
    property color color: Theme.primaryColor

    readonly property bool large: glyph.width >= Theme.iconSizeLarge
    /// The icons to try, in order.
    readonly property var names: glyph.namesFor(glyph.kind, glyph.large)
    /// How many of them the theme did not have.
    property int missed: 0
    readonly property string iconName: glyph.names[Math.min(glyph.missed, glyph.names.length - 1)]

    function namesFor(kind, large) {
        switch (kind) {
        case "phone":
        case "tablet":
            return ["icon-m-device", "icon-m-phone"]
        case "computer": return large ? ["icon-l-computer", "icon-m-computer"] : ["icon-m-computer"]
        case "bluetooth": return ["icon-m-bluetooth-device", "icon-m-bluetooth"]
        case "photo": return large ? ["icon-l-image", "icon-m-image"] : ["icon-m-image"]
        case "video": return large ? ["icon-l-video", "icon-m-video"] : ["icon-m-video"]
        case "music": return large ? ["icon-l-music", "icon-m-music"] : ["icon-m-music"]
        case "document": return large ? ["icon-l-document", "icon-m-document"] : ["icon-m-document"]
        case "folder": return ["icon-m-folder"]
        case "text": return ["icon-m-message", "icon-m-sms"]
        case "camera": return ["icon-m-camera"]
        case "keyboard": return ["icon-m-keyboard", "icon-m-edit"]
        case "code": return ["icon-m-cloud-upload", "icon-m-share"]
        case "receive": return ["icon-m-device-download", "icon-m-device"]
        }
        return ["icon-m-file-other", "icon-m-document"]
    }

    onNamesChanged: glyph.missed = 0
    onStatusChanged: {
        if (glyph.status === Image.Error && glyph.missed < glyph.names.length - 1) {
            glyph.missed++
        }
    }

    source: "image://theme/" + glyph.iconName + "?" + glyph.color
    width: Theme.iconSizeMedium
    height: glyph.width
    fillMode: Image.PreserveAspectFit
    smooth: true
}
