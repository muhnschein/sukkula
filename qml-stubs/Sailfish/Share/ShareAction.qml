import QtQuick 2.6

// Opens the system share sheet with `resources`. This one only counts how
// often it was asked to, and keeps what it was given, for tests.
QtObject {
    property string mimeType: ""
    property var resources: []
    property string title: ""
    property int triggered: 0
    function trigger() { triggered++ }
}
