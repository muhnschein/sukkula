// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6

/*
 * A test: `steps` run one after another, with the event loop turning in
 * between so queued events arrive and bindings settle. A step may return a
 * number of milliseconds to wait before the next one. The runner
 * (tests/qml/runner) waits for `done` and reports `failures`.
 */
Item {
    id: script

    property var steps: []
    property int interval: 20
    property bool done: false
    property var failures: []
    property string current: ""
    property int _index: 0

    width: 540
    height: 960

    function fail(message) {
        var next = script.failures.slice(0)
        next.push(script.current + ": " + message)
        script.failures = next
    }

    function verify(condition, message) {
        if (!condition) {
            script.fail(message ? message : "verify failed")
        }
    }

    /// JSON with object keys sorted, so objects compare by content.
    function canonical(value) {
        if (Array.isArray(value)) {
            var items = []
            for (var i = 0; i < value.length; i++) {
                items.push(script.canonical(value[i]))
            }
            return "[" + items.join(",") + "]"
        }
        if (value !== null && typeof value === "object") {
            var keys = Object.keys(value).sort()
            var parts = []
            for (var k = 0; k < keys.length; k++) {
                parts.push(JSON.stringify(keys[k]) + ":" + script.canonical(value[keys[k]]))
            }
            return "{" + parts.join(",") + "}"
        }
        var s = JSON.stringify(value)
        return s === undefined ? "undefined" : s
    }

    function compare(actual, expected, message) {
        var a = script.canonical(actual)
        var e = script.canonical(expected)
        if (a !== e) {
            script.fail((message ? message : "compare") + ": got " + a + ", expected " + e)
        }
    }

    property var _reported: ({})

    /// No text item below `root` breaks the plain-text rule right now.
    /// Each violation is reported once, however often it is seen.
    function verifyPlainText(root, where) {
        var violations = probe.plainTextViolations(root)
        for (var i = 0; i < violations.length; i++) {
            if (script._reported[violations[i]] !== true) {
                script._reported[violations[i]] = true
                script.fail((where ? where + ": " : "") + violations[i])
            }
        }
    }

    function start() {
        script._index = 0
        stepper.interval = script.interval
        stepper.start()
    }

    function _next() {
        // Between every two steps, the whole tree is held to the
        // plain-text rule: pages that come and go are checked while they
        // are there.
        script.verifyPlainText(script, "")
        if (script._index >= script.steps.length) {
            script.current = "end"
            script.done = true
            return
        }
        var step = script.steps[script._index]
        script._index++
        script.current = "step " + script._index
        var wait = script.interval
        try {
            var asked = step()
            if (typeof asked === "number") {
                wait = asked
            }
        } catch (err) {
            script.fail("threw " + err + (err && err.stack ? "\n" + err.stack : ""))
        }
        stepper.interval = Math.max(1, wait)
        stepper.start()
    }

    Timer {
        id: stepper
        repeat: false
        onTriggered: script._next()
    }

    Component.onCompleted: script.start()
}
