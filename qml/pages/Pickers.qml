// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Pickers 1.0

/*
 * Where files to send are chosen (F-C6): the platform's own pickers, one
 * per tile of the Send tab -- photos and videos as Gallery shows them,
 * documents as the Documents app lists them, and the file browser for any
 * file at all.
 *
 * The first three list what the media index knows, which needs the
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

    /// The picker for "photo", "video", "document" or "file".
    function component(kind) {
        switch (kind) {
        case "photo": return pickers.photos
        case "video": return pickers.videos
        case "document": return pickers.documents
        }
        return pickers.files
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

    property Component photos: Component {
        MultiImagePickerDialog {
            id: photoDialog
            objectName: "photoPicker"
            onAccepted: pickers.take(photoDialog.selectedContent)
        }
    }
    property Component videos: Component {
        MultiVideoPickerDialog {
            id: videoDialog
            objectName: "videoPicker"
            onAccepted: pickers.take(videoDialog.selectedContent)
        }
    }
    property Component documents: Component {
        MultiDocumentPickerDialog {
            id: documentDialog
            objectName: "documentPicker"
            onAccepted: pickers.take(documentDialog.selectedContent)
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
