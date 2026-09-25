// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Pickers 1.0

/*
 * A file to send, from Silica's file browser.
 *
 * The browser rather than the content pickers: those list what the media
 * index knows, which needs the MediaIndexing permission Sukkula does not
 * have (spec §2). The browser runs in this process, so it shows exactly
 * what Sailjail lets Sukkula read -- with Internet;Bluetooth;Downloads
 * that is ~/Downloads, and files arriving from the Share menu.
 *
 * Its own file, so Sailfish.Pickers is named here and nowhere else.
 */
FilePickerPage {
    id: picker

    /// An absolute path was chosen.
    signal picked(string path)

    onSelectedContentPropertiesChanged: {
        var chosen = picker.selectedContentProperties
        if (chosen && chosen.filePath) {
            picker.picked(String(chosen.filePath))
        }
    }
}
