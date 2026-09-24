import QtQuick 2.6

// Silica's TextField is a TextInput with a label, a placeholder and a
// description; those three are Silica-owned text, AutoText on purpose.
TextInput {
    id: field
    property string label
    property string placeholderText
    property string description
    property bool errorHighlight: false
    width: parent ? parent.width : 540
    height: 100

    Text { y: 40; text: field.label }
    Text { y: 60; text: field.text.length === 0 ? field.placeholderText : "" }
    Text { y: 80; text: field.description }
}
