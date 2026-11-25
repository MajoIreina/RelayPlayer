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
    enhancement_recovery_required: bool,
    wake: Option<crate::wake::WakeTask>,
    gate: Option<StartGate>,
    pub snapshot: Value,
    pub timings: Value,
    connect_began: Option<std::time::Instant>,
}

// The two independent prerequisites may complete in either order.
struct StartGate {
    began: std::time::Instant,
    host_ms: Option<u64>,
    runtime_ms: Option<u64>,
}
impl StartGate {
    fn new() -> Self {
        Self {
            began: std::time::Instant::now(),
            host_ms: None,
            runtime_ms: None,
        }
    }
    fn elapsed_ms(&self) -> u64 {
        self.began.elapsed().as_millis() as u64
    }
    fn ready(&self) -> bool {
        self.host_ms.is_some() && self.runtime_ms.is_some()
    }
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
            enhancement_recovery_required: false,
            wake: None,
            gate: None,
            snapshot: json!({}),
            timings: json!({}),
            connect_began: None,
        }
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.wake = None;
        self.gate = None;
        self.connect_began = None;
        self.target = None;
        self.reconnect = false;
        self.capture = false;
        self.devices = false;
        self.pending = false;
        self.changed.clear();
        self.sent_at = None;
        self.player.close()?;
        self.note = "正在结束播放；已连接的 PS5 将进入待机……".into();
        Ok(())
    }
    pub fn feedback(&self) -> Value {
        let f = &self.snapshot;
        let stats = &f["stats"];
        let pending = self.pending
            || (self.player.running()
                && stats["revision"].as_u64().unwrap_or(0) < self.player.render_revision());
        let up = &self.settings["upscaler"];
        let state = stats["upscalerState"].as_str().unwrap_or("waiting");
        let retry = self.player.running() && !self.pending && !self.reconnect && !self.waking();
        json!({"pendingSettings":if pending{self.changed.clone()}else{Vec::new()},
            "upscaler":{"requested":up,"actual":stats["activeUpscaler"],"state":state,"reason":stats["upscalerReason"],"canRetry":retry && up!="off" && state!="suspended"},
            "frameGeneration":{"canRetry":retry && self.settings["frameGeneration"]==true && stats["frameGenerationReasonCode"]!=4},
            "bitrateLock":{"state":if self.settings["lockBitrate"]==true{if self.pending{"pending-reconnect"}else{"locked"}}else{"off"},"targetKbps":self.settings["bitrateKbps"]}})
    }
    // Restart the isolated renderer rather than re-entering a driver/runtime that failed.
    // The existing reconnect path keeps the console awake and retains its paired target.
    pub fn retry_enhancements(&mut self) -> io::Result<()> {
        if !self.player.running()
            || self.pending
            || self.reconnect
            || self.waking()
            || (self.settings["upscaler"] == "off" && self.settings["frameGeneration"] != true)
        {
            return Err(io::Error::other(
                "请在播放中启用超分或补帧后重试，并等待当前操作完成。",
            ));
        }
        self.player.close_for_reconnect()?;
        self.reconnect = true;
        self.pending = true;
        self.snapshot = json!({});
        self.note = "正在重新连接并重建超分／补帧引擎；PS5 保持开机。".into();
        Ok(())
    }
    pub fn settings_need_reconnect(&self, patch: &Value) -> bool {
        self.player.running()
            && patch.as_object().is_some_and(|o| {
                o.iter().any(|(k, v)| {
                    self.settings[k] != *v
                        && (crate::playback::session_key(k)
                            || (self.enhancement_recovery_required
                                && matches!(
                                    k.as_str(),
                                    "upscaler"
                                        | "upscalerQuality"
                                        | "frameGeneration"
                                        | "frameGenerationBackend"
                                )))
                })
            })
    }
    pub fn connect(&mut self, args: &Value) -> io::Result<()> {
        if !self.connections.operation.is_empty()
            || self.waking()
            || self.player.preparing()
            || self.player.running()
        {
            return Err(io::Error::other("请等待连接操作完成。"));
        }
        self.connect_began = None;
        self.timings = json!({});
        self.snapshot = json!({});
        self.enhancement_recovery_required = false;
        let gate = StartGate::new();
        let target = self.connections.target(args)?;
        let wake = crate::wake::WakeTask::start(&target)?;
        self.player
            .set_playback_settings(crate::playback::session_settings(&self.settings).to_string())?;
        self.player.set_render_settings(&self.settings)?;
        self.player
            .prepare_stream(self.settings["neuralRendering"] == true)?;
        self.gate = Some(gate);
        self.remembered_connection = false;
        self.last_error.clear();
        self.capture = false;
        self.devices = false;
        self.target = Some(target);
        self.wake = Some(wake);
        self.note = "正在检查主机状态；待机时将自动唤醒……".into();
        Ok(())
    }
    pub fn waking(&self) -> bool {
        self.gate.is_some() || self.wake.is_some()
    }
    pub fn capture(&mut self, args: &Value) -> io::Result<()> {
        if self.waking() {
            return Err(io::Error::other("请先停止当前主机唤醒。"));
        }
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
        if self.waking() {
            return Err(io::Error::other("请先停止当前主机唤醒。"));
        }
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
        if self.player.preparing() || self.gate.is_some() {
            return Err(io::Error::other("请等待组件准备完成或取消后再调整设置。"));
        }
        let mut next = self.settings.clone();
        for (k, v) in object {
            next[k] = v.clone();
        }
        if next == self.settings {
            return Ok(());
        }
        fs::create_dir_all(self.path.parent().unwrap())?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, next.to_string())?;
        fs::rename(tmp, &self.path)?;
        let needs_reconnect = self.settings_need_reconnect(patch);
        // Apply to the new process on reconnect. Do not send another algorithm call
        // to the failed runtime while its shutdown is being requested.
        if !needs_reconnect {
            self.player.set_render_settings(&next)?;
        }
        self.settings = next;
        self.changed = object.keys().cloned().collect();
        self.sent_at = Some(std::time::Instant::now());
        self.pending = needs_reconnect;
        if needs_reconnect {
            self.reconnect = true;
            self.player.close_for_reconnect()?;
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
        while let Some(event) = self.wake.as_ref().and_then(|w| w.poll()) {
            use crate::wake::Event;
            match event {
                Event::Checking => self.note = "正在检查主机状态……".into(),
                Event::Waking => self.note = "已请求网络唤醒，等待 PS5 启动并就绪……".into(),
                Event::Waiting => self.note = "主机已开机，等待远程游玩服务就绪……".into(),
                Event::Failed(message) => {
                    self.wake = None;
                    self.gate = None;
                    self.target = None;
                    self.player.close()?;
                    self.progress = None;
                    self.note = message;
                }
                Event::Ready(ip) => {
                    self.wake = None;
                    if let Some(target) = &mut self.target {
                        target["ipAddress"] = json!(ip.to_string());
                    }
                    if let Some(gate) = &mut self.gate {
                        gate.host_ms = Some(gate.elapsed_ms());
                    }
                    self.note = "主机已就绪，正在检查串流组件……".into();
                }
            }
        }
        let update = match self.player.poll() {
            Ok(update) => update,
            Err(error) => {
                self.wake = None;
                self.gate = None;
                self.target = None;
                self.progress = None;
                return Err(error);
            }
        };
        match update {
            Some(Update::Ready) => {
                self.enhancement_recovery_required = false;
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
                if let Some(gate) = &mut self.gate {
                    gate.runtime_ms = Some(gate.elapsed_ms());
                }
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
                self.wake = None;
                self.gate = None;
                self.target = None;
                self.progress = None;
                self.note = "已取消组件准备。".into();
            }
            None => (),
        }
        if self.gate.as_ref().is_some_and(StartGate::ready) {
            let gate = self.gate.take().unwrap();
            if let Some(target) = &self.target {
                let launch = self.connections.write_runtime_host(target).and_then(|_| {
                    self.player.launch_stream(
                        &Connections::runtime_nickname(target),
                        target["ipAddress"].as_str().unwrap_or_default(),
                    )
                });
                if let Err(error) = launch {
                    self.target = None;
                    self.progress = None;
                    self.player.close()?;
                    return Err(error);
                }
                self.pending = false;
                self.progress = None;
                self.note = "正在连接 PS5，等待视频画面……".into();
            }
            self.timings = json!({"hostReadyMs":gate.host_ms,"runtimeReadyMs":gate.runtime_ms,"launchMs":gate.elapsed_ms()});
            self.connect_began = Some(gate.began);
        }
        self.snapshot = self.player.render_feedback();
        let f = &self.snapshot;
        self.enhancement_recovery_required |= f["stats"]["upscalerState"] == "error"
            || matches!(
                f["stats"]["frameGenerationReasonCode"].as_i64(),
                Some(2 | 5 | 6)
            );
        if !self.capture && !self.remembered_connection && self.player.video_received() {
            self.remembered_connection = true;
            if let Some(began) = self.connect_began.take() {
                self.timings["firstVideoSignalMs"] = json!(began.elapsed().as_millis() as u64);
            }
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
    fn stop_discards_a_late_wakeup_result_before_any_engine_is_prepared() {
        let root =
            std::env::temp_dir().join(format!("Relay-Wake-Cancel-QA-{}", std::process::id()));
        let mut p = PublicPlayback::new(root.clone(), root, true, None);
        p.target = Some(json!({"hostId":"020000000001","ipAddress":"127.0.0.1"}));
        p.wake = Some(crate::wake::WakeTask::ready_fixture());
        let mut gate = StartGate::new();
        gate.runtime_ms = Some(1);
        p.gate = Some(gate);
        assert!(p.waking());
        p.stop().unwrap();
        p.poll().unwrap();
        assert!(!p.waking() && !p.player.preparing() && !p.player.running());
        assert!(p.target.is_none());
    }
    #[test]
    fn old_picture_settings_load_with_neutral_new_controls_and_noop_save_writes_nothing() {
        let root =
            std::env::temp_dir().join(format!("Relay-Picture-017-QA-{}", std::process::id()));
        let path = root.join("data/public-playback.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = br#"{"saturation":1.13,"gamma":1.05,"upscaler":"artcnn"}"#;
        fs::write(&path, old).unwrap();
        let mut p = PublicPlayback::new(root.clone(), root.clone(), true, None);
        assert_eq!(p.settings["saturation"], 1.13);
        assert_eq!(p.settings["upscaler"], "artcnn");
        assert_eq!(p.settings["exposure"], 0.0);
        assert_eq!(p.settings["brightness"], 0.0);
        let time = fs::metadata(&path).unwrap().modified().unwrap();
        p.save(&json!({"saturation":1.13})).unwrap();
        assert_eq!(fs::read(&path).unwrap(), old);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), time);
        let mut gate = StartGate::new();
        gate.runtime_ms = Some(1);
        p.gate = Some(gate);
        assert!(p.save(&json!({"fps":30})).is_err());
        p.stop().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn connection_gate_never_launches_with_only_one_prerequisite() {
        for host_first in [true, false] {
            let mut gate = StartGate::new();
            assert!(!gate.ready());
            if host_first {
                gate.host_ms = Some(1);
            } else {
                gate.runtime_ms = Some(1);
            }
            assert!(!gate.ready());
            gate.host_ms = Some(2);
            gate.runtime_ms = Some(3);
            assert!(gate.ready());
        }
    }
    #[test]
    fn feedback_distinguishes_hdr_suspension_failure_and_missing_frames() {
        let root =
            std::env::temp_dir().join(format!("Relay-Enhancement-QA-{}", std::process::id()));
        let mut p = PublicPlayback::new(root.clone(), root, true, None);
        p.settings["upscaler"] = json!("rtx-vsr");
        assert_eq!(p.feedback()["upscaler"]["state"], "waiting");
        for state in ["suspended", "error", "no-scaling", "active"] {
            p.snapshot = json!({"stats":{"upscalerState":state,"upscalerReason":"fixture reason","activeUpscaler":"off"}});
            let feedback = p.feedback();
            assert_eq!(feedback["upscaler"]["state"], state);
            assert_eq!(feedback["upscaler"]["reason"], "fixture reason");
            assert_eq!(feedback["upscaler"]["canRetry"], false);
        }
        let before = p.settings.clone();
        assert!(p.retry_enhancements().is_err());
        assert_eq!(p.settings, before);
        assert!(!p.pending && !p.reconnect);
    }
    #[test]
    fn playback_settings_are_bounded_and_never_accept_credentials_or_fake_algorithms() {
        for (key, lo, hi) in [("exposure", -3.0, 3.0), ("brightness", -1.0, 1.0)] {
            assert!(
                valid_setting(key, &json!(0))
                    && valid_setting(key, &json!(lo))
                    && valid_setting(key, &json!(hi))
            );
            assert!(
                !valid_setting(key, &json!(lo - 0.01)) && !valid_setting(key, &json!(hi + 0.01))
            );
        }
        assert!(valid_setting("bitrateKbps", &json!(200000)));
        assert!(!valid_setting("bitrateKbps", &json!(200001)));
        assert!(!valid_setting("bitrateKbps", &json!(-1)));
        assert!(valid_setting("upscaler", &json!("rtx-vsr")));
        assert!(!valid_setting("token", &json!("fixture")));
    }
}
