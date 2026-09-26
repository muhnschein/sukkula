// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Pickers 1.0

/*
 * One file to send, from Silica's file browser: what SendView falls back
 * to where FilePicker.qml's dialog for several cannot be loaded.
 *
 * See FilePicker.qml for why the browser and not the content pickers.
 */
FilePickerPage {
    id: picker

    /// Absolute paths were chosen: one, here.
    signal picked(var paths)

    onSelectedContentPropertiesChanged: {
        var chosen = picker.selectedContentProperties
        if (chosen && chosen.filePath) {
            picker.picked([String(chosen.filePath)])
        }
    }
}
