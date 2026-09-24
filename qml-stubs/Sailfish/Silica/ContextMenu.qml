import QtQuick 2.6

Item {
    property bool active: false
    default property alias content: holder.data
    width: parent ? parent.width : 540
    height: holder.height

    Column {
        id: holder
        width: parent.width
    }

    function open(item) { active = true }
    function close() { active = false }
}
