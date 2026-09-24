// SPDX-License-Identifier: GPL-3.0-or-later
//
// The Qt bridge's half of the C ABI contract (sukkula.h): events arrive on
// the GUI thread, in order, copied; commands go out with the engine's
// return codes; nothing is delivered after stop(), and destroying the
// bridge while the engine is mid-burst is safe (run under ASan with
// CONFIG+=sukkula_sanitize).
//
// With the stub engine every test runs; with the real one
// (SUKKULA_ENGINE=rust) the stub's special commands are skipped.

#include <functional>

#include <QCoreApplication>
#include <QElapsedTimer>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSignalSpy>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QThread>
#include <QtTest>

#include <sukkula.h>

#include "bridge.h"

#ifdef SUKKULA_STUB_ENGINE
#include "sukkula_stub.h"
#endif

namespace {

// Collects events, and checks each one arrives on the GUI thread.
class Sink : public QObject
{
    Q_OBJECT
public:
    QStringList events;
    int offThread = 0;

public slots:
    void take(const QString &json)
    {
        if (QThread::currentThread() != qApp->thread()) {
            offThread++;
        }
        events << json;
    }
};

// The signal, not QObject::event(QEvent *), which shares its name.
const auto eventSignal = static_cast<void (Bridge::*)(const QString &)>(&Bridge::event);

QString typeOf(const QString &json)
{
    return QJsonDocument::fromJson(json.toUtf8()).object().value(QStringLiteral("type")).toString();
}

bool waitFor(const std::function<bool()> &done, int ms = 5000)
{
    QElapsedTimer timer;
    timer.start();
    while (!done() && timer.elapsed() < ms) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 20);
        QThread::msleep(1);
    }
    return done();
}

} // namespace

class TestBridge : public QObject
{
    Q_OBJECT

private slots:
    void initTestCase()
    {
        // The directories the bridge builds come from these, as on the
        // phone; test mode keeps them out of the real home.
        QStandardPaths::setTestModeEnabled(true);
        QCoreApplication::setOrganizationName(QStringLiteral("sukkula"));
        QCoreApplication::setApplicationName(QStringLiteral("sukkula"));
    }

    void versionComesFromTheLibrary()
    {
        Bridge bridge;
        QCOMPARE(bridge.version(), QString::fromUtf8(sukkula_version()));
        QVERIFY(!bridge.version().isEmpty());
    }

