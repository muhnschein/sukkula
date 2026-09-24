// SPDX-License-Identifier: GPL-3.0-or-later
//
// libsailfishapp for the host: boots src/main.cpp against a desktop Qt, the
// QML stubs and the repository's own qml/, so the whole shell starts,
// talks to the engine and quits under a test's control. Test-only knobs
// live here, never in main.cpp:
//
//   SUKKULA_HOST_ROOT          where pathTo() looks (default: the source tree)
//   SUKKULA_HOST_TRANSLATIONS  where pathTo("translations") looks
//   SUKKULA_HOST_QUIT_AFTER_MS quit after this long
//   SUKKULA_HOST_DUMP_TEXTS    print every text item's text before quitting
//
// Every Qt warning is printed with a HOST-WARN: prefix and counted; the
// last line is "HOST-APP: warnings=N texts=M", which tests/run-cpp-tests.sh
// reads.

#include "sailfishapp.h"

#include <cstdio>

#include <QDir>
#include <QGuiApplication>
#include <QPixmap>
#include <QQmlEngine>
#include <QQuickImageProvider>
#include <QQuickItem>
#include <QQuickView>
#include <QSet>
#include <QTimer>

namespace {

int g_warnings = 0;
QQuickView *g_view = nullptr;

void onMessage(QtMsgType type, const QMessageLogContext &, const QString &message)
{
    // Handlers in Qt 5.6's syntax, which a 5.15 host calls deprecated.
    if (message.contains(QLatin1String("Implicitly defined onFoo properties in Connections are deprecated"))) {
        return;
    }
    if (type != QtDebugMsg && type != QtInfoMsg) {
        g_warnings++;
        std::fprintf(stderr, "HOST-WARN: %s\n", qPrintable(message));
    }
}

// image://theme/..., which Silica provides on the phone.
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

// QObject children and visual children both: a Repeater's delegates are
// the latter only.
void collectTexts(QObject *object, QStringList &out, QSet<QObject *> &seen)
{
    if (!object || seen.contains(object)) {
        return;
    }
    seen.insert(object);
    if (object->inherits("QQuickText") || object->inherits("QQuickTextEdit")) {
        const QString text = object->property("text").toString();
        if (!text.isEmpty()) {
            out << text;
        }
    }
    for (QObject *child : object->children()) {
        collectTexts(child, out, seen);
    }
    if (auto *item = qobject_cast<QQuickItem *>(object)) {
        for (QQuickItem *child : item->childItems()) {
            collectTexts(child, out, seen);
        }
    }
}

void finish()
{
    QStringList texts;
    QSet<QObject *> seen;
    if (g_view && g_view->rootObject()) {
        collectTexts(g_view->rootObject(), texts, seen);
    }
    if (qEnvironmentVariableIsSet("SUKKULA_HOST_DUMP_TEXTS")) {
        for (const QString &text : texts) {
            std::printf("HOST-TEXT: %s\n", qPrintable(text));
        }
    }
    if (g_view && g_view->status() != QQuickView::Ready) {
        g_warnings++;
        std::fprintf(stderr, "HOST-WARN: the view is not ready\n");
    }
    std::printf("HOST-APP: warnings=%d texts=%d\n", g_warnings, int(texts.size()));
    std::fflush(stdout);
}

QString root()
{
    const QString env = qEnvironmentVariable("SUKKULA_HOST_ROOT");
    return env.isEmpty() ? QStringLiteral(SUKKULA_SOURCE_ROOT) : env;
}

} // namespace

namespace SailfishApp {

QGuiApplication *application(int &argc, char **argv)
{
    qInstallMessageHandler(onMessage);
    auto *app = new QGuiApplication(argc, argv);
    QObject::connect(app, &QCoreApplication::aboutToQuit, finish);
    bool ok = false;
    const int quitAfter = qEnvironmentVariableIntValue("SUKKULA_HOST_QUIT_AFTER_MS", &ok);
    if (ok && quitAfter > 0) {
        QTimer::singleShot(quitAfter, app, &QCoreApplication::quit);
    }
    return app;
}

QQuickView *createView()
{
    auto *view = new QQuickView;
    view->engine()->addImageProvider(QStringLiteral("theme"), new ThemeImages);
    view->setResizeMode(QQuickView::SizeRootObjectToView);
    view->resize(540, 960);
    g_view = view;
    QObject::connect(view, &QObject::destroyed, [] { g_view = nullptr; });
    return view;
}

QUrl pathTo(const QString &filename)
{
    if (filename == QLatin1String("translations")) {
        const QString env = qEnvironmentVariable("SUKKULA_HOST_TRANSLATIONS");
        if (!env.isEmpty()) {
            return QUrl::fromLocalFile(env);
        }
    }
    return QUrl::fromLocalFile(QDir(root()).filePath(filename));
}

QUrl pathToMainQml()
{
    return pathTo(QStringLiteral("qml/harbour-sukkula.qml"));
}

} // namespace SailfishApp
