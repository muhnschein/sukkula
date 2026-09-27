import QtQuick 2.6

// Silica's swipeable pager (a C++ type: "Sailfish.Silica/PagedView 1.0").
//
// Every delegate is made at once, as with a cacheSize that covers them
// all, and laid side by side, a page apart, with the current one at
// x = 0: a test sees which one is on screen by its x. moveTo() is what a
// swipe or a tab does.
Item {
    id: root

    property var model: 0
    property Component delegate: null
    property int currentIndex: 0
    readonly property int count: repeater.count
    readonly property Item currentItem: repeater.count > root.currentIndex && root.currentIndex >= 0
                                        ? repeater.itemAt(root.currentIndex)
                                        : null
    readonly property Item contentItem: root
    property bool interactive: true
    readonly property bool dragging: false
    readonly property bool moving: false
    property int cacheSize: 0
    property real horizontalSpacing: 0
    property real verticalSpacing: 0

    function moveTo(index, transition) {
        root.currentIndex = index
    }
    function itemAt(index) {
        return repeater.itemAt(index)
    }

    Repeater {
        id: repeater
        model: root.model
        delegate: root.delegate
        onItemAdded: {
            item.x = Qt.binding(function () { return (index - root.currentIndex) * root.width })
        }
    }
}
