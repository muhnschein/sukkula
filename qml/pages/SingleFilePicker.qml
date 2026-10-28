// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Pickers 1.0

/*
 * One file to send, from Silica's file browser: what SendView falls back
 * to where Pickers.qml's dialogs for several cannot be loaded.
 */
FilePickerPage {
    id: picker

    /// Files were chosen: [{path, size}], one here.
    signal picked(var files)

    onSelectedContentPropertiesChanged: {
        var chosen = picker.selectedContentProperties
        if (chosen && chosen.filePath) {
            var size = Number(chosen.fileSize)
            picker.picked([{ path: String(chosen.filePath),
                             size: isFinite(size) && size >= 0 ? Math.floor(size) : -1 }])
        }
    }
}
