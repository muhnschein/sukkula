// SPDX-License-Identifier: GPL-3.0-or-later
//
// A host stand-in for libsailfishapp's header, declaring the same four
// functions with the same forward declarations, so src/main.cpp compiles
// and runs on a desktop Qt exactly as written. Never used on a device.

#ifndef SAILFISHAPP_H
#define SAILFISHAPP_H

#include <QtGlobal>
#include <QUrl>

class QGuiApplication;
class QQuickView;
class QString;

namespace SailfishApp {
QGuiApplication *application(int &argc, char **argv);
QQuickView *createView();
QUrl pathTo(const QString &filename);
QUrl pathToMainQml();
}

#endif // SAILFISHAPP_H
