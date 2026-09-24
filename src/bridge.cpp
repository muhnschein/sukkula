// SPDX-License-Identifier: GPL-3.0-or-later

#include "bridge.h"

#include <QCoreApplication>
#include <QDir>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMetaObject>
#include <QStandardPaths>

#include <sukkula.h>

Bridge::Bridge(QObject *parent)
    : QObject(parent)
{
}

Bridge::~Bridge()
{
    stop();
}

QString Bridge::version() const
{
    const char *v = sukkula_version();
    return v ? QString::fromUtf8(v) : QString();
}

QByteArray Bridge::startConfig(const QString &dataDir, const QString &downloadDir)
{
    // Both must be absolute: an empty location (no $HOME) would otherwise
    // turn "/Sukkula" into a directory at the file system root.
    if (dataDir.isEmpty() || downloadDir.isEmpty() || !QDir::isAbsolutePath(dataDir)
        || !QDir::isAbsolutePath(downloadDir)) {
        return QByteArray();
    }
    QJsonObject config;
    config.insert(QStringLiteral("v"), 1);
    config.insert(QStringLiteral("data_dir"), QDir::cleanPath(dataDir));
    config.insert(QStringLiteral("download_dir"), QDir::cleanPath(downloadDir));
    // No device_model: the engine reads /etc/hw-release itself (F-C7), and
    // never allow_loopback, which is for the engine's own tests.
    return QJsonDocument(config).toJson(QJsonDocument::Compact);
}

bool Bridge::start()
{
    if (m_engine) {
        return true;
    }
    // Sailjail grants ~/.local/share/<OrganizationName>/<ApplicationName>,
    // which is what AppDataLocation is once main() has set both names; and
    // Downloads, where received files go (spec §2, S3).
    const QString data = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    const QString downloads = QStandardPaths::writableLocation(QStandardPaths::DownloadLocation);
    const QByteArray config
        = startConfig(data, downloads.isEmpty() ? QString() : downloads + QStringLiteral("/Sukkula"));
    if (config.isEmpty()) {
        // The engine was never asked, so it emitted nothing: say why the
        // way it would have, so the UI has one path for a failed start.
        QMetaObject::invokeMethod(
            this, "deliver", Qt::QueuedConnection,
            Q_ARG(QString,
                  QStringLiteral("{\"type\":\"fatal\",\"error\":{\"code\":\"storage\","
                                 "\"message\":\"no usable home directory\"}}")));
        return false;
    }
    m_engine = sukkula_start(config.constData(), &Bridge::onEvent, this);
    return m_engine != nullptr;
}

int Bridge::command(const QString &json)
{
    if (!m_engine) {
        return SUKKULA_ERR_NULL;
    }
    const QByteArray utf8 = json.toUtf8();
    // A C string ends at the first NUL, so an embedded one would hand the
    // engine a different command from the one QML built. JSON.stringify
    // never produces one; refuse rather than truncate.
    if (utf8.contains('\0')) {
        return SUKKULA_ERR_UTF8;
    }
    return sukkula_command(m_engine, utf8.constData());
}

void Bridge::stop()
{
    SukkulaEngine *engine = m_engine;
    m_engine = nullptr;
    // Blocks until every engine thread is done; after this the callback
    // is never called again and `this` may go.
    sukkula_stop(engine);
    // Events the engine posted before it stopped have not been delivered
    // yet; they would describe an engine that no longer exists.
    if (engine) {
        QCoreApplication::removePostedEvents(this, QEvent::MetaCall);
    }
}

void Bridge::deliver(const QString &json)
{
    emit event(json);
}

void Bridge::onEvent(const char *json, void *userdata)
{
    if (!json || !userdata) {
        return;
    }
    // The string is valid only during this call: copy it now, bounded.
    const uint length = qstrnlen(json, MaxEventBytes + 1);
    if (length > uint(MaxEventBytes)) {
        return;
    }
    const QString copy = QString::fromUtf8(json, int(length));
    auto *self = static_cast<Bridge *>(userdata);
    // Queued, always: this is an engine thread, and QML may only be
    // touched from the GUI thread. The posted event belongs to `self`, so
    // if `self` is destroyed before it runs, Qt discards it -- and `self`
    // cannot be destroyed while this call is running, because ~Bridge()
    // waits in sukkula_stop() for it to return.
    QMetaObject::invokeMethod(self, "deliver", Qt::QueuedConnection, Q_ARG(QString, copy));
}
