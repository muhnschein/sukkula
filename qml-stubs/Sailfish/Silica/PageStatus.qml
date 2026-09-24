import QtQuick 2.6

// Silica's PageStatus is a C++ enum; QML property names cannot start with
// a capital, so the stub is a QML enum (Qt 5.10+, host only). Same values
// as Silica's.
QtObject {
    enum Value {
        Inactive = 0,
        Activating = 1,
        Active = 2,
        Deactivating = 3
    }
}
