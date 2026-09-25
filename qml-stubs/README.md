# QML stubs for host tests

Minimal stand-ins for the Sailfish OS modules the app imports
(`Sailfish.Silica`, `Sailfish.Share`, `Sailfish.Pickers`, `Nemo.KeepAlive`,
`Nemo.Notifications`), so the app's QML can be loaded and exercised with a
desktop Qt 5.15 under `QT_QPA_PLATFORM=offscreen` (see `tests/README.md`).

They are **never shipped** (the RPM installs `qml/` only) and never loaded
on a device. They declare the surface Sukkula uses, not all of Silica: if
the app starts using a property that is missing here, add it here -- do not
work around it in the app.

Two things they do on purpose:

- Every Silica component that draws a string it was given (`PageHeader`,
  `SectionHeader`, `TextSwitch`, `Button`, `MenuItem`, `ComboBox`,
  `DialogHeader`, `TextField`, `TextArea`) draws it with a plain `Text` left
  at Qt's default `AutoText`, as Silica may. The QML tests feed hostile
  peer strings marked `EVIL` and fail if any of them reaches a text item
  that is not `Text.PlainText` -- which is what catches a peer name handed
  to a Silica property instead of to one of the app's own labels (S2).
- `ApplicationWindow` has a working page stack (`PageStack.qml`) that
  really creates, activates, deactivates and destroys pages, so that the
  consent dialog, the Share menu flow and page status handlers run.

These stubs use QML `enum`s, which need Qt 5.10: fine on the host, and the
reason they must never be installed. The app's own QML stays within Qt 5.6.
