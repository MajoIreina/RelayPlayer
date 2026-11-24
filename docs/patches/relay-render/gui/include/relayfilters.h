#pragma once
#include <QJsonObject>
#include <QString>
#include <QByteArray>
inline QByteArray relayFilterShader(const QJsonObject &settings){
 auto number=[&](const char *key,double fallback){return settings.value(key).toDouble(fallback);};
 return QString::fromUtf8(R"GLSL(//!HOOK SCALED
//!BIND HOOKED
//!DESC Relay color and texture
vec4 hook(){
 vec4 c=HOOKED_tex(HOOKED_pos);
 vec3 avg=(HOOKED_texOff(vec2(1,0)).rgb+HOOKED_texOff(vec2(-1,0)).rgb+HOOKED_texOff(vec2(0,1)).rgb+HOOKED_texOff(vec2(0,-1)).rgb)*0.25;
 c.rgb+=(c.rgb-avg)*%1;
 float l=dot(c.rgb,vec3(0.2126,0.7152,0.0722));
 float sat=max(c.r,max(c.g,c.b))-min(c.r,min(c.g,c.b));
 c.rgb=mix(vec3(l),c.rgb,1.0+%2*(1.0-clamp(sat,0.0,1.0)));
 c.rgb+=%3*0.2*pow(max(0.0,1.0-l),3.0);
 float noise=fract(sin(dot(HOOKED_pos*HOOKED_size,vec2(12.9898,78.233)))*43758.5453)-0.5;
 c.rgb+=noise*%4*0.08;
 return c;
}
)GLSL").arg(number("clarity",0)+number("textureRecovery",0),0,'f',4).arg(number("vibrance",0),0,'f',4).arg(number("shadowBoost",0),0,'f',4).arg(number("filmGrain",0),0,'f',4).toUtf8();
}
