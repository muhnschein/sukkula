import QtQuick 2.6

Flickable {
    property Item pullDownMenu
    property Item pushUpMenu
    function scrollToTop() { contentY = 0 }
}
