pragma Singleton
import QtQuick 2.6

// Silica's Screen: the values the app reads.
QtObject {
    property int width: 540
    property int height: 960
    property real sizeCategory: 1
    // The display cutout's rect. ModeTabs clears it the way PageHeader does.
    property rect topCutout: Qt.rect(0, 0, 0, 0)
}
