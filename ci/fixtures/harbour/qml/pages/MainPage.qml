import QtQuick 2.6
import Sailfish.Silica 1.0
import Nemo.KeepAlive 1.2
import Nemo.Notifications 1.0
import "../components"

Page {
    KeepAlive { enabled: false }
    Notification { id: done }
    SilicaFlickable {
        anchors.fill: parent
        PageHeader { title: qsTr("Sukkula") }
    }
}