    void commandsBeforeStartAreRefused()
    {
        Bridge bridge;
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":1,\"cmd\":{\"type\":\"get_settings\"}}")),
                 int(SUKKULA_ERR_NULL));
        bridge.stop(); // Idempotent before start, too.
    }

    void startConfigIsAbsoluteAndMinimal()
    {
        QVERIFY(Bridge::startConfig(QString(), QStringLiteral("/dl")).isEmpty());
        QVERIFY(Bridge::startConfig(QStringLiteral("/d"), QString()).isEmpty());
        QVERIFY(Bridge::startConfig(QStringLiteral("relative"), QStringLiteral("/dl")).isEmpty());
        QVERIFY(Bridge::startConfig(QStringLiteral("/d"), QStringLiteral("dl")).isEmpty());

        const QByteArray json = Bridge::startConfig(QStringLiteral("/home/u/.local/share/sukkula/sukkula/"),
                                                    QStringLiteral("/home/u/Downloads/Sukkula"));
        const QJsonObject o = QJsonDocument::fromJson(json).object();
        QCOMPARE(o.keys(), (QStringList{ QStringLiteral("data_dir"), QStringLiteral("download_dir"),
                                         QStringLiteral("v") }));
        QCOMPARE(o.value(QStringLiteral("v")).toInt(), 1);
        QCOMPARE(o.value(QStringLiteral("data_dir")).toString(),
                 QStringLiteral("/home/u/.local/share/sukkula/sukkula"));
        QCOMPARE(o.value(QStringLiteral("download_dir")).toString(),
                 QStringLiteral("/home/u/Downloads/Sukkula"));
        // Never the tests-only switch.
        QVERIFY(!json.contains("allow_loopback"));
    }

    void startDeliversTheFirstEventsOnTheGuiThread()
    {
        Bridge bridge;
        Sink sink;
        connect(&bridge, eventSignal, &sink, &Sink::take);
        QVERIFY(bridge.start());
        QVERIFY(bridge.start()); // Once only; the second is a no-op.
        QVERIFY(waitFor([&] { return sink.events.size() >= 3; }));
        QCOMPARE(typeOf(sink.events.value(0)), QStringLiteral("started"));
        QCOMPARE(typeOf(sink.events.value(1)), QStringLiteral("settings"));
        QCOMPARE(typeOf(sink.events.value(2)), QStringLiteral("receiving"));
        QCOMPARE(sink.offThread, 0);
#ifdef SUKKULA_STUB_ENGINE
        const QJsonObject config = QJsonDocument::fromJson(sukkula_stub_last_config()).object();
        QVERIFY(config.value(QStringLiteral("data_dir")).toString().endsWith(QStringLiteral("/sukkula/sukkula")));
        QVERIFY(config.value(QStringLiteral("download_dir")).toString().endsWith(QStringLiteral("/Sukkula")));
#endif
        bridge.stop();
    }

    void commandsAreAnsweredWithTheirId()
    {
        Bridge bridge;
        Sink sink;
        connect(&bridge, eventSignal, &sink, &Sink::take);
        QVERIFY(bridge.start());
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":42,\"cmd\":{\"type\":\"get_settings\"}}")),
                 int(SUKKULA_OK));
        QVERIFY(waitFor([&] {
            for (const QString &e : sink.events) {
                const QJsonObject o = QJsonDocument::fromJson(e.toUtf8()).object();
                if (o.value(QStringLiteral("type")).toString() == QLatin1String("reply")
                    && o.value(QStringLiteral("id")).toInt() == 42) {
                    return true;
                }
            }
            return false;
        }));
        QCOMPARE(sink.offThread, 0);
        bridge.stop();
    }

    void anEmbeddedNulIsRefusedNotTruncated()
    {
        Bridge bridge;
        QVERIFY(bridge.start());
        QString json = QStringLiteral("{\"v\":1,\"id\":7,\"cmd\":{\"type\":\"get_settings\"}}");
        json.insert(10, QChar(0));
        QCOMPARE(bridge.command(json), int(SUKKULA_ERR_UTF8));
        bridge.stop();
    }

    void anOversizedCommandIsTheEnginesToRefuse()
    {
        Bridge bridge;
        QVERIFY(bridge.start());
        const QString code(70 * 1024, QLatin1Char('a'));
        const QString json = QStringLiteral("{\"v\":1,\"id\":8,\"cmd\":{\"type\":\"receive_wormhole\",\"code\":\"")
                             + code + QStringLiteral("\"}}");
        QCOMPARE(bridge.command(json), int(SUKKULA_ERR_TOO_LONG));
        bridge.stop();
    }

    void aBusyEngineIsReportedAsSuch()
    {
#ifdef SUKKULA_STUB_ENGINE
        Bridge bridge;
        QVERIFY(bridge.start());
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":3,\"stub\":\"busy\"}")), int(SUKKULA_ERR_BUSY));
        bridge.stop();
#else
        QSKIP("needs the stub engine");
#endif
    }

    void nothingIsDeliveredAfterStop()
    {
        Bridge bridge;
        Sink sink;
        connect(&bridge, eventSignal, &sink, &Sink::take);
        QVERIFY(bridge.start());
        QVERIFY(waitFor([&] { return sink.events.size() >= 3; }));
#ifdef SUKKULA_STUB_ENGINE
        // Thousands of events in flight, most not yet delivered.
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":9,\"stub\":\"burst\"}")), int(SUKKULA_OK));
        QThread::msleep(5);
#endif
        bridge.stop();
        const int seen = sink.events.size();
        QCoreApplication::processEvents();
        QThread::msleep(50);
        QCoreApplication::processEvents();
        QCOMPARE(sink.events.size(), seen);
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":10,\"cmd\":{\"type\":\"get_settings\"}}")),
                 int(SUKKULA_ERR_NULL));
        bridge.stop();
    }

    void aFailedStartReportsWhy()
    {
#ifdef SUKKULA_STUB_ENGINE
        Bridge bridge;
        Sink sink;
        connect(&bridge, eventSignal, &sink, &Sink::take);
        sukkula_stub_fail_next_start(1);
        QVERIFY(!bridge.start());
        QVERIFY(waitFor([&] { return !sink.events.isEmpty(); }));
        QCOMPARE(typeOf(sink.events.value(0)), QStringLiteral("fatal"));
        QCOMPARE(sink.offThread, 0);
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":1,\"cmd\":{\"type\":\"get_settings\"}}")),
                 int(SUKKULA_ERR_NULL));
