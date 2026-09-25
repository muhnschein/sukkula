import QtQuick 2.6
import Sailfish.Silica 1.0

// Silica's ApplicationWindow: an Item holding a working page stack
// (PageStack.qml), the initial page, and the cover, created so that the
// cover's bindings run too.
Item {
    id: window

    property var initialPage
    property var cover
    property int allowedOrientations: 0
    property int defaultAllowedOrientations: 0
    property alias pageStack: stack
    property Item coverItem: null
    // For tests: how often activate() was called.
    property int activateCount: 0

    width: 540
    height: 960

    function activate() {
        window.activateCount++
    }

    function deactivate() {}

    PageStack {
        id: stack
        anchors.fill: parent
    }

    Item {
        id: coverHolder
        width: 234
        height: 374
    }

    Component.onCompleted: {
        if (window.initialPage) {
            stack.push(window.initialPage)
        }
        if (window.cover) {
            var comp = typeof window.cover.createObject === "function"
                       ? window.cover : Qt.createComponent(window.cover)
            window.coverItem = comp.createObject(coverHolder)
            if (!window.coverItem) {
                console.warn("ApplicationWindow stub: cover failed: " + comp.errorString())
            }
        }
    }
}
