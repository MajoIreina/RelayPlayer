//! Public runtime adapter for the original Relay player shell. No second app/layout.
use crate::{
    chiaki::{Player, Stage, Update},
    connections::Connections,
};
use serde_json::{json, Value};
use std::{fs, io, path::PathBuf};

pub struct PublicPlayback {
    pub player: Player,
    pub note: String,
    pub progress: Option<f32>,
    pub settings: Value,
    pub pending: bool,
    pub connections: Connections,
    target: Option<Value>,
    path: PathBuf,
    reconnect: bool,
    capture: bool,
    devices: bool,
    changed: Vec<String>,
    sent_at: Option<std::time::Instant>,
    last_error: String,
    remembered_connection: bool,
}
impl PublicPlayback {
    pub fn new(
        install: PathBuf,
        profile: PathBuf,
        isolated: bool,
        archive: Option<PathBuf>,
    ) -> Self {
        let path = profile.join("data/public-playback.json");
        let connections = Connections::new(&profile, isolated);
        let inherited = connections.playback_settings();
        let saved = fs::read(&path)
            .ok()
            .and_then(|v| serde_json::from_slice::<Value>(&v).ok());
        let mut settings = crate::playback::defaults();
        for (key, value) in settings.as_object_mut().unwrap() {
            if let Some(v) = inherited.get(key).filter(|v| valid_setting(key, v)) {
                *value = v.clone();
            }
        }
        if let Some(saved) = saved {
            for (key, value) in settings.as_object_mut().unwrap() {
                if let Some(v) = saved.get(key).filter(|v| valid_setting(key, v)) {
                    *value = v.clone();
                }
            }
        }
        Self {
            connections,
            player: Player::new(install, isolated.then_some(profile), archive),
            path,
            settings,
            note: "选择主机连接；首次使用会自动准备串流组件。".into(),
            progress: None,
            pending: false,
            target: None,
            reconnect: false,
            capture: false,
            devices: false,
            changed: Vec::new(),
            sent_at: None,
            last_error: String::new(),
            remembered_connection: false,
        }
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.reconnect = false;
        self.capture = false;
        self.devices = false;
        self.pending = false;
        self.changed.clear();
        self.sent_at = None;
        self.player.close()?;
        self.note = "正在断开本次播放……".into();
        Ok(())
    }
    pub fn feedback(&self) -> Value {
        let f = self.player.render_feedback();
        let stats = &f["stats"];
        let pending = self.pending
            || (self.player.running()
                && stats["revision"].as_u64().unwrap_or(0) < self.player.render_revision());
        let up = &self.settings["upscaler"];
        json!({"pendingSettings":if pending{self.changed.clone()}else{Vec::new()},
            "upscaler":{"requested":up,"actual":stats["activeUpscaler"],"state":if stats["upscalerUnavailable"]==true{"error"}else if up!="off" && stats["activeUpscaler"]=="off"{"no-scaling"}else{"active"},"reason":if stats["upscalerUnavailable"]==true{"当前显卡、驱动或运行库无法启用所选算法。"}else{""}},
            "bitrateLock":{"state":if self.settings["lockBitrate"]==true{if self.pending{"pending-reconnect"}else{"locked"}}else{"off"},"targetKbps":self.settings["bitrateKbps"]}})
    }
    pub fn connect(&mut self, args: &Value) -> io::Result<()> {
        if !self.connections.operation.is_empty() {
            return Err(io::Error::other("请等待连接操作完成。"));
        }
        let target = self.connections.target(args)?;
        self.remembered_connection = false;
        self.last_error.clear();
        self.capture = false;
        self.devices = false;
        self.player
            .set_playback_settings(crate::playback::session_settings(&self.settings).to_string())?;
        self.player.set_render_settings(&self.settings)?;
        self.player
            .prepare_stream(self.settings["neuralRendering"] == true)?;
        self.target = Some(target);
        self.note = "正在检查串流组件……".into();
        Ok(())
    }
    pub fn capture(&mut self, args: &Value) -> io::Result<()> {
        let id = args["deviceId"]
            .as_str()
            .ok_or_else(|| io::Error::other("请选择采集设备。"))?;
        self.save(&json!({"captureDeviceId":id,"pictureSource":"capture"}))?;
        self.capture = true;
        self.devices = false;
        if self.player.running() {
            return Ok(());
        }
        self.player
            .set_playback_settings(crate::playback::session_settings(&self.settings).to_string())?;
        self.player.set_render_settings(&self.settings)?;
        self.player.prepare_stream(false)?;
        Ok(())
    }
    pub fn devices(&mut self) -> io::Result<()> {
        if self.player.running() {
            return self.player.send_control(&json!({"type":"devices"}));
        }
        self.devices = true;
        self.capture = false;
        self.player.prepare_stream(false)?;
        self.note = "正在准备设备枚举组件……".into();
        Ok(())
    }
    pub fn unlock(&mut self, args: &Value) -> io::Result<()> {
        let pin = args["pin"]
            .as_str()
            .filter(|pin| pin.len() == 4 && pin.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| io::Error::other("主机解锁码须为 4 位数字。"))?;
        self.player.send_control(&json!({"type":"pin","pin":pin}))
    }
    pub fn save(&mut self, patch: &Value) -> io::Result<()> {
        let object = patch
            .as_object()
            .ok_or_else(|| io::Error::other("设置格式无效。"))?;
        if object.iter().any(|(k, v)| !valid_setting(k, v)) {
            return Err(io::Error::other(
                "当前后端尚未接入此参数；未将不支持的选项标记为生效。",
            ));
        }
        if self.player.preparing() {
            return Err(io::Error::other("请等待组件准备完成或取消后再调整设置。"));
        }
        let mut next = self.settings.clone();
        for (k, v) in object {
            next[k] = v.clone();
        }
        fs::create_dir_all(self.path.parent().unwrap())?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, next.to_string())?;
        fs::rename(tmp, &self.path)?;
        let needs_reconnect = self.player.running()
            && object
                .iter()
                .any(|(k, v)| crate::playback::session_key(k) && self.settings[k] != *v);
        self.changed = object.keys().cloned().collect();
        self.sent_at = Some(std::time::Instant::now());
        self.settings = next;
        self.player.set_render_settings(&self.settings)?;
        self.pending = needs_reconnect;
        if needs_reconnect {
            self.reconnect = true;
            self.player.close()?;
        }
        self.note = if self.pending {
            "正在重新连接以应用串流参数。"
        } else {
            "设置已发送；播放时由渲染器确认，空闲时在连接后应用。"
        }
        .into();
        Ok(())
    }
    pub fn poll(&mut self) -> io::Result<()> {
        match self.player.poll()? {
            Some(Update::Ready) => {
                if self.devices || self.capture {
                    self.player.launch_capture(self.devices)?;
                    self.note = if self.devices {
                        "正在枚举视频设备……"
                    } else {
                        "正在打开采集设备……"
                    }
                    .into();
                    self.progress = None;
                    return Ok(());
                }
                let target = self
                    .target
                    .clone()
                    .ok_or_else(|| io::Error::other("未选择连接主机。"))?;
                self.connections.write_runtime_host(&target)?;
                self.player.launch_stream(
                    &Connections::runtime_nickname(&target),
                    target["ipAddress"].as_str().unwrap_or_default(),
                )?;
                self.pending = false;
                self.progress = None;
                self.note = "正在连接 PS5，等待视频画面……".into();
            }
            Some(Update::Progress(p)) => {
                self.progress = Some(p.done as f32 / p.total as f32);
                self.note = match p.stage {
                    Stage::Waiting => "等待另一实例完成组件准备……".into(),
                    Stage::Download => format!(
                        "正在下载组件：{:.0} / {:.0} MiB",
                        p.done as f64 / 1048576.,
                        p.total as f64 / 1048576.
                    ),
                    Stage::Extract => format!("正在解压：{} / {}", p.done, p.total),
                    Stage::Verify => format!("正在校验：{} / {}", p.done, p.total),
                    Stage::Profile => "正在应用播放设置与手柄修复……".into(),
                    Stage::Ready => "组件已校验。".into(),
                };
            }
            Some(Update::Opened) => {
                self.pending = false;
                self.progress = None;
                self.note = "正在连接 PS5，等待视频画面……".into();
            }
            Some(Update::Closed(status)) => {
                self.progress = None;
                if self.reconnect {
                    self.reconnect = false;
                    if self.capture {
                        let id = self.settings["captureDeviceId"].clone();
                        return self.capture(&json!({"deviceId":id}));
                    }
                    let target = self
                        .target
                        .clone()
                        .ok_or_else(|| io::Error::other("未选择主机。"))?;
                    return self.connect(&target);
                }
                self.note = if status.success() {
                    "本次连接已关闭。".into()
                } else {
                    format!("连接窗口退出：{status}。")
                };
            }
            Some(Update::Cancelled) => {
                self.progress = None;
                self.note = "已取消组件准备。".into();
            }
            None => (),
        }
        let f = self.player.render_feedback();
        if !self.capture && !self.remembered_connection && self.player.video_received() {
            self.remembered_connection = true;
            self.note = "已连接 PS5，正在播放。".into();
            if let Some(target) = &self.target {
                if let Err(e) = self.connections.remember_connected(target) {
                    self.note = format!("画面已连接，但最近主机记录未保存：{e}");
                }
            }
        }
        if let Some(error) = f["error"]["message"].as_str() {
            if error != self.last_error {
                self.last_error = error.to_owned();
                self.note = format!("播放组件：{error}");
            }
        } else if self.player.running() && !self.pending && !self.changed.is_empty() {
            if f["stats"]["revision"].as_u64().unwrap_or(0) >= self.player.render_revision() {
                self.changed.clear();
                self.sent_at = None;
                self.note = "渲染器已处理新视频帧，设置已应用。".into();
            } else if self.sent_at.is_some_and(|t| t.elapsed().as_secs() > 15) {
                self.sent_at = None;
                self.note = "设置尚未获得视频帧确认，请查看运行状态中的算法与硬件信息。".into();
            }
        }
        Ok(())
    }
}

fn valid_setting(key: &str, value: &Value) -> bool {
    crate::playback::valid(key, value)
}
pub fn supported_setting(key: &str) -> bool {
    crate::playback::supported(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_settings_are_bounded_and_never_accept_credentials_or_fake_algorithms() {
        assert!(valid_setting("bitrateKbps", &json!(200000)));
        assert!(!valid_setting("bitrateKbps", &json!(200001)));
        assert!(!valid_setting("bitrateKbps", &json!(-1)));
        assert!(valid_setting("upscaler", &json!("rtx-vsr")));
        assert!(!valid_setting("token", &json!("fixture")));
    }
}
