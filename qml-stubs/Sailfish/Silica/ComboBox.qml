import QtQuick 2.6

// Silica's ComboBox: a label, the chosen item's text as `value`, and a
// ContextMenu of MenuItems. Silica-owned text, AutoText on purpose.
Item {
    id: box
    property string label
    property string description
    property int currentIndex: 0
    property string value: box._textAt(box.currentIndex)
    property Item menu
    property Item currentItem: null
    width: parent ? parent.width : 540
    height: 100

    function _items() {
        var out = []
        if (!box.menu) {
            return out
        }
        var queue = [box.menu]
        while (queue.length > 0) {
            var node = queue.shift()
            var kids = node.children
            for (var i = 0; i < kids.length; i++) {
                if (kids[i].hasOwnProperty("text") && typeof kids[i].clicked === "function"
                        && kids[i].hasOwnProperty("down")) {
                    out.push(kids[i])
                } else {
                    queue.push(kids[i])
                }
            }
        }
        return out
    }

    function _textAt(index) {
        var items = box._items()
        return index >= 0 && index < items.length ? items[index].text : ""
    }

    // What a tap on the index'th menu item does, for tests.
    function choose(index) {
        var items = box._items()
        if (index < 0 || index >= items.length) {
            return
        }
        box.currentIndex = index
        box.currentItem = items[index]
        items[index].clicked()
    }

    Text { text: box.label }
    Text { y: 40; text: box.value }
    Text { y: 70; text: box.description }
}
