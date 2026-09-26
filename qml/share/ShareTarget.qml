// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Share 1.0

/*
 * Sukkula in the system Share menu (F-C6): files from the gallery or the
 * file manager, text and links from anywhere.
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
 * What arrives is only a list of paths and texts for the centre of the
 * main page's send radar: nothing is sent until the user taps a peer.
 */
Item {
    id: target

    /// [{kind: "file", path} | {kind: "text", text}], never empty.
    signal shared(var items)

    /*
     * The words the share sheet shows. It reads them from the desktop
     * file, which no catalogue reaches, so they are repeated here for
     * lupdate; they must match the Description= of the group of the same
     * name in harbour-sukkula.desktop (tests/qml/static_checks.py checks).
     */
    //: Shown in the phone's share sheet, for files shared to Sukkula.
    readonly property string filesDescription: qsTr("Send nearby")
    //: Shown in the phone's share sheet, for text or a link shared to Sukkula.
    readonly property string textDescription: qsTr("Send text nearby")

    // Most items taken from one share: an offer carries at most 500 files
    // (S6), and the engine checks again.
    readonly property int maxItems: 500
    // Longest text taken; the engine's own limit is 64 KiB (S6).
    readonly property int maxTextChars: 65536

    ShareProvider {
        objectName: "shareFiles"
        method: "files"
        registerName: true
        onTriggered: target.take(resources)
    }

    ShareProvider {
        objectName: "shareText"
        method: "text"
        capabilities: ["text/*", "text/x-url", "text/uri-list"]
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

    /// Told apart by what each resource carries rather than by its type
    /// enum, which no host stub can stand in for: a resource has a path or
    /// it has data.
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
            } else if (one.data && String(one.data).length > 0) {
                var text = String(one.data)
                if (text.length > target.maxTextChars) {
                    // Bounded, not split inside a surrogate pair; the
                    // engine refuses what is still too long, and says so.
                    var cut = target.maxTextChars
                    var last = text.charCodeAt(cut - 1)
                    if (last >= 0xd800 && last <= 0xdbff) {
                        cut--
                    }
                    text = text.substring(0, cut)
                }
                items.push({ kind: "text", text: text })
            }
        }
        if (items.length > 0) {
            target.shared(items)
        }
    }
}
