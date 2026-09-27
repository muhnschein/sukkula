// SPDX-License-Identifier: GPL-3.0-or-later

#include "scanner.h"

#include <QByteArray>
#include <QMetaObject>
#include <QQuickItem>
#include <QQuickItemGrabResult>
#include <QRunnable>
#include <QSizeF>

#include <sukkula.h>

#include <algorithm>

namespace {

// How long a grab may take before the frame is given up on. A grab whose
// item went away in the meantime never says it is ready.
const int GrabTimeoutMs = 2000;

// One grey frame through sukkula_scan_qr(), and what it read posted back
// to the scanner's thread.
class ScanJob : public QRunnable
{
public:
    ScanJob(QObject *scanner, const QImage &frame)
        : m_scanner(scanner)
        , m_frame(frame)
    {
    }

    void run() override
    {
        QByteArray out(SUKKULA_SCAN_BYTES, '\0');
        const int32_t n = sukkula_scan_qr(m_frame.constBits(), static_cast<uint32_t>(m_frame.width()),
                                          static_cast<uint32_t>(m_frame.height()),
                                          static_cast<uint32_t>(m_frame.bytesPerLine()), out.data(),
                                          static_cast<uint32_t>(out.size()));
        const QString json = n > 0 ? QString::fromUtf8(out.constData(), n) : QString();
        // The scanner waits for this job before it is destroyed, so it is
        // still there; a call posted to it and not yet run when it goes is
        // dropped with it.
        QMetaObject::invokeMethod(m_scanner, "deliver", Qt::QueuedConnection, Q_ARG(QString, json));
    }

private:
    QObject *const m_scanner;
    const QImage m_frame;
};

} // namespace

Scanner::Scanner(QObject *parent)
    : QObject(parent)
{
    m_pool.setMaxThreadCount(1);
    m_grabTimeout.setSingleShot(true);
    m_grabTimeout.setInterval(GrabTimeoutMs);
    connect(&m_grabTimeout, &QTimer::timeout, this, [this]() {
        m_grab.clear();
        setBusy(false);
        emit missed();
    });
}

Scanner::~Scanner()
{
    m_pool.waitForDone();
}

bool Scanner::busy() const
{
    return m_busy;
}

bool Scanner::scan(QQuickItem *item)
{
    if (m_busy || !item || !item->window() || !item->isVisible() || item->width() < 1
        || item->height() < 1) {
        return false;
    }
    // Scaled while it is grabbed, on the GPU: the engine never sees more
    // than MaxSide.
    QSizeF size(item->width(), item->height());
    if (std::max(size.width(), size.height()) > MaxSide) {
        size.scale(MaxSide, MaxSide, Qt::KeepAspectRatio);
    }
    const QSize target = size.toSize().expandedTo(QSize(1, 1));
    QSharedPointer<QQuickItemGrabResult> grab = item->grabToImage(target);
    if (!grab) {
        return false;
    }
    m_grab = grab;
    connect(grab.data(), &QQuickItemGrabResult::ready, this, &Scanner::onGrabbed);
    m_grabTimeout.start();
    setBusy(true);
    return true;
}

bool Scanner::scanImage(const QImage &image)
{
    if (m_busy) {
        return false;
    }
    setBusy(true);
    submit(prepare(image));
    return true;
}

QImage Scanner::prepare(const QImage &image)
{
    if (image.isNull()) {
        return QImage();
    }
    QImage frame = image;
    if (std::max(frame.width(), frame.height()) > MaxSide) {
        frame = frame.scaled(MaxSide, MaxSide, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    }
    return frame.isNull() ? QImage() : frame.convertToFormat(QImage::Format_Grayscale8);
}

void Scanner::onGrabbed()
{
    // A grab given up on, or not this one: nothing to do.
    auto *grab = qobject_cast<QQuickItemGrabResult *>(sender());
    if (!m_grab || grab != m_grab.data()) {
        return;
    }
    m_grabTimeout.stop();
    const QImage image = m_grab->image();
    m_grab.clear();
    submit(prepare(image));
}

void Scanner::submit(const QImage &frame)
{
    if (frame.isNull()) {
        // Nothing to scan; answered as a frame would be, after the caller
        // has returned.
        QMetaObject::invokeMethod(this, "deliver", Qt::QueuedConnection, Q_ARG(QString, QString()));
        return;
    }
    m_pool.start(new ScanJob(this, frame));
}

void Scanner::deliver(const QString &json)
{
    setBusy(false);
    if (json.isEmpty()) {
        emit missed();
    } else {
        emit found(json);
    }
}

void Scanner::setBusy(bool busy)
{
    if (m_busy != busy) {
        m_busy = busy;
        emit busyChanged();
    }
}
