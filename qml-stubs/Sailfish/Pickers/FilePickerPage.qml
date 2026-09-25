import QtQuick 2.6
import Sailfish.Silica 1.0

// Silica's file browser. A test drives it by assigning to
// selectedContentProperties, which is what a tap on a file does.
Page {
    property string title: ""
    property var nameFilters: []
    property bool showSystemFiles: false
    property var selectedContentProperties: ({})
}
