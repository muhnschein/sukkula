import QtQuick 2.6
import Sailfish.Silica 1.0

Item {
    property var pageStack: null
    property int status: PageStatus.Inactive
    property int allowedOrientations: 0
    property int orientation: 0
    property bool backNavigation: true
    property bool forwardNavigation: true
    readonly property bool isPortrait: true
    readonly property bool isLandscape: false
}
