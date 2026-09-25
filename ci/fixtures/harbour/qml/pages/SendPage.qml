import QtQuick 2.6
import Sailfish.Silica 1.0
import Sailfish.Pickers 1.0
import Sailfish.Share 1.0

Page {
    ShareProvider { method: "files" }
    Component { id: picker; FilePickerPage { } }
}
