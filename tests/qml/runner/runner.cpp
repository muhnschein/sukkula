// SPDX-License-Identifier: GPL-3.0-or-later
//
// Runs one QML test file (tests/qml/tst_*.qml) against the app's QML and
// the stubs in qml-stubs/, headless:
//
//   qml_runner --app <repo>/qml --stubs <repo>/qml-stubs tests/qml/tst_x.qml
//
// The test's root is a Script (tests/qml/helpers/Script.qml): steps run
// one after another with the event loop turning in between, and the root
// says `done` and lists `failures`. The runner adds three checks of its
// own, which no test can forget:
//
//  1. Every Qt warning fails the test. Qt reports a binding that did not
//     bind, an unknown property or a type error as a warning and carries
//     on, and most of them are real faults. (The one exception is the Qt
//     5.15 deprecation notice for the Qt 5.6 handler syntax the app must
//     use.)
//  2. Every text item created by a file under qml/ must be
//     `Text.PlainText` (S2), and every text item anywhere -- the stubs'
//     Silica-owned ones included -- that shows a string marked EVIL, which
//     is how the tests spell peer-supplied data, must be too. The second
//     rule is what catches a peer name handed to a Silica property instead
//     of to one of the app's own labels. Checked at the end, and whenever
//     a test calls probe.plainTextViolations().
//  3. The test must finish within its time.
//
// `bridge` in the root context is a FakeBridge: the Bridge's interface,
// recording commands and replying to them, plus emitEvent() for tests.

#include <cstdio>

#include <QDir>
#include <QElapsedTimer>
#include <QFileInfo>
#include <QGuiApplication>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPixmap>
#include <QPointer>
#include <QQmlComponent>
#include <QQmlContext>
#include <QQmlEngine>
#include <QQuickImageProvider>
#include <QQuickItem>
#include <QSet>
#include <QTimer>
#include <QTranslator>

namespace {

QStringList g_messages;

void onMessage(QtMsgType type, const QMessageLogContext &context, const QString &message)
{
    if (message.contains(QLatin1String("Implicitly defined onFoo properties in Connections are deprecated"))) {
        return;
    }
    if (type == QtDebugMsg || type == QtInfoMsg) {
        std::fprintf(stderr, "  debug: %s\n", qPrintable(message));
        return;
    }
    QString where;
    if (context.file) {
        where = QStringLiteral(" (%1:%2)").arg(QString::fromUtf8(context.file)).arg(context.line);
    }
    g_messages << message + where;
}

class ThemeImages : public QQuickImageProvider
{
public:
    ThemeImages()
        : QQuickImageProvider(QQuickImageProvider::Pixmap)
    {
    }
    QPixmap requestPixmap(const QString &, QSize *size, const QSize &) override
    {
        QPixmap pixmap(8, 8);
        pixmap.fill(Qt::transparent);
        if (size) {
            *size = pixmap.size();
        }
        return pixmap;
    }
};

// Everything below `root`: QObject children and visual children both,
// since a Repeater's delegates are only the latter.
void walk(QObject *object, QSet<QObject *> &seen, QList<QObject *> &out)
{
    if (!object || seen.contains(object)) {
        return;
    }
    seen.insert(object);
    out << object;
    for (QObject *child : object->children()) {
        walk(child, seen, out);
    }
    if (auto *item = qobject_cast<QQuickItem *>(object)) {
        for (QQuickItem *child : item->childItems()) {
            walk(child, seen, out);
        }
    }
}

QList<QObject *> everything(QObject *root)
{
    QSet<QObject *> seen;
    QList<QObject *> out;
    walk(root, seen, out);
    return out;
}

bool isText(QObject *object)
{
    return object->inherits("QQuickText") || object->inherits("QQuickTextEdit");
}

} // namespace

