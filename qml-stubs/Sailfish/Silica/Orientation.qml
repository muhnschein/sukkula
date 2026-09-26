import QtQuick 2.6

// Silica's Orientation flags, as a QML enum (Qt 5.10+, host only; see
// PageStatus.qml). Same values as Silica's.
QtObject {
    enum Value {
        None = 0,
        Portrait = 1,
        Landscape = 2,
        PortraitInverted = 4,
        PortraitMask = 5,
        LandscapeInverted = 8,
        LandscapeMask = 10,
        All = 15
    }
}
