// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Pickers 1.0

/*
 * Where files to send are chosen (F-C6): the platform's content picker,
 * which bundles pictures, videos, music, documents and the file system in
 * one dialog, as piirit's attach button does (its AttachLibraryPage).
 *
 * The media it lists come from the media index, which needs the
 * MediaIndexing permission (spec v0.7); its file system shows what Sailjail
 * lets Sukkula read: Downloads, Documents, Music, Pictures, Videos and
 * memory cards. It hands back paths and sizes only; nothing here opens a
 * file or draws one.
 *
 * Sailfish.Pickers is named here and in SingleFilePicker.qml, and nowhere
 * else.
 */
QtObject {
    id: pickers

    /// Files were chosen: [{path, size}], `size` in bytes or -1 where the
    /// picker did not say.
    signal picked(var files)

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

    function sizeOf(item) {
        var n = item ? Number(item.fileSize) : NaN
        return isFinite(n) && n >= 0 ? Math.floor(n) : -1
    }

    /// What was ticked, out on `picked`.
    function take(chosen) {
        var files = []
        for (var i = 0; chosen && i < chosen.count; i++) {
            var item = chosen.get(i)
            var path = pickers.pathOf(item)
            if (path.length > 0) {
                files.push({ path: path, size: pickers.sizeOf(item) })
            }
        }
        if (files.length > 0) {
            pickers.picked(files)
        }
    }

    property Component content: Component {
        MultiContentPickerDialog {
            id: contentDialog
            objectName: "contentPicker"
            onAccepted: pickers.take(contentDialog.selectedContent)
        }
    }
}
