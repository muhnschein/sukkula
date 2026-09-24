import QtQuick 2.6

// Draws `title` and `description` with Silica-owned text items, left at
// AutoText: a peer string handed to them is what the tests look for.
Item {
    property string title
    property string description
    width: parent ? parent.width : 540
    height: 110

    Text {
        objectName: "pageHeaderTitle"
        text: parent.title
    }
    Text {
        y: 60
        text: parent.description
    }
}