// The C++ Bridge's interface, for tests.
class FakeBridge : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QString version READ version CONSTANT)
    Q_PROPERTY(QStringList commands READ commands NOTIFY commandsChanged)
    Q_PROPERTY(bool autoReply MEMBER m_autoReply)
    Q_PROPERTY(bool startSucceeds MEMBER m_startSucceeds)
    Q_PROPERTY(int commandResult MEMBER m_commandResult)
    Q_PROPERTY(int startCount MEMBER m_startCount NOTIFY startCountChanged)
    Q_PROPERTY(int stopCount MEMBER m_stopCount)
    Q_PROPERTY(double nextTransfer MEMBER m_nextTransfer)

public:
    using QObject::event;

    QString version() const { return QStringLiteral("9.9.9-fake"); }
    QStringList commands() const { return m_commands; }

    Q_INVOKABLE bool start()
    {
        m_startCount++;
        emit startCountChanged();
        if (!m_startSucceeds) {
            emitEvent(QStringLiteral("{\"type\":\"fatal\",\"error\":{\"code\":\"storage\",\"message\":\"no space\"}}"));
            return false;
        }
        emitEvent(QStringLiteral("{\"type\":\"started\",\"version\":\"9.9.9-fake\",\"api\":1,"
                                 "\"protocols\":[\"local_send\",\"quick_share\",\"wormhole\",\"bluetooth\"]}"));
        emitEvent(QStringLiteral("{\"type\":\"settings\",\"settings\":{\"device_name\":\"\","
                                 "\"localsend\":{\"enabled\":true,\"pin\":null},"
                                 "\"quickshare\":{\"enabled\":true,\"visibility\":\"everyone\",\"ble_nudge\":true},"
                                 "\"wormhole\":{\"enabled\":true,\"mailbox_url\":null,\"relay_url\":null},"
                                 "\"bluetooth\":{\"enabled\":true},\"logging\":false},"
                                 "\"effective_device_name\":\"Jolla Phone\"}"));
        emitEvent(QStringLiteral("{\"type\":\"receiving\",\"on\":false,\"protocols\":["
                                 "{\"protocol\":\"local_send\",\"state\":\"off\"},"
                                 "{\"protocol\":\"quick_share\",\"state\":\"off\"},"
                                 "{\"protocol\":\"wormhole\",\"state\":\"send_only\"},"
                                 "{\"protocol\":\"bluetooth\",\"state\":\"send_only\"}]}"));
        return true;
    }

    Q_INVOKABLE int command(const QString &json)
    {
        m_commands << json;
        emit commandsChanged();
        if (m_commandResult != 0) {
            return m_commandResult;
        }
        const QJsonObject o = QJsonDocument::fromJson(json.toUtf8()).object();
        if (m_autoReply) {
            QJsonObject reply;
            reply.insert(QStringLiteral("type"), QStringLiteral("reply"));
            reply.insert(QStringLiteral("id"), o.value(QStringLiteral("id")));
            reply.insert(QStringLiteral("ok"), true);
            const QString type = o.value(QStringLiteral("cmd")).toObject().value(QStringLiteral("type")).toString();
            if (type == QLatin1String("send") || type == QLatin1String("receive_wormhole")) {
                reply.insert(QStringLiteral("transfer"), m_nextTransfer);
                m_nextTransfer += 1;
            }
            emitEvent(QString::fromUtf8(QJsonDocument(reply).toJson(QJsonDocument::Compact)));
        }
        return 0;
    }

    Q_INVOKABLE void stop() { m_stopCount++; }

    // Delivered later, from the event loop, as the real bridge does.
    Q_INVOKABLE void emitEvent(const QString &json)
    {
        QMetaObject::invokeMethod(this, "deliver", Qt::QueuedConnection, Q_ARG(QString, json));
    }

    // The last command as an object, or null.
    Q_INVOKABLE QVariant lastCommand() const
    {
        if (m_commands.isEmpty()) {
            return QVariant();
        }
        return QJsonDocument::fromJson(m_commands.last().toUtf8()).toVariant();
    }

    // Every command, parsed.
    Q_INVOKABLE QVariantList parsedCommands() const
    {
        QVariantList out;
        for (const QString &c : m_commands) {
            out << QJsonDocument::fromJson(c.toUtf8()).toVariant();
        }
        return out;
    }

    Q_INVOKABLE void clearCommands()
    {
        m_commands.clear();
        emit commandsChanged();
    }

