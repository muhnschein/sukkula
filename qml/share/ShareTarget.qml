// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Share 1.0

/*
 * Sukkula in the system Share menu (F-C6): files from Gallery, the file
 * manager, anywhere. Files only: Sukkula sends no texts (spec v0.7).
 *
 * The desktop file offers the methods (`X-Share-Methods`, and a group per
 * method with its name in the sheet); a `ShareProvider` of the same name
 * here receives what is shared. The platform calls the provider over
 * D-Bus under <OrganizationName>.<ApplicationName> -- sukkula.sukkula,
 * the one name Sailjail lets this app own -- which `registerName` claims,
 * and starts the app through [X-Sailjail] ExecDBus when it is not running.
 *
 * Loaded by the window through a Loader, so that a fault in this one
 * platform module costs sharing rather than the window.
 *
 * What arrives is only a list of paths, chosen on the main page's Send
 * tab: nothing is sent until the user taps a device.
 */
Item {
    id: target

    /// [{kind: "file", path}], never empty.
    signal shared(var items)

    /*
     * The words the share sheet shows. It reads them from the desktop
     * file, which no catalogue reaches, so they are repeated here for
     * lupdate; they must match the Description= of the group of the same
     * name in harbour-sukkula.desktop (tests/qml/static_checks.py checks).
     */
    //: Shown in the phone's share sheet, for files shared to Sukkula.
    readonly property string filesDescription: qsTr("Send nearby")

    // Most items taken from one share: an offer carries at most 500 files
    // (S6), and the engine checks again.
    readonly property int maxItems: 500

    ShareProvider {
        objectName: "shareFiles"
        method: "files"
        registerName: true
        onTriggered: target.take(resources)
    }

    /// A local path from a resource's filePath or file:// url, or "".
    function pathOf(resource) {
        var path = resource.filePath ? String(resource.filePath) : ""
        if (path.length === 0 && resource.url) {
            var url = String(resource.url)
            if (url.indexOf("file:///") === 0) {
                try {
                    path = decodeURIComponent(url.substring(7))
                } catch (err) {
                    path = ""
                }
            }
        }
        return path.charAt(0) === "/" ? path : ""
    }

    /// Files only: a resource with a local path. Anything else -- a text,
    /// a link -- is not Sukkula's to send.
    function take(resources) {
        var items = []
        var count = resources && typeof resources.length === "number" ? resources.length : 0
        for (var i = 0; i < count && items.length < target.maxItems; i++) {
            var one = resources[i]
            if (!one) {
                continue
            }
            var path = target.pathOf(one)
            if (path.length > 0) {
                items.push({ kind: "file", path: path })
            }
        }
        if (items.length > 0) {
            target.shared(items)
        }
    }
}
