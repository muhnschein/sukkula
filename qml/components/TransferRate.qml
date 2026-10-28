// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * How much of a transfer has gone, and roughly how long is left at the
 * rate so far (F-C5): "4.3 MB of 8.2 MB · about 10 seconds left".
 */
QtObject {
    id: rate

    property real bytes: 0
    property real total: 0
    /// Still going: an ended transfer has no time left to say.
    property bool active: true

    /// When the bytes started to come, and how many there were then.
    property real since: 0
    property real from: 0
    property real now: 0

    onBytesChanged: {
        rate.now = Date.now()
        if (rate.bytes > 0 && rate.since === 0) {
            rate.since = rate.now
            rate.from = rate.bytes
        }
    }

    /// Another transfer: the rate starts again.
    function reset() {
        rate.since = 0
        rate.from = 0
        rate.now = 0
    }

    /// Seconds left at the rate so far, or -1 while that says nothing yet.
    function secondsLeft() {
        var took = (rate.now - rate.since) / 1000
        var done = rate.bytes - rate.from
        if (!rate.active || rate.since === 0 || took < 2 || done <= 0 || rate.total <= rate.bytes) {
            return -1
        }
        return Math.ceil((rate.total - rate.bytes) / (done / took))
    }

    function sizeLine() {
        if (!rate.active || rate.total <= 0) {
            return ""
        }
        //: Progress: %1 bytes so far, %2 bytes in all, both formatted.
        var line = qsTr("%1 of %2").arg(Format.formatFileSize(rate.bytes)).arg(Format.formatFileSize(rate.total))
        var left = rate.secondsLeft()
        if (left < 0) {
            return line
        }
        //: Progress: how long is left, after how much is done; %1 is that, e.g. "4.3 MB of 8.2 MB".
        return left < 90 ? qsTr("%1 · about %n second(s) left", "", left).arg(line)
                         //: Progress: how long is left, after how much is done; %1 is that.
                         : qsTr("%1 · about %n minute(s) left", "", Math.round(left / 60)).arg(line)
    }
}