#else
        QSKIP("needs the stub engine");
#endif
    }

    void oversizedEventsAreDropped()
    {
#ifdef SUKKULA_STUB_ENGINE
        Bridge bridge;
        Sink sink;
        connect(&bridge, eventSignal, &sink, &Sink::take);
        QVERIFY(bridge.start());
        QVERIFY(waitFor([&] { return sink.events.size() >= 3; }));
        sink.events.clear();
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":11,\"stub\":\"oversize\"}")), int(SUKKULA_OK));
        // The reply follows the 2 MiB event; the event itself never shows.
        QVERIFY(waitFor([&] { return !sink.events.isEmpty(); }));
        QCOMPARE(sink.events.size(), 1);
        QCOMPARE(typeOf(sink.events.value(0)), QStringLiteral("reply"));
        bridge.stop();
#else
        QSKIP("needs the stub engine");
#endif
    }

    void aBurstArrivesWholeInOrderAndBounded()
    {
#ifdef SUKKULA_STUB_ENGINE
        Bridge bridge;
        Sink sink;
        connect(&bridge, eventSignal, &sink, &Sink::take);
        QVERIFY(bridge.start());
        QVERIFY(waitFor([&] { return sink.events.size() >= 3; }));
        sink.events.clear();
        QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":21,\"stub\":\"burst\"}")), int(SUKKULA_OK));
        // The GUI thread looks away for a while: the engine thread must
        // wait at the bound rather than queue 5000 events.
        QThread::msleep(200);
        QVERIFY(bridge.undelivered() <= Bridge::MaxUndelivered);
        QVERIFY(bridge.undelivered() >= Bridge::MaxUndelivered / 2);
        int most = 0;
        QVERIFY(waitFor([&] {
            most = qMax(most, bridge.undelivered());
            return sink.events.size() == 5001;
        }, 20000));
        QVERIFY(most <= Bridge::MaxUndelivered);
        for (int i = 0; i < 5000; i++) {
            const QJsonObject o = QJsonDocument::fromJson(sink.events[i].toUtf8()).object();
            if (o.value(QStringLiteral("bytes")).toInt() != i) {
                QFAIL(qPrintable(QStringLiteral("event %1 out of order: %2").arg(i).arg(sink.events[i])));
            }
        }
        QCOMPARE(typeOf(sink.events.last()), QStringLiteral("reply"));
        QCOMPARE(bridge.undelivered(), 0);
        QCOMPARE(sink.offThread, 0);
        bridge.stop();
#else
        QSKIP("needs the stub engine");
#endif
    }

    void destroyingTheBridgeMidBurstIsSafe()
    {
#ifdef SUKKULA_STUB_ENGINE
        for (int round = 0; round < 20; round++) {
            auto *bridge = new Bridge;
            Sink sink;
            connect(bridge, eventSignal, &sink, &Sink::take);
            QVERIFY(bridge->start());
            QCOMPARE(bridge->command(QStringLiteral("{\"v\":1,\"id\":1,\"stub\":\"burst\"}")), int(SUKKULA_OK));
            QThread::usleep(unsigned(round) * 100);
            if (round % 2) {
                QCoreApplication::processEvents();
            }
            delete bridge; // Stops the engine first; pending events go with it.
            QCoreApplication::processEvents();
            QCOMPARE(sink.offThread, 0);
        }
#else
        QSKIP("needs the stub engine");
#endif
    }

    void aSlowReplyAfterDestructionGoesNowhere()
    {
#ifdef SUKKULA_STUB_ENGINE
        Sink sink;
        {
            Bridge bridge;
            connect(&bridge, eventSignal, &sink, &Sink::take);
            QVERIFY(bridge.start());
            QVERIFY(waitFor([&] { return sink.events.size() >= 3; }));
            QCOMPARE(bridge.command(QStringLiteral("{\"v\":1,\"id\":12,\"stub\":\"slow\"}")), int(SUKKULA_OK));
        }
        const int seen = sink.events.size();
        QThread::msleep(300);
        QCoreApplication::processEvents();
        QCOMPARE(sink.events.size(), seen);
#else
        QSKIP("needs the stub engine");
#endif
    }
};

QTEST_GUILESS_MAIN(TestBridge)
#include "tst_bridge.moc"
