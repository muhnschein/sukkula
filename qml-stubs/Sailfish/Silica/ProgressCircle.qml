import QtQuick 2.6

// Silica's ProgressCircle: a ring filled round to `value`.
Item {
    property real value: 0
    property color progressColor: "white"
    property color backgroundColor: "grey"
    property real borderWidth: 2
    property bool inAlternateCycle: false
    width: 100
    height: 100
}
