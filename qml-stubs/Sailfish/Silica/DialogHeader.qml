import QtQuick 2.6

// Silica-owned text, AutoText on purpose: see qml-stubs/README.md.
Item {
    property string title
    property string acceptText
    property string cancelText
    width: parent ? parent.width : 540
    height: 110

    Text { text: parent.title }
    Text { y: 40; objectName: "dialogAcceptText"; text: parent.acceptText }
    Text { y: 80; objectName: "dialogCancelText"; text: parent.cancelText }
}
