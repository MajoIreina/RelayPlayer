//! Validated public playback controls. Values are requests until the renderer acknowledges frames.
use serde_json::{json, Value};
pub fn defaults() -> Value {
    serde_json::from_str(r#"{
 "resolution":"1080p","fps":60,"bitrateKbps":30000,"codec":"hevc","pictureSource":"stream",
 "lockBitrate":false,"framePacing":"auto","hdr":false,"autoHdr":false,"toneMapMode":"auto","ambientFill":false,
 "saturation":1.0,"contrast":1.0,"gamma":1.0,"vibrance":0.0,"shadowBoost":0.0,"clarity":0.0,"colorTemperature":0.0,"filmGrain":0.0,
 "antiAliasing":false,"debanding":false,"debandingStrength":"medium",
 "upscaler":"off","upscalerTarget":"auto","upscalerQuality":"0","artcnnStyle":"auto","upscalerSharpness":0.0,"textureRecovery":0.0,
 "neuralRendering":false,"neuralRenderingPreset":"default","neuralRenderingIntensity":1.0,"neuralRenderingHudProtection":"smart","neuralRenderingColorPreserve":true,
 "captureDeviceId":"","captureWidth":0,"captureHeight":0,"captureFps":0,"captureFormat":"auto","captureColorRange":"auto","captureHdrMode":"auto","captureSkipWebcams":true,
 "frameGeneration":false,"frameGenerationBackend":"warp","frameGenerationMultiplier":2,"frameDedup":"auto"
}"#).expect("static playback defaults")
}
pub fn valid(key: &str, v: &Value) -> bool {
    let choice = |options: &[&str]| v.as_str().is_some_and(|s| options.contains(&s));
    let range = |lo: f64, hi: f64| {
        v.as_f64()
            .is_some_and(|n| n.is_finite() && n >= lo && n <= hi)
    };
    match key {
        "captureDeviceId" => v
            .as_str()
            .is_some_and(|s| s.len() <= 256 && !s.chars().any(char::is_control)),
        "captureWidth" => v
            .as_u64()
            .is_some_and(|n| n == 0 || (320..=7680).contains(&n)),
        "captureHeight" => v
            .as_u64()
            .is_some_and(|n| n == 0 || (240..=4320).contains(&n)),
        "captureFps" => range(0.0, 240.0),
        "captureFormat" => choice(&["auto", "NV12", "YUY2", "MJPG", "RGB24", "P010"]),
        "captureColorRange" => choice(&["auto", "limited", "full"]),
        "captureHdrMode" => choice(&["auto", "off", "hdr10"]),
        "captureSkipWebcams" => v.is_boolean(),
        "resolution" => choice(&["360p", "540p", "720p", "1080p"]),
        "fps" => matches!(v.as_u64(), Some(30 | 60)),
        "codec" => choice(&["h264", "hevc", "h265", "h265_hdr"]),
        "bitrateKbps" => v.as_u64().is_some_and(|n| (1000..=200000).contains(&n)),
        "pictureSource" => choice(&["auto", "stream", "capture"]),
        "framePacing" => choice(&["auto", "lowest-latency", "smooth-vsync"]),
        "toneMapMode" => choice(&["auto", "hdr10-passthrough", "bt2390-sdr"]),
        "lockBitrate"
        | "hdr"
        | "autoHdr"
        | "ambientFill"
        | "antiAliasing"
        | "debanding"
        | "neuralRendering"
        | "neuralRenderingColorPreserve"
        | "frameGeneration" => v.is_boolean(),
        "saturation" => range(0.5, 1.5),
        "contrast" | "gamma" => range(0.8, 1.2),
        "vibrance" | "colorTemperature" => range(-1.0, 1.0),
        "shadowBoost" | "clarity" | "filmGrain" | "upscalerSharpness" | "textureRecovery" => {
            range(0.0, 1.0)
        }
        "debandingStrength" => choice(&["low", "medium", "high"]),
        "upscaler" => choice(&["off", "fsr1", "rtx-vsr", "artcnn", "integer"]),
        "upscalerTarget" => choice(&["auto", "1080p", "1440p", "2160p"]),
        "upscalerQuality" => choice(&["0", "1", "2", "3", "4"]),
        "artcnnStyle" => choice(&["auto", "sharp", "denoise"]),
        "neuralRenderingPreset" => choice(&["default", "natural", "cinematic"]),
        "neuralRenderingHudProtection" => choice(&["off", "smart", "strict"]),
        "neuralRenderingIntensity" => range(0.0, 3.0),
        "frameGenerationBackend" => choice(&["dlssg", "nvidia", "warp"]),
        "frameGenerationMultiplier" => matches!(v.as_u64(), Some(2..=4)),
        "frameDedup" => choice(&["off", "auto", "60-to-30"]),
        _ => false,
    }
}
pub fn supported(key: &str) -> bool {
    defaults().get(key).is_some()
        && !matches!(
            key,
            "autoHdr" | "neuralRenderingHudProtection" | "neuralRenderingColorPreserve"
        )
}
pub fn session_key(key: &str) -> bool {
    matches!(
        key,
        "resolution" | "fps" | "bitrateKbps" | "codec" | "hdr" | "autoHdr" | "pictureSource"
    )
}
pub fn session_settings(settings: &Value) -> Value {
    let mut result = json!({});
    for key in [
        "resolution",
        "fps",
        "bitrateKbps",
        "codec",
        "neuralRendering",
    ] {
        result[key] = settings[key].clone();
    }
    if settings["hdr"] == true || settings["codec"] == "h265_hdr" {
        result["codec"] = json!("h265_hdr");
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_reject_invalid_values_and_credentials() {
        for (k, v) in defaults().as_object().unwrap() {
            assert!(valid(k, v), "{k}");
        }
        assert!(!valid("rp_key", &json!("secret")));
        assert!(!valid("upscaler", &json!("dlss-fake")));
        assert!(!valid("bitrateKbps", &json!(200001)));
        assert!(!valid("frameGenerationMultiplier", &json!(5)));
    }
    #[test]
    fn hdr_maps_to_negotiated_codec() {
        let mut s = defaults();
        s["hdr"] = json!(true);
        assert_eq!(session_settings(&s)["codec"], "h265_hdr");
    }
}
