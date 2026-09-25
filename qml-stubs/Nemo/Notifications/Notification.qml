import QtQuick 2.6

// Nemo's Notification, recording what it was asked to do.
QtObject {
    property string appName
    property string appIcon
    property string summary
    property string body
    property string previewSummary
    property string previewBody
    property string category
    property bool isTransient: false
    property int urgency: 1
    property int expireTimeout: -1
    property int replacesId: 0
    property var remoteActions: []

    // What the tests read.
    property bool isPublished: false
    property int publishCount: 0
    property int closeCount: 0

    function publish() {
        isPublished = true
        publishCount += 1
        if (replacesId === 0) {
            replacesId = publishCount
        }
    }

    function close() {
        isPublished = false
        closeCount += 1
    }
}
