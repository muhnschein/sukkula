import QtQuick 2.6

// Silica-owned text, AutoText on purpose: see qml-stubs/README.md.
Item {
    id: item
    property string text
    property bool down: false
    signal clicked()
    width: parent ? parent.width : 540
    height: 60

    Text { text: item.text }
}
