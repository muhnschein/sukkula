import QtQuick 2.6
import Sailfish.Silica 1.0

// Silica's Dialog is a Page with accept and reject. Either one emits its
// signal and takes the dialog off the stack, as the real one does.
Item {
    id: dialog

    property var pageStack: null
    property int status: PageStatus.Inactive
    property int allowedOrientations: 0
    property bool backNavigation: true
    property bool forwardNavigation: true
    property bool canAccept: true
    property var acceptDestination
    property int result: 0

    signal accepted()
    signal rejected()
    signal done()

    function accept() {
        if (!dialog.canAccept) {
            return
        }
        dialog.result = 1
        dialog.accepted()
        dialog.done()
        if (dialog.pageStack) {
            dialog.pageStack._remove(dialog)
        }
    }

    function reject() {
        dialog.result = 2
        dialog.rejected()
        dialog.done()
        if (dialog.pageStack) {
            dialog.pageStack._remove(dialog)
        }
    }

    function open() {}
}
