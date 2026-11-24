#pragma once
#include <QJsonObject>
#include <QSize>
#include <QString>
// Report measured renderer identity and NR evidence without probing on the UI thread.
inline QJsonObject relayGpuReport(const QString &adapter, const QString &driver,
    bool vulkan, bool nr_compiled, int nr_state, bool nr_confirmed,
    QSize display, double refresh)
{
 QJsonObject gpu;
 if(!adapter.isEmpty())gpu["adapterName"]=adapter;
 if(!driver.isEmpty())gpu["driverVersion"]=driver;
 gpu["renderer"]=vulkan?"Vulkan":"OpenGL";
 if(display.width()>0 && display.height()>0){
  gpu["displayWidth"]=display.width();gpu["displayHeight"]=display.height();
  if(refresh>0)gpu["displayHz"]=refresh;
 }
 if(nr_confirmed)gpu["dlssNrReported"]=true;
 if(nr_confirmed && nr_state!=3 && vulkan)gpu["dlssNrSupported"]=true;
 else if(nr_state==3 || !nr_compiled || !vulkan)gpu["dlssNrSupported"]=false;
 gpu["nrProbeState"]=nr_confirmed?"confirmed":nr_state==3?"failed":!vulkan?"backend-unsupported":!nr_compiled?"not-compiled":nr_state==2?"preparing":"not-probed";
 if(nr_state==3)gpu["nrUnavailableReason"]="NR 初始化或帧处理失败，请检查当前运行库与显卡驱动";
 else if(!vulkan)gpu["nrUnavailableReason"]="当前 OpenGL 渲染器不支持 NR，请使用 Vulkan";
 else if(!nr_compiled)gpu["nrUnavailableReason"]="当前渲染器未编译 NR 支持";
 return gpu;
}
