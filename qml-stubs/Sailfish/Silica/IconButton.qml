import QtQuick 2.6

Item {
    property alias icon: image
    property bool down: false
    property bool highlighted: false
    signal clicked()
    width: 80
    height: 80

    Image {
        id: image
        anchors.centerIn: parent
    }
}
