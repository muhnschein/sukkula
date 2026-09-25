import QtQuick 2.6; import Sailfish.Silica 1.0

ListItem {
    property string sender
    Label { text: sender; textFormat: Text.PlainText }
}
