// SPDX-License-Identifier: GPL-3.0-or-later
//
// The scanner (src/scanner.cpp): frames made grey and small before the
// engine sees them, one frame at a time, the answer on the GUI thread,
// a grab of a real item on screen, and a scanner destroyed while a frame
// is in hand (run under ASan with CONFIG+=sukkula_sanitize).
//
// The frame with a code in it is "gala-tulip-acorn" as a QR code, the
// croc_code example of docs/FFI.md: the real engine reads it, and the stub
// answers the same for any frame with something dark in it.

#include <QColor>
#include <QImage>
#include <QPainter>
#include <QQmlComponent>
#include <QQmlEngine>
#include <QQuickItem>
#include <QQuickView>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QThread>
#include <QtTest>

#include "scanner.h"

#ifdef SUKKULA_STUB_ENGINE
#include "sukkula_stub.h"
#endif

namespace {

const char *const CrocQr[25] = {
    "1111111001011010001111111", "1000001011111100001000001", "1011101000110001001011101",
    "1011101000011100001011101", "1011101011011100101011101", "1000001001110011001000001",
    "1111111010101010101111111", "0000000001111000100000000", "1010101001110001100010010",
    "0111000001011100101000111", "0111111110101010101110111", "1011100011010110010110000",
    "1001101110000010001000001", "0101000011010010111001011", "1000001101000100010011111",
    "0100000010110000100011001", "1001101110001001111110011", "0000000010111101100010111",
    "1111111001111011101011111", "1000001000001110100010010", "1011101011010010111111001",
    "1011101001110010100110000", "1011101010000100100110101", "1000001001110001110000010",
    "1111111010101001111111011",
};

const QString Croc = QStringLiteral("{\"found\":\"croc\",\"code\":\"gala-tulip-acorn\"}");

// A colour image of `size` with the QR code in it, `module` pixels a
// module and a quiet zone of four modules around it.
QImage withCode(const QSize &size, int module)
{
    QImage image(size, QImage::Format_ARGB32);
    image.fill(QColor(240, 236, 220));
    QPainter p(&image);
    for (int y = 0; y < 25; y++) {
        for (int x = 0; x < 25; x++) {
            if (CrocQr[y][x] == '1') {
                p.fillRect((4 + x) * module, (4 + y) * module, module, module, QColor(20, 30, 60));
            }
        }
    }
    return image;
}

} // namespace

class TestScanner : public QObject
{
    Q_OBJECT

private slots:
    void framesAreGreyAndSmall()
    {
        QVERIFY(Scanner::prepare(QImage()).isNull());
        const QImage big = Scanner::prepare(withCode(QSize(1280, 720), 12));
        QCOMPARE(big.format(), QImage::Format_Grayscale8);
        QCOMPARE(big.size(), QSize(640, 360));
        const QImage tall = Scanner::prepare(QImage(QSize(300, 2000), QImage::Format_RGB32));
        QCOMPARE(tall.size(), QSize(96, 640));
        const QImage small = Scanner::prepare(withCode(QSize(300, 200), 4));
        QCOMPARE(small.size(), QSize(300, 200));
        QCOMPARE(small.format(), QImage::Format_Grayscale8);
    }

    void aCodeIsFoundOnTheGuiThread()
    {
        Scanner scanner;
        QSignalSpy found(&scanner, &Scanner::found);
        QSignalSpy missed(&scanner, &Scanner::missed);
        QSignalSpy busy(&scanner, &Scanner::busyChanged);
        QThread *gui = QThread::currentThread();
        QThread *seen = nullptr;
        connect(&scanner, &Scanner::found, this, [&seen]() { seen = QThread::currentThread(); });
        QVERIFY(scanner.scanImage(withCode(QSize(1280, 960), 12)));
        QVERIFY(scanner.busy());
        QVERIFY(found.wait(10000));
        QCOMPARE(found.first().first().toString(), Croc);
        QCOMPARE(seen, gui);
        QVERIFY(!scanner.busy());
        QCOMPARE(busy.count(), 2);
        QCOMPARE(missed.count(), 0);
#ifdef SUKKULA_STUB_ENGINE
        unsigned w = 0, h = 0, stride = 0;
        int scans = 0;
        sukkula_stub_last_scan(&w, &h, &stride, &scans);
        QCOMPARE(w, 640u);
        QCOMPARE(h, 480u);
        QCOMPARE(stride, 640u);
#endif
    }

