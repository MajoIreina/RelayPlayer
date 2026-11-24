#pragma once
#include <QThread>
#include <QJsonObject>
#include <QJsonArray>
#include <QMutex>
#include <QPointer>
#include <QAtomicInteger>
extern "C" {
#include <libavutil/frame.h>
}
class QmlMainWindow;
QJsonArray relayCaptureDevices();
class RelayCapture : public QThread {
 QmlMainWindow *owner;
 QJsonObject controls;
 QMutex frame_mutex;
 AVFrame *latest=nullptr;
 bool delivery_pending=false;
 QAtomicInteger<int> state=0,width=0,height=0,fps_milli=0;
 QAtomicInteger<qint64> last_frame_us=0;
 void run() override;
public:
 RelayCapture(QmlMainWindow *owner,const QJsonObject &settings);
 ~RelayCapture();
 void deliver();
 QJsonObject metrics() const;
};
