// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Pickers 1.0

/*
 * Where files to send are chosen (F-C6), in the platform's own pickers:
 * the content picker, which bundles pictures, videos, music and documents
 * in one dialog, as piirit's attach button does (its AttachLibraryPage);
 * and the file browser, for a file the media index does not list.
 *
 * The content picker lists what the media index knows, which needs the
 * MediaIndexing permission (spec v0.7). The file browser runs in this
 * process and shows exactly what Sailjail lets Sukkula read: Downloads,
 * Documents, Music, Pictures, Videos and memory cards. Every picker hands
 * back paths and sizes only; nothing here opens a file or draws one.
 *
 * Sailfish.Pickers is named here and in SingleFilePicker.qml, and nowhere
 * else.
 */
QtObject {
    id: pickers

    /// Files were chosen: [{path, size}], `size` in bytes or -1 where the
    /// picker did not say.
    signal picked(var files)

    /// The picker for "content" or "files".
    function component(kind) {
        return kind === "files" ? pickers.files : pickers.content
    }

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
    property Component files: Component {
        MultiFilePickerDialog {
            id: fileDialog
            objectName: "filePicker"
            onAccepted: pickers.take(fileDialog.selectedContent)
        }
    }
}
