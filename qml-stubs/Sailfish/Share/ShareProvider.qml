import QtQuick 2.6

// What the platform triggers when something is shared to the app. The
// real one registers `method` against the desktop file's group of the same
// name; this one only carries what a test hands it.
QtObject {
    property string method: ""
    property var capabilities: []
    property bool registerName: false
    signal triggered(var resources)
}
