// SPDX-License-Identifier: GPL-3.0-or-later
//
// The one bridge between QML and the Rust engine (spec §3).
//
// It moves JSON strings across the C ABI in crates/sukkula-ffi/include/
// sukkula.h and does nothing else: no parsing, no state. Everything the
// engine says arrives as `event(json)` on the GUI thread; everything QML
// asks for goes out through `command(json)`. qml/engine/Engine.qml is the
// only caller.

#ifndef SUKKULA_BRIDGE_H
#define SUKKULA_BRIDGE_H

#include <QByteArray>
#include <QMutex>
#include <QObject>
#include <QString>
#include <QWaitCondition>

struct SukkulaEngine;

class Bridge : public QObject
{
    Q_OBJECT
    // The engine's version, for the About page. A static string in the
    // library, so it is known before start() and never changes.
    Q_PROPERTY(QString version READ version CONSTANT)

public:
    // Longest event passed on to QML. The engine's own events are bounded
    // well below this (256 KiB, docs/FFI.md), so anything longer is a
    // fault, and dropping it beats handing QML a string it would spend a
    // second parsing.
    static const int MaxEventBytes = 1024 * 1024;
    // Most events posted to the GUI thread and not yet delivered. At the
    // bound the engine's delivery thread waits, which makes the engine's
    // own bounded queue (1024 events) the one that fills: memory stays
    // bounded however fast events come and however busy QML is.
    static const int MaxUndelivered = 256;

    explicit Bridge(QObject *parent = nullptr);
    // Stops the engine first: after sukkula_stop() returns the callback
    // never runs again, so no event can reach a half-destroyed object.
    ~Bridge() override;

    QString version() const;

    // Starts the engine once; true if it is running. On failure the engine
    // has already emitted one "fatal" event, which arrives like any other.
    Q_INVOKABLE bool start();
    // Hands the engine one command. Returns the sukkula_command() code:
    // 0 when taken (its "reply" follows as an event), negative otherwise.
    Q_INVOKABLE int command(const QString &json);
    // Stops the engine; idempotent. Blocks for a few seconds at most.
    // No event is emitted after it returns, not even one already posted.
    Q_INVOKABLE void stop();

    // The StartConfig JSON for the given directories, or an empty array
    // when either is unusable. Public for the host tests.
    static QByteArray startConfig(const QString &dataDir, const QString &downloadDir);

    // Events posted and not yet delivered. For the host tests.
    int undelivered() const;

    // QObject::event(QEvent *) is a virtual of the same name as the signal
    // below; bringing it into scope keeps it callable and keeps
    // -Woverloaded-virtual quiet.
    using QObject::event;

signals:
    // One engine event, as the JSON the engine produced.
    void event(const QString &json);

private slots:
    // Runs on the GUI thread, posted there by onEvent(). Private so QML
    // cannot fake an event through it.
    void deliver(const QString &json);

private:
    // The C callback. Runs on the engine's delivery thread: copies, waits
    // while MaxUndelivered events are queued, posts, returns.
    static void onEvent(const char *json, void *userdata);

    SukkulaEngine *m_engine = nullptr;

    // Guards the two below, between the delivery thread and the GUI thread.
    mutable QMutex m_lock;
    QWaitCondition m_drained;
    int m_undelivered = 0;
    // Set by stop() before sukkula_stop(), which waits for a callback in
    // progress: a callback waiting for room gives up at once instead.
    bool m_stopping = false;
};

#endif // SUKKULA_BRIDGE_H