signals:
    void event(const QString &json);
    void commandsChanged();
    void startCountChanged();

private slots:
    void deliver(const QString &json) { emit event(json); }

private:
    QStringList m_commands;
    bool m_autoReply = true;
    bool m_startSucceeds = true;
    int m_commandResult = 0;
    int m_startCount = 0;
    int m_stopCount = 0;
    double m_nextTransfer = 100;
};

// Finding things in the object tree, for tests and for the checks above.
class Probe : public QObject
{
    Q_OBJECT
public:
    explicit Probe(const QString &appDir)
        : m_appDir(QDir(appDir).absolutePath() + QLatin1Char('/'))
    {
    }

    // The first object below `root` with this objectName, or null.
    Q_INVOKABLE QObject *find(QObject *root, const QString &name) const
    {
        for (QObject *o : everything(root)) {
            if (o->objectName() == name) {
                return o;
            }
        }
        return nullptr;
    }

    Q_INVOKABLE QVariantList findAll(QObject *root, const QString &name) const
    {
        QVariantList out;
        for (QObject *o : everything(root)) {
            if (o->objectName() == name) {
                out << QVariant::fromValue(o);
            }
        }
        return out;
    }

    // The text of every visible text item below `root`.
    Q_INVOKABLE QStringList texts(QObject *root) const
    {
        QStringList out;
        for (QObject *o : everything(root)) {
            if (isText(o) && visible(o)) {
                out << o->property("text").toString();
            }
        }
        return out;
    }

    // What the file that created `object` is, relative to qml/, or "".
    Q_INVOKABLE QString appFileOf(QObject *object) const
    {
        QQmlContext *context = QQmlEngine::contextForObject(object);
        const QString path = context ? context->baseUrl().toLocalFile() : QString();
        return path.startsWith(m_appDir) ? path.mid(m_appDir.size()) : QString();
    }

    // Rule 2 of the header, as messages.
    Q_INVOKABLE QStringList plainTextViolations(QObject *root)
    {
        QStringList out;
        for (QObject *o : everything(root)) {
            if (!isText(o)) {
                continue;
            }
            const QString file = appFileOf(o);
            if (!file.isEmpty()) {
                m_appTextsChecked++;
            }
            const int format = o->property("textFormat").toInt();
            if (format == 0) { // Text.PlainText, TextEdit.PlainText
                continue;
            }
            const QString text = o->property("text").toString();
            if (!file.isEmpty()) {
                out << QStringLiteral("%1: a %2 with textFormat %3 (not PlainText) shows \"%4\"")
                           .arg(file, QString::fromUtf8(o->metaObject()->className()))
                           .arg(format)
                           .arg(text.left(60));
            } else if (text.contains(QLatin1String("EVIL"))) {
                out << QStringLiteral("peer text reached a Silica-owned text item (textFormat %1): \"%2\"")
                           .arg(format)
                           .arg(text.left(60));
            }
        }
        return out;
    }

    // How many text items from the app's files the checks have looked at,
    // over the whole test: a check that saw none checked nothing.
    int appTextsChecked() const { return m_appTextsChecked; }

    // Warnings so far, so a test can assert that something it did warned.
    Q_INVOKABLE QStringList takeMessages()
    {
        const QStringList out = g_messages;
        g_messages.clear();
        return out;
    }

    Q_INVOKABLE void collectGarbage(QObject *anything) const
    {
        QQmlEngine *engine = qmlEngine(anything);
        if (engine) {
            engine->collectGarbage();
        }
    }

private:
    static bool visible(QObject *o)
    {
        auto *item = qobject_cast<QQuickItem *>(o);
        return !item || item->isVisible();
    }

