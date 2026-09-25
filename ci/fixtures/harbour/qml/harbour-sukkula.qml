import QtQuick 2.6
import Sailfish.Silica 1.0
import "pages"
import "components"

ApplicationWindow {
    initialPage: Component { MainPage { } }
    cover: Qt.resolvedUrl("cover/CoverPage.qml")
    allowedOrientations: defaultAllowedOrientations
}
