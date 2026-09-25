import QtQuick 2.6

// Silica-owned text, AutoText on purpose: see qml-stubs/README.md.
Item {
    id: header
    property string text
    width: parent ? parent.width : 540
    height: 60

    Text { text: header.text }
}
