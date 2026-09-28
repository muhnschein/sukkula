import QtQuick 2.6
import Sailfish.Silica 1.0

// Silica's MultiContentPickerDialog: several pictures, videos, songs or
// documents from the media index, in one dialog. A test fills
// `selectedContent` (each row {url, filePath?, fileSize?}) and calls accept(),
// which is what ticking files and pulling "Accept" does.
Dialog {
    property string title: ""
    property var nameFilters: []
    property bool showSystemFiles: false
    property ListModel selectedContent: ListModel {}
}
