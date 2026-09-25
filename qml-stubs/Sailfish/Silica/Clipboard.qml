pragma Singleton
import QtQuick 2.6

// Silica's Clipboard: a C++ singleton on the phone. Tests read `text`.
QtObject {
    property string text: ""
    readonly property bool hasText: text.length > 0
}
