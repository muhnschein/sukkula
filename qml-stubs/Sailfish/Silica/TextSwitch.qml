import QtQuick 2.6

// Silica-owned text, AutoText on purpose: see qml-stubs/README.md.
// Clicking toggles `checked` only with automaticCheck, as in Silica.
Item {
    id: toggle
    property string text
    property string description
    property bool checked: false
    property bool automaticCheck: true
    property bool busy: false
    property bool down: false
    signal clicked()
    width: parent ? parent.width : 540
    height: 100

    // What a tap does, for tests.
    function click() {
        if (!toggle.enabled) {
            return
        }
        if (toggle.automaticCheck) {
            toggle.checked = !toggle.checked
        }
        toggle.clicked()
    }

    Text { text: toggle.text }
    Text { y: 50; text: toggle.description }
}
