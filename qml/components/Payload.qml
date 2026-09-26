// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * What is about to be sent: the files and texts at the centre of the send
 * radar (F-C6). Filled from the Share menu or the file picker, and by the
 * text typed on the "What to send" page.
 *
 * A shared text is kept apart from the typed one: it can be anything,
 * markup included, and is only ever shown in the app's own plain-text
 * labels, never put into a Silica text box (S2).
 */
QtObject {
    id: payload

    /// [{path, name}], absolute paths only, each once.
    property var files: []
    /// Texts from the Share menu.
    property var texts: []
    /// The text typed on the "What to send" page.
    property string typed: ""
    /// Goes up with every change: what a send took can be told from what
    /// was chosen since.
    property int revision: 0

    // Most files one offer may carry (S6), which the engine checks again.
    readonly property int maxFiles: 500
    readonly property int itemCount: payload.files.length + payload.texts.length
                                     + (payload.typed.length > 0 ? 1 : 0)
    readonly property bool hasText: payload.texts.length > 0 || payload.typed.length > 0

    onFilesChanged: payload.revision++
    onTextsChanged: payload.revision++
    onTypedChanged: payload.revision++

    function basename(path) {
        var parts = String(path).split("/")
        return parts[parts.length - 1]
    }

    function addFile(path) {
        path = String(path)
        if (path.length === 0 || path.charAt(0) !== "/" || payload.files.length >= payload.maxFiles) {
            return false
        }
        for (var i = 0; i < payload.files.length; i++) {
            if (payload.files[i].path === path) {
                return true
            }
        }
        var next = payload.files.slice(0)
        next.push({ path: path, name: payload.basename(path) })
        payload.files = next
        return true
    }

    function removeFile(index) {
        var next = payload.files.slice(0)
        next.splice(index, 1)
        payload.files = next
    }

    function removeText(index) {
        var next = payload.texts.slice(0)
        next.splice(index, 1)
        payload.texts = next
    }

    function clear() {
        payload.files = []
        payload.texts = []
        payload.typed = ""
    }

    /// Replaces everything with what the Share menu handed over:
    /// [{kind: "file", path} | {kind: "text", text}]; anything else is
    /// dropped.
    function load(items) {
        payload.clear()
        var list = Array.isArray(items) ? items : []
        var texts = []
        for (var i = 0; i < list.length; i++) {
            var item = list[i]
            if (!item) {
                continue
            }
            if (item.kind === "file" && typeof item.path === "string") {
                payload.addFile(item.path)
            } else if (item.kind === "text" && typeof item.text === "string") {
                texts.push(item.text)
            }
        }
        payload.texts = texts
    }

    /// The send command's items.
    function items() {
        var out = []
        for (var i = 0; i < payload.files.length; i++) {
            out.push({ kind: "file", path: payload.files[i].path })
        }
        for (var t = 0; t < payload.texts.length; t++) {
            out.push({ kind: "text", text: payload.texts[t] })
        }
        if (payload.typed.length > 0) {
            out.push({ kind: "text", text: payload.typed })
        }
        return out
    }
}
