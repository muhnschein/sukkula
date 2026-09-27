import QtQuick 2.6

// QtMultimedia's Camera, as far as ScanPage.qml uses it, with the same
// values as Qt's. There is no camera: it is "active" once asked to be, if
// a test has not made it unavailable.
QtObject {
    id: camera

    enum CaptureModes { CaptureViewfinder = 0, CaptureStillImage = 1, CaptureVideo = 2 }
    enum States { UnloadedState = 0, LoadedState = 1, ActiveState = 2 }
    enum Statuses {
        UnavailableStatus = 0, UnloadedStatus = 1, LoadingStatus = 2, UnloadingStatus = 3,
        LoadedStatus = 4, StandbyStatus = 5, StartingStatus = 6, StoppingStatus = 7,
        ActiveStatus = 8
    }
    enum Availabilities { Available = 0, Unavailable = 1, Busy = 2, ResourceMissing = 3 }
    enum FocusModes {
        FocusManual = 1, FocusHyperfocal = 2, FocusInfinity = 4, FocusAuto = 8,
        FocusContinuous = 16, FocusMacro = 32
    }
    enum FlashModes { FlashAuto = 1, FlashOff = 2, FlashOn = 4 }

    property int captureMode: 0
    property int cameraState: 2
    /// A test sets this to play a camera another app holds, or none.
    property int availability: 0
    readonly property int cameraStatus: camera.availability !== 0 ? 0 : camera.cameraState === 2 ? 8 : 1

    property CameraFocus focus: CameraFocus {}
    property CameraFlash flash: CameraFlash {}
}
