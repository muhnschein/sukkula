pragma Singleton
import QtQuick 2.6

// Silica's Theme: the values the app reads. A missing one would bind to
// undefined silently, so every one used is declared.
QtObject {
    property real paddingSmall: 6
    property real paddingMedium: 12
    property real paddingLarge: 24
    property real horizontalPageMargin: 24
    property real fontSizeTiny: 16
    property real fontSizeExtraSmall: 20
    property real fontSizeSmall: 24
    property real fontSizeMedium: 30
    property real fontSizeLarge: 40
    property real fontSizeExtraLarge: 50
    property real fontSizeHuge: 72
    property real itemSizeExtraSmall: 70
    property real itemSizeSmall: 80
    property real itemSizeMedium: 100
    property real itemSizeLarge: 120
    property real itemSizeExtraLarge: 160
    property real iconSizeSmall: 32
    property real iconSizeMedium: 64
    property real iconSizeLarge: 96
    property color primaryColor: "#ffffff"
    property color secondaryColor: "#b0ffffff"
    property color highlightColor: "#80c0ff"
    property color secondaryHighlightColor: "#6090c0"
    property color highlightBackgroundColor: "#4080c0"
    property color errorColor: "#ff4040"
    property string fontFamily: "Sans"
    property string fontFamilyHeading: "Sans"

    function rgba(colour, alpha) {
        return Qt.rgba(colour.r, colour.g, colour.b, alpha)
    }
}
