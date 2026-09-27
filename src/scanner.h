// SPDX-License-Identifier: GPL-3.0-or-later
//
// Reads a code off the camera (spec v0.6): grabs the viewfinder, makes it
// grey and small, and hands it to sukkula_scan_qr() (crates/sukkula-ffi/
// include/sukkula.h) on a thread of its own. What the engine reads comes
// back as `found(json)` on the GUI thread, the JSON exactly as the engine
// wrote it; qml/pages/ScanPage.qml is the only caller, and Engine.qml the
// only reader of the JSON.
//
// Like the bridge it holds no state of the app's and parses nothing: one
// frame at a time, and whatever the frame says goes to QML untouched.

#ifndef SUKKULA_SCANNER_H
#define SUKKULA_SCANNER_H

#include <QImage>
#include <QObject>
#include <QSharedPointer>
#include <QString>
#include <QThreadPool>
#include <QTimer>

class QQuickItem;
class QQuickItemGrabResult;

class Scanner : public QObject
{
    Q_OBJECT
    // A frame is being grabbed or scanned; scan() refuses another meanwhile.
    Q_PROPERTY(bool busy READ busy NOTIFY busyChanged)

public:
    // Longest side of a frame handed to the engine, in pixels. A code that
    // fills a third of the viewfinder is still a few pixels a module, and
    // the engine takes 1024 at most (sukkula.h).
    static const int MaxSide = 640;

    explicit Scanner(QObject *parent = nullptr);
    // Waits for the frame in hand: the engine bounds a scan, so this is
    // short, and nothing is delivered to a scanner that is gone.
    ~Scanner() override;

    bool busy() const;

    // Grabs what `item` shows and scans it. False, and nothing happens,
    // when a frame is still in hand or the item cannot be grabbed (not in
    // a window, or not shown).
    Q_INVOKABLE bool scan(QQuickItem *item);

    // Scans `image`: what scan() does once the grab is ready. False when a
    // frame is still in hand. Public for the host tests.
    bool scanImage(const QImage &image);

    // `image` as the engine gets it: grey, a byte a pixel, and at most
    // MaxSide on its longer side. A null image for an empty one. Public
    // for the host tests.
    static QImage prepare(const QImage &image);

signals:
    // A QR code was read: the engine's JSON, e.g.
    // {"found":"croc","code":"gala-tulip-acorn"} (docs/FFI.md).
    void found(const QString &json);
    // A frame was scanned and nothing in it was read, or it could not be.
    void missed();
    void busyChanged();

private slots:
    // Runs on the GUI thread, posted there by the worker.
    void deliver(const QString &json);

private:
    void onGrabbed();
    // Hands `frame` to the worker, or answers at once for an empty one.
    void submit(const QImage &frame);
    void setBusy(bool busy);

    // One thread, one frame: a slow frame delays the next, never stacks.
    QThreadPool m_pool;
    QSharedPointer<QQuickItemGrabResult> m_grab;
    // A grab whose item went away never finishes; this gives up on it.
    QTimer m_grabTimeout;
    bool m_busy = false;
};

#endif // SUKKULA_SCANNER_H
