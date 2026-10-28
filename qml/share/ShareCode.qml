// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Share 1.0

/*
 * A code to send with, handed to the system's share sheet as text
 * (F-MW1, F-CR1): to a message, a mail, anything that takes text. Only
 * the code goes, which Sukkula's own engine made.
 *
 * Loaded by the code page through a Loader, so that a fault in this one
 * platform module costs the Share button rather than the page.
 */
ShareAction {
    id: action

    /// The code, as the engine gave it.
    property string code: ""

    mimeType: "text/plain"
    resources: action.code.length > 0 ? [{ "data": action.code, "name": "code.txt", "type": "text/plain" }] : []
    //: Title of the share sheet for a code to send with.
    title: qsTr("Share code")
}
