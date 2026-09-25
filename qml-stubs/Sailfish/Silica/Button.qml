import QtQuick 2.6

// Silica-owned text, AutoText on purpose: see qml-stubs/README.md.
Item {
    id: button
    property string text
    property color color: "#ffffff"
    property bool down: false
    property bool highlighted: false
    signal clicked()
    width: 240
    height: 70

    Text { text: button.text }
}
