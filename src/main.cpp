// SPDX-License-Identifier: GPL-3.0-or-later
//
// Sukkula's start-up shim (spec §3): boot Silica through libsailfishapp,
// hand QML the one bridge to the Rust engine, and stop the engine before
// anything it could call back into is gone.

#include <QGuiApplication>
#include <QLocale>
#include <QQmlContext>
#include <QQuickView>
#include <QScopedPointer>
#include <QTranslator>

#include <sailfishapp.h>

#include "bridge.h"

// Exported: the silica-qt5 booster dlopen()s this binary and looks main()
// up in its dynamic symbol table (Harbour 1.7). src/dynamic.list puts it
// there, since the link strips everything else.
Q_DECL_EXPORT int main(int argc, char *argv[])
{
    QScopedPointer<QGuiApplication> app(SailfishApp::application(argc, argv));

    // Must match [X-Sailjail] in harbour-sukkula.desktop: Sailjail grants
    // ~/.local/share/sukkula/sukkula, and AppDataLocation is built from
    // these two names. libsailfishapp sets them from the desktop file
    // too; saying so here keeps the path right however the app started.
    app->setOrganizationName(QStringLiteral("sukkula"));
    app->setApplicationName(QStringLiteral("sukkula"));

    // harbour-sukkula-<lang>.qm, else harbour-sukkula.qm (the English
    // plural forms). Installed before any QML is compiled.
    QTranslator translator;
    if (translator.load(QLocale(), QStringLiteral("harbour-sukkula"), QStringLiteral("-"),
                        SailfishApp::pathTo(QStringLiteral("translations")).toLocalFile(),
                        QStringLiteral(".qm"))) {
        app->installTranslator(&translator);
    }

    // Declared before the view, so destroyed after it: QML never holds a
    // dangling bridge. Engine.qml calls start() once it is listening.
    Bridge bridge;
    QObject::connect(app.data(), &QCoreApplication::aboutToQuit, &bridge, &Bridge::stop);

    QScopedPointer<QQuickView> view(SailfishApp::createView());
    view->rootContext()->setContextProperty(QStringLiteral("bridge"), &bridge);
    view->setSource(SailfishApp::pathTo(QStringLiteral("qml/harbour-sukkula.qml")));
    view->show();

    const int status = app->exec();
    // Again, for an exit that did not pass aboutToQuit; stop() is
    // idempotent. After it no engine thread runs.
    bridge.stop();
    return status;
}