    QString m_appDir;
    int m_appTextsChecked = 0;
};

int main(int argc, char *argv[])
{
    qInstallMessageHandler(onMessage);
    QGuiApplication app(argc, argv);

    QString appDir;
    QString stubsDir;
    QString testFile;
    int timeoutMs = 20000;
    // Catalogues to install, e.g. the English one for its plural forms.
    QList<QTranslator *> translators;
    const QStringList args = app.arguments();
    for (int i = 1; i < args.size(); i++) {
        if (args[i] == QLatin1String("--app") && i + 1 < args.size()) {
            appDir = args[++i];
        } else if (args[i] == QLatin1String("--stubs") && i + 1 < args.size()) {
            stubsDir = args[++i];
        } else if (args[i] == QLatin1String("--qm") && i + 1 < args.size()) {
            auto *translator = new QTranslator(&app);
            if (!translator->load(args[++i])) {
                std::fprintf(stderr, "cannot load %s\n", qPrintable(args[i]));
                return 2;
            }
            app.installTranslator(translator);
            translators << translator;
        } else if (args[i] == QLatin1String("--timeout") && i + 1 < args.size()) {
            timeoutMs = args[++i].toInt();
        } else {
            testFile = args[i];
        }
    }
    if (appDir.isEmpty() || stubsDir.isEmpty() || testFile.isEmpty()) {
        std::fprintf(stderr, "usage: qml_runner --app <qml dir> --stubs <stubs dir> [--qm file.qm]... "
                             "[--timeout ms] test.qml\n");
        return 2;
    }
    const QString name = QFileInfo(testFile).fileName();

    QQmlEngine engine;
    engine.addImportPath(QDir(stubsDir).absolutePath());
    engine.addImageProvider(QStringLiteral("theme"), new ThemeImages);
    FakeBridge bridge;
    Probe probe(appDir);
    engine.rootContext()->setContextProperty(QStringLiteral("bridge"), &bridge);
    engine.rootContext()->setContextProperty(QStringLiteral("probe"), &probe);

    QStringList failures;
    QQmlComponent component(&engine, QUrl::fromLocalFile(QFileInfo(testFile).absoluteFilePath()));
    QPointer<QObject> root = component.create();
    if (!root) {
        for (const QQmlError &e : component.errors()) {
            failures << e.toString();
        }
    } else {
        QElapsedTimer timer;
        timer.start();
        while (root && !root->property("done").toBool() && timer.elapsed() < timeoutMs) {
            app.processEvents(QEventLoop::AllEvents, 20);
        }
        if (!root) {
            failures << QStringLiteral("the test's root went away");
        } else {
            if (!root->property("done").toBool()) {
                failures << QStringLiteral("timed out after %1 ms at %2")
                                .arg(timeoutMs)
                                .arg(root->property("current").toString());
            }
            for (const QVariant &f : root->property("failures").toList()) {
                failures << f.toString();
            }
            // The script checks between steps; report here only what it
            // has not already.
            for (const QString &v : probe.plainTextViolations(root)) {
                bool known = false;
                for (const QString &f : failures) {
                    known = known || f.endsWith(v);
                }
                if (!known) {
                    failures << v;
                }
            }
            if (probe.appTextsChecked() == 0) {
                failures << QStringLiteral("no text item from qml/ was ever checked: did anything load?");
            }
        }
    }
    for (const QString &m : g_messages) {
        failures << QStringLiteral("Qt warning: ") + m;
    }

    if (failures.isEmpty()) {
        std::printf("PASS %s\n", qPrintable(name));
    } else {
        std::printf("FAIL %s\n", qPrintable(name));
        for (const QString &f : failures) {
            std::printf("  - %s\n", qPrintable(f));
        }
    }
    std::fflush(stdout);
    delete root.data();
    return failures.isEmpty() ? 0 : 1;
}

#include "runner.moc"
