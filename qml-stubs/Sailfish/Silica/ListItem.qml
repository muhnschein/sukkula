import QtQuick 2.6

Item {
    property real contentHeight: 80
    property var menu: null
    property bool down: false
    property bool highlighted: false
    property bool showMenuOnPressAndHold: true
    signal clicked()
    signal pressAndHold()
    width: parent ? parent.width : 540
    height: contentHeight
    function showMenu() {}
}
