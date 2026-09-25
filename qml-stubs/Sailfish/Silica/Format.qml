pragma Singleton
import QtQuick 2.6

// Silica's Format: only formatFileSize, which the app uses for sizes.
QtObject {
    function formatFileSize(bytes) {
        var n = Number(bytes)
        if (!isFinite(n) || n < 0) {
            n = 0
        }
        var units = ["B", "kB", "MB", "GB", "TB"]
        var u = 0
        while (n >= 1000 && u < units.length - 1) {
            n = n / 1000
            u++
        }
        return (u === 0 ? n : n.toFixed(1)) + " " + units[u]
    }
}
