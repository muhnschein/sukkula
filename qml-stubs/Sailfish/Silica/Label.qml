import QtQuick 2.6

// Silica's Label is a Text. Left at Qt's default AutoText here, so a label
// in the app that does not say `textFormat: Text.PlainText` is caught.
Text {
    property int truncationMode: 0
    property bool highlighted: false
    color: "#ffffff"
}
