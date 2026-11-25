#pragma once
#include <QJsonObject>
#include <QString>
#include <QByteArray>
#include <cmath>
#include <libplacebo/shaders/custom.h>
#include <libplacebo/shaders/colorspace.h>
inline QByteArray relayFilterShader(const QJsonObject &settings){
 auto number=[&](const char *key,double lo,double hi){const double v=settings.value(key).toDouble(0);return std::isfinite(v)?qBound(lo,v,hi):0.0;};
 const double detail=number("clarity",0,1)+number("textureRecovery",0,1);
 const double vibrance=number("vibrance",-1,1),shadows=number("shadowBoost",0,1),grain=number("filmGrain",0,1);
 QByteArray shader;
 // Neutral controls use no custom pass and do not sample neighbouring pixels.
 if(detail!=0 || vibrance!=0 || shadows!=0 || grain!=0)shader+=QString::fromUtf8(R"GLSL(//!HOOK SCALED
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
)GLSL").arg(detail,0,'f',4).arg(vibrance,0,'f',4).arg(shadows,0,'f',4).arg(grain,0,'f',4).toUtf8();
 return shader;
}

// A COLOR hook can be fused into the render shader. libplacebo's optional
// LINEAR scaling stage is deliberately disabled for HDR, so do not hook it.
struct RelayExposure {
 float gain=1.0f;
 struct pl_hook hook={};
 RelayExposure(){
  hook.stages=PL_HOOK_SCALED;hook.input=PL_HOOK_SIG_COLOR;hook.priv=this;
  hook.hook=[](void *priv,const pl_hook_params *p){
   auto self=static_cast<RelayExposure*>(priv);
   pl_shader_linearize(p->sh,&p->color);
   pl_shader_var variable={};variable.var=pl_var_float("relay_exposure_gain");variable.data=&self->gain;variable.dynamic=true;
   pl_custom_shader code={};code.description="Relay exposure";code.body="color.rgb *= relay_exposure_gain;";code.input=code.output=PL_SHADER_SIG_COLOR;code.variables=&variable;code.num_variables=1;
   const bool ok=pl_shader_custom(p->sh,&code);
   pl_shader_delinearize(p->sh,&p->color);
   pl_hook_res result={};result.failed=!ok;result.output=PL_HOOK_SIG_NONE;result.sh=p->sh;
   result.repr=p->repr;result.color=p->color;result.components=p->components;result.rect=p->rect;return result;
  };
 }
 void set(double ev){gain=static_cast<float>(std::exp2(std::isfinite(ev)?qBound(-3.0,ev,3.0):0.0));}
 const struct pl_hook *get() const{return gain==1.0f?nullptr:&hook;}
};

