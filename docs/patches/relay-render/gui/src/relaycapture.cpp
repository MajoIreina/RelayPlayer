// DirectShow capture and FFmpeg decode. Enumerating devices never opens a camera.
#include "relaycapture.h"
#include "qmlmainwindow.h"
#include <QMetaObject>
#include <QElapsedTimer>
#include <QMutexLocker>
#include <QJsonDocument>
#include <QPointer>
#include <windows.h>
#include <dshow.h>
#include <cstdio>
#include <chiaki/time.h>
extern "C" {
#include <libavdevice/avdevice.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
}
QJsonArray relayCaptureDevices(){
 QJsonArray result;
 HRESULT init=CoInitializeEx(nullptr,COINIT_MULTITHREADED);
 ICreateDevEnum *dev=nullptr;IEnumMoniker *items=nullptr;
 if(SUCCEEDED(CoCreateInstance(CLSID_SystemDeviceEnum,nullptr,CLSCTX_INPROC_SERVER,IID_ICreateDevEnum,(void**)&dev))){
  if(dev->CreateClassEnumerator(CLSID_VideoInputDeviceCategory,&items,0)==S_OK){
   IMoniker *moniker=nullptr;
   while(items->Next(1,&moniker,nullptr)==S_OK){
    IPropertyBag *bag=nullptr;
    if(SUCCEEDED(moniker->BindToStorage(nullptr,nullptr,IID_IPropertyBag,(void**)&bag))){
     VARIANT name;VariantInit(&name);
     if(SUCCEEDED(bag->Read(L"FriendlyName",&name,nullptr))&&name.vt==VT_BSTR){
      const auto text=QString::fromWCharArray(name.bstrVal);
      result.append(QJsonObject{{"id",text},{"name",text},{"usable",true},{"modes",QJsonArray()}});
     }
     VariantClear(&name);bag->Release();
    }
    moniker->Release();
   }
   items->Release();
  }
  dev->Release();
 }
 if(SUCCEEDED(init))CoUninitialize();return result;
}
RelayCapture::RelayCapture(QmlMainWindow *w,const QJsonObject &s):QThread(w),owner(w),controls(s){}
RelayCapture::~RelayCapture(){requestInterruption();wait();QMutexLocker lock(&frame_mutex);av_frame_free(&latest);}
void RelayCapture::deliver(){
 AVFrame *frame=nullptr;{QMutexLocker lock(&frame_mutex);frame=latest;latest=nullptr;delivery_pending=false;}
 if(frame && !isInterruptionRequested())owner->presentRelayCapture(frame,controls.value("captureFps").toDouble(60));else av_frame_free(&frame);
}
QJsonObject RelayCapture::metrics() const {
 const int current=state.loadAcquire();
 const bool stalled=current==2 && chiaki_time_now_monotonic_us()-last_frame_us.loadAcquire()>2000000;
 QJsonObject result{{"captureState",current==3?"error":current==4?"stopped":stalled?"stalled":current==2?"active":"opening"},{"captureAudioActive",false}};
 if(width.loadRelaxed()>0){result["captureWidth"]=width.loadRelaxed();result["captureHeight"]=height.loadRelaxed();}
 if(current==2)result["captureFps"]=stalled?0.0:fps_milli.loadRelaxed()/1000.0;
 return result;
}
void RelayCapture::run(){
 state.storeRelease(1);QElapsedTimer sample;sample.start();int frame_count=0;
 CoInitializeEx(nullptr,COINIT_MULTITHREADED);avdevice_register_all();
 AVFormatContext *format=avformat_alloc_context();
 format->interrupt_callback={[](void*ctx){return static_cast<RelayCapture*>(ctx)->isInterruptionRequested()?1:0;},this};
 AVDictionary *opts=nullptr;
 auto width=controls.value("captureWidth").toInt();auto height=controls.value("captureHeight").toInt();
 if(width>0&&height>0)av_dict_set(&opts,"video_size",QString("%1x%2").arg(width).arg(height).toUtf8().constData(),0);
 const auto fps=controls.value("captureFps").toDouble();if(fps>0)av_dict_set(&opts,"framerate",QString::number(fps).toUtf8().constData(),0);
 const auto pixel=controls.value("captureFormat").toString("auto");
 if(pixel=="MJPG")av_dict_set(&opts,"vcodec","mjpeg",0);
 else if(pixel!="auto")av_dict_set(&opts,"pixel_format",pixel=="YUY2"?"yuyv422":pixel=="RGB24"?"bgr24":pixel=="P010"?"p010le":"nv12",0);
 av_dict_set(&opts,"rtbufsize","64M",0);
 const QByteArray input=("video="+controls.value("captureDeviceId").toString()).toUtf8();
 const auto *device=av_find_input_format("dshow");
 int status=avformat_open_input(&format,input.constData(),device,&opts);av_dict_free(&opts);
 AVCodecContext *codec=nullptr;AVPacket *packet=nullptr;AVFrame *frame=nullptr;
 if(status>=0 && avformat_find_stream_info(format,nullptr)>=0){
  const int index=av_find_best_stream(format,AVMEDIA_TYPE_VIDEO,-1,-1,nullptr,0);
  if(index>=0){
   auto *params=format->streams[index]->codecpar;codec=avcodec_alloc_context3(avcodec_find_decoder(params->codec_id));
   if(codec && avcodec_parameters_to_context(codec,params)>=0 && avcodec_open2(codec,codec->codec,nullptr)>=0){
    packet=av_packet_alloc();frame=av_frame_alloc();
    while(!isInterruptionRequested()&&av_read_frame(format,packet)>=0){
     if(packet->stream_index==index && avcodec_send_packet(codec,packet)>=0){
      while(avcodec_receive_frame(codec,frame)>=0){
       this->width.storeRelaxed(frame->width);this->height.storeRelaxed(frame->height);
       last_frame_us.storeRelease(static_cast<qint64>(chiaki_time_now_monotonic_us()));state.storeRelease(2);
       ++frame_count;if(sample.elapsed()>=1000){fps_milli.storeRelaxed(qRound(frame_count*1000000.0/sample.elapsed()));frame_count=0;sample.restart();}
       const auto range=controls.value("captureColorRange").toString("auto");
       if(range!="auto")frame->color_range=range=="full"?AVCOL_RANGE_JPEG:AVCOL_RANGE_MPEG;
       const auto hdr=controls.value("captureHdrMode").toString("auto");
       if(hdr=="hdr10"){frame->color_primaries=AVCOL_PRI_BT2020;frame->color_trc=AVCOL_TRC_SMPTE2084;frame->colorspace=AVCOL_SPC_BT2020_NCL;}
       else if(hdr=="off"){frame->color_primaries=AVCOL_PRI_BT709;frame->color_trc=AVCOL_TRC_BT709;frame->colorspace=AVCOL_SPC_BT709;}
       bool queue=false;
       {QMutexLocker lock(&frame_mutex);av_frame_free(&latest);latest=av_frame_clone(frame);if(!delivery_pending){delivery_pending=true;queue=true;}}
       if(queue)QMetaObject::invokeMethod(this,[this]{deliver();},Qt::QueuedConnection);
       av_frame_unref(frame);
      }
     }
     av_packet_unref(packet);
    }
   }else status=-1;
  }else status=-1;
 }
 state.storeRelease(isInterruptionRequested()?4:3);
 if(status<0){std::printf("RELAY_RENDER {\"type\":\"error\",\"message\":\"Capture device could not be opened; check device connection and capture mode\"}\n");std::fflush(stdout);}
 av_frame_free(&frame);av_packet_free(&packet);avcodec_free_context(&codec);avformat_close_input(&format);CoUninitialize();
}
