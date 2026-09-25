pragma Singleton
import QtQuick 2.6

// Silica's StandardPaths names the user's folders; the app reads
// `download`. The tests' files live under this one.
QtObject {
    property string home: "/home/defaultuser"
    property string download: "/home/defaultuser/Downloads"
    property string documents: "/home/defaultuser/Documents"
    property string pictures: "/home/defaultuser/Pictures"
}
