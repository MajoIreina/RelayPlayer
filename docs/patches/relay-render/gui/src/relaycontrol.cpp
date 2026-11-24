// Relay integration: private inherited stdin, bounded commands, no network control listener.
#include "relaycontrol.h"
#include "qmlmainwindow.h"
#include <QJsonDocument>
#include <QPointer>
#include <QMetaObject>
#include <thread>
#include <cstdio>
#include <string>
void relayControlStart(QmlMainWindow *window) {
 if(qEnvironmentVariable("RELAY_RENDER_PROTOCOL") != QStringLiteral("1")) return;
 QPointer<QmlMainWindow> guard(window);
 std::thread([guard]{
  std::string line; int c;
  while((c=std::getchar())!=EOF){
   if(c!='\n'){if(line.size()>=65536) return;line.push_back(static_cast<char>(c));continue;}
   QJsonParseError error; auto doc=QJsonDocument::fromJson(QByteArray::fromStdString(line),&error);line.clear();
   if(error.error!=QJsonParseError::NoError||!doc.isObject())continue;
   auto command=doc.object();
   if(guard)QMetaObject::invokeMethod(guard,[guard,command]{if(guard)guard->applyRelaySettings(command);},Qt::QueuedConnection);
  }
 }).detach();
 std::printf("RELAY_RENDER {\"type\":\"ready\",\"protocol\":1}\n");std::fflush(stdout);
}
