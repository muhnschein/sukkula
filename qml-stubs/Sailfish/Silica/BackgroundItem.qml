import QtQuick 2.6

Item {
    property bool down: false
    property bool highlighted: false
    signal clicked()
    width: parent ? parent.width : 540
    height: 80
}
