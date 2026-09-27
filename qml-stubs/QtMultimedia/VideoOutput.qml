import QtQuick 2.6

// QtMultimedia's VideoOutput, as far as ScanPage.qml uses it: an item that
// shows nothing.
Item {
    enum FillModes { Stretch = 0, PreserveAspectFit = 1, PreserveAspectCrop = 2 }

    property var source: null
    property int fillMode: 1
    property bool autoOrientation: false
}
