// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * What is about to be sent: the files chosen on the Send tab (F-C6), from
 * the pickers or the Share menu. Files only: Sukkula sends no texts of
 * its own (spec v0.7).
 */
QtObject {
    id: payload

    /// [{path, name, size}], absolute paths only, each once. `size` is in
    /// bytes, or -1 where nobody said (the Share menu gives paths only).
    property var files: []
    /// Goes up with every change: what a send took can be told from what
    /// was chosen since.
    property int revision: 0

    // Most files one offer may carry (S6), which the engine checks again.
    readonly property int maxFiles: 500
    readonly property int itemCount: payload.files.length
    /// Every file's size added up, or -1 while any is not known.
    readonly property real totalSize: {
        var sum = 0
        for (var i = 0; i < payload.files.length; i++) {
            if (payload.files[i].size < 0) {
                return -1
            }
            sum += payload.files[i].size
        }
        return sum
    }

    onFilesChanged: payload.revision++

    function basename(path) {
        var parts = String(path).split("/")
        return parts[parts.length - 1]
    }

    /// The file names, in the order they were chosen.
    function names() {
        var out = []
        for (var i = 0; i < payload.files.length; i++) {
            out.push(payload.files[i].name)
        }
        return out
    }

    /// Adds one file, `size` bytes (anything but a number at least 0 is
    /// not known). False when it cannot be taken.
    function addFile(path, size) {
        path = String(path)
        if (path.length === 0 || path.charAt(0) !== "/" || payload.files.length >= payload.maxFiles) {
            return false
        }
        for (var i = 0; i < payload.files.length; i++) {
            if (payload.files[i].path === path) {
                return true
            }
        }
        var known = typeof size === "number" && isFinite(size) && size >= 0
        var next = payload.files.slice(0)
        next.push({ path: path, name: payload.basename(path), size: known ? Math.floor(size) : -1 })
        payload.files = next
        return true
    }

    function removeFile(index) {
        var next = payload.files.slice(0)
        next.splice(index, 1)
        payload.files = next
    }

    function clear() {
        payload.files = []
    }

    /// Replaces everything with what the Share menu handed over:
    /// [{kind: "file", path}]; anything else is dropped.
    function load(items) {
        payload.clear()
        var list = Array.isArray(items) ? items : []
        for (var i = 0; i < list.length; i++) {
            var item = list[i]
            if (item && item.kind === "file" && typeof item.path === "string") {
                payload.addFile(item.path)
            }
        }
    }

    /// The send command's items.
    function items() {
        var out = []
        for (var i = 0; i < payload.files.length; i++) {
            out.push({ kind: "file", path: payload.files[i].path })
        }
        return out
    }
}
