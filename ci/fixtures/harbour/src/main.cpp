// Fixture: the shape of the real src/main.cpp, for the selftest.
#include <QGuiApplication>
#include <QQuickView>
#include <QStandardPaths>
#include <sailfishapp.h>

// The booster dlopen()s the binary and calls this through .dynsym.
Q_DECL_EXPORT int main(int argc, char *argv[])
{
    QScopedPointer<QGuiApplication> app(SailfishApp::application(argc, argv));
    const QString data = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    Q_UNUSED(data);
    QScopedPointer<QQuickView> view(SailfishApp::createView());
    view->setSource(SailfishApp::pathToMainQml());
    view->show();
    return app->exec();
}
