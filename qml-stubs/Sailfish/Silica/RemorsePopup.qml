import QtQuick 2.6

// Silica's RemorsePopup: says what is about to happen, and does it after
// a countdown unless cancelled. Here the countdown is a test's to end:
// trigger() does what the end of it does. The text is Sukkula's own.
Item {
    id: popup
    property string text: ""
    property bool active: false
    property var _callback: null
    signal triggered()
    signal canceled()

    function execute(title, callback, timeout) {
        popup.text = title
        popup._callback = callback
        popup.active = true
    }
    function trigger() {
        var callback = popup._callback
        popup.active = false
        popup._callback = null
        if (callback) {
            callback()
        }
        popup.triggered()
    }
    function cancel() {
        popup.active = false
        popup._callback = null
        popup.canceled()
    }

    Text { text: popup.active ? popup.text : "" }
}
