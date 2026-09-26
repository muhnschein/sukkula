// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Pickers 1.0

/*
 * Files to send, several at once, from Silica's file browser.
 *
 * The browser rather than the content pickers: those list what the media
 * index knows, which needs the MediaIndexing permission Sukkula does not
 * have (spec §2). The browser runs in this process, so it shows exactly
 * what Sailjail lets Sukkula read: Downloads, Documents, Music, Pictures,
 * Videos and memory cards, and files arriving from the Share menu.
 *
 * Sailfish.Pickers is named here and in SingleFilePicker.qml, and nowhere
 * else.
 */
MultiFilePickerDialog {
    id: picker

    /// Absolute paths were chosen.
    signal picked(var paths)

    /// A chosen item's path: `filePath` where the picker gives it, else
    /// its local file URL's.
    function pathOf(item) {
        if (!item) {
            return ""
        }
        if (typeof item.filePath === "string" && item.filePath.length > 0) {
            return item.filePath
        }
        var url = String(item.url || "")
        if (url.indexOf("file://") !== 0) {
            return ""
        }
        try {
            return decodeURIComponent(url.substring(7))
        } catch (err) {
            return ""
        }
    }

    onAccepted: {
        var paths = []
        var chosen = picker.selectedContent
        for (var i = 0; chosen && i < chosen.count; i++) {
            var path = picker.pathOf(chosen.get(i))
            if (path.length > 0) {
                paths.push(path)
            }
        }
        if (paths.length > 0) {
            picker.picked(paths)
        }
    }
}
