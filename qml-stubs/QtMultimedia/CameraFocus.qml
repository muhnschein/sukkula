import QtQuick 2.6

// Camera.focus: typed, so that `focus.focusMode:` assigns.
QtObject {
    property int focusMode: 8
    property int focusPointMode: 0
    property point customFocusPoint: Qt.point(0.5, 0.5)
}