    void aFrameWithoutACodeIsMissed()
    {
        Scanner scanner;
        QSignalSpy found(&scanner, &Scanner::found);
        QSignalSpy missed(&scanner, &Scanner::missed);
        QImage blank(QSize(320, 240), QImage::Format_RGB32);
        blank.fill(Qt::white);
        QVERIFY(scanner.scanImage(blank));
        QVERIFY(missed.wait(10000));
        QCOMPARE(found.count(), 0);
        // An empty image is answered too, after the call has returned.
        QVERIFY(scanner.scanImage(QImage()));
        QCOMPARE(missed.count(), 1);
        QVERIFY(missed.wait(1000));
        QVERIFY(!scanner.busy());
    }

    void oneFrameAtATime()
    {
#ifdef SUKKULA_STUB_ENGINE
        sukkula_stub_scan_delay_ms(300);
#endif
        Scanner scanner;
        QSignalSpy found(&scanner, &Scanner::found);
        QVERIFY(scanner.scanImage(withCode(QSize(400, 300), 6)));
        QVERIFY(!scanner.scanImage(withCode(QSize(400, 300), 6)));
        QVERIFY(found.wait(10000));
        QCOMPARE(found.count(), 1);
        QVERIFY(scanner.scanImage(withCode(QSize(400, 300), 6)));
        QVERIFY(found.wait(10000));
#ifdef SUKKULA_STUB_ENGINE
        sukkula_stub_scan_delay_ms(0);
#endif
    }

    void aScannerDestroyedMidFrameWaitsAndSaysNothing()
    {
#ifdef SUKKULA_STUB_ENGINE
        sukkula_stub_scan_delay_ms(300);
#endif
        int heard = 0;
        {
            Scanner scanner;
            connect(&scanner, &Scanner::found, this, [&heard]() { heard++; });
            connect(&scanner, &Scanner::missed, this, [&heard]() { heard++; });
            QVERIFY(scanner.scanImage(withCode(QSize(400, 300), 6)));
        }
        QTest::qWait(500);
        QCOMPARE(heard, 0);
#ifdef SUKKULA_STUB_ENGINE
        sukkula_stub_scan_delay_ms(0);
#endif
    }

    // The item on screen, grabbed as the viewfinder is: an Image of the
    // code in an offscreen window.
    void anItemOnScreenIsGrabbedAndScanned()
    {
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        const QString png = dir.filePath(QStringLiteral("code.png"));
        QVERIFY(withCode(QSize(330, 330), 10).save(png));

        QQuickView view;
        view.setResizeMode(QQuickView::SizeRootObjectToView);
        QQmlComponent component(view.engine());
        component.setData("import QtQuick 2.6\n"
                          "Item { width: 400; height: 400\n"
                          "  property alias source: image.source\n"
                          "  Image { id: image; objectName: \"viewfinder\"; anchors.centerIn: parent }\n"
                          "}\n",
                          QUrl());
        auto *root = qobject_cast<QQuickItem *>(component.create());
        QVERIFY2(root, qPrintable(component.errorString()));
        root->setProperty("source", QUrl::fromLocalFile(png));
        view.setContent(QUrl(), &component, root);
        view.resize(400, 400);
        view.show();
        QVERIFY(QTest::qWaitForWindowExposed(&view));

        auto *item = root->findChild<QQuickItem *>(QStringLiteral("viewfinder"));
        QVERIFY(item);
        QTRY_VERIFY(item->width() > 0);

        Scanner scanner;
        QSignalSpy found(&scanner, &Scanner::found);
        QVERIFY(!scanner.scan(nullptr));
        QVERIFY(scanner.scan(item));
        QVERIFY(!scanner.scan(item));
        QVERIFY(found.wait(10000));
        QCOMPARE(found.first().first().toString(), Croc);

        // Not shown: not grabbed.
        item->setVisible(false);
        QVERIFY(!scanner.scan(item));
    }
};

QTEST_MAIN(TestScanner)
#include "tst_scanner.moc"
