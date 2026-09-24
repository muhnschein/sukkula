import QtQuick 2.6

Item {
    property bool busy: false
    property bool active: false
    default property alias content: holder.data
    width: parent ? parent.width : 540

    Column {
        id: holder
        width: parent.width
    }
}
