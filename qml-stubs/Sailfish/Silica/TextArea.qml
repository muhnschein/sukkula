import QtQuick 2.6

// Silica's TextArea is a TextEdit with a label and a placeholder, both
// Silica-owned text, AutoText on purpose. The TextEdit keeps Qt's default.
TextEdit {
    id: area
    property string label
    property string placeholderText
    property string description
    property bool errorHighlight: false
    width: parent ? parent.width : 540
    height: 160

    Text { y: 120; text: area.label }
    Text { y: 140; text: area.text.length === 0 ? area.placeholderText : "" }
}
