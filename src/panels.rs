//! Fluent settings surfaces. Hardware actions are explicit; enumerating devices never starts one.
use egui::{self, RichText, Ui};
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Connect,
    Capture,
    Picture,
    Controls,
    Diagnostics,
}

type Actions = Vec<(String, Value)>;

#[derive(Default)]
pub struct Panels {
    remote_tab: bool,
    ip: String,
    account_id: String,
    manual_account: bool,
    auth_started: bool,
    pin: String,
    ps4: bool,
    pair_open: bool,
    paired: String,
    remove_prompt: bool,
    lan_hosts: Option<Vec<Value>>,
    remote_hosts: Option<Vec<Value>>,
    credential: String,
    duid: String,
    remote_name: String,
    signout_prompt: bool,
    unlock_pin: String,
    selected_capture: Option<String>,
    pub nr_request: Option<u64>,
    pub upscaler_request: Option<u64>,
    // Only keys the user has changed are staged. Applying a camera setting must never
    // reset a quick control changed on the playback bar, or edits on another page.
    draft: Map<String, Value>,
}

fn strval<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}
fn items(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn busy(s: &Value) -> bool {
    !strval(s, "operation").is_empty()
}
fn can_play(s: &Value) -> bool {
    !busy(s) && s["ready"].as_bool() == Some(true)
}
// JavaScript normalizes numbers when serializing a settings acknowledgement (1.0 → 1).
// All numeric settings are small bounded values; compare their value, not their JSON spelling.
fn setting_matches(saved: Option<&Value>, edited: &Value) -> bool {
    saved.is_some_and(|v| {
        v == edited || matches!((v.as_f64(), edited.as_f64()), (Some(a), Some(b)) if a == b)
    })
}
fn is_camera(d: &Value) -> bool {
    if d["likelyWebcam"].as_bool() == Some(true) {
        return true;
    }
    let name = strval(d, "name").to_lowercase();
    [
        "webcam",
        "virtual camera",
        "virtual cam",
        "摄像头",
        "攝影機",
        "網路攝影",
    ]
    .iter()
    .any(|hint| name.contains(hint))
}
fn playing(s: &Value) -> bool {
    matches!(
        s["status"]["phase"].as_str(),
        Some("connecting" | "streaming" | "reconnecting" | "pin-required" | "waking")
    )
}
fn cmd(out: &mut Actions, name: &str, args: Value) {
    out.push((name.to_owned(), args));
}
fn title(ui: &mut Ui, text: &str) {
    ui.add_space(5.0);
    ui.label(RichText::new(text).strong().size(14.0));
    ui.add_space(3.0);
}
fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).small().weak());
}
fn divider(ui: &mut Ui) {
    ui.add_space(7.0);
    ui.separator();
    ui.add_space(5.0);
}
fn btn(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::new(text)).clicked()
}
fn main_btn(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    let fill = ui.visuals().selection.bg_fill;
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).strong()).fill(fill),
    )
    .clicked()
}
fn edit(
    ui: &mut Ui,
    label: &str,
    value: &mut String,
    placeholder: &str,
    secret: bool,
    length: usize,
) {
    let caption = ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .hint_text(placeholder)
            .password(secret)
            .char_limit(length),
    )
    .labelled_by(caption.id);
}
fn row(ui: &mut Ui, label: &str, right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let width = (ui.available_width() * 0.38).clamp(56., 132.);
        ui.allocate_ui_with_layout(
            egui::vec2(width, 22.),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(label);
            },
        );
        right(ui);
    });
}
fn choose(
    ui: &mut Ui,
    id: &str,
    current: &str,
    options: &[(String, String)],
    empty: &str,
) -> Option<String> {
    let display = options
        .iter()
        .find(|(v, _)| v == current)
        .map(|(_, x)| x.as_str())
        .unwrap_or(empty);
    let mut choice = None;
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width().clamp(45., 450.))
        .height(235.)
        .truncate()
        .selected_text(display)
        .show_ui(ui, |ui| {
            for (v, label) in options {
                if ui.selectable_label(v == current, label).clicked() {
                    choice = Some(v.clone());
                }
            }
        });
    choice
}
fn literal_options(opts: &[(&str, &str)]) -> Vec<(String, String)> {
    opts.iter()
        .map(|(k, t)| (k.to_string(), t.to_string()))
        .collect()
}
fn page_key(tab: Tab, k: &str) -> bool {
    match tab {
        Tab::Capture => k.starts_with("capture"),
        Tab::Controls => matches!(
            k,
            "hapticsTier"
                | "adaptiveTriggers"
                | "keyboardMouseEnabled"
                | "mouseSensitivity"
                | "mouseInvertY"
        ),
        Tab::Picture => !page_key(Tab::Capture, k) && !page_key(Tab::Controls, k),
        _ => false,
    }
}
fn short_stat(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Bool(true) => "开启".into(),
        Value::Bool(false) => "关闭".into(),
        Value::Number(n) => n.to_string(),
        _ => "—".into(),
    }
}
fn bitrate_mbps(kbps: f64) -> String {
    let mut value = format!("{:.3}", kbps / 1000.0);
    while value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.pop();
    }
    format!("{value} Mbps")
}
fn bitrate_lock_message(feedback: &Value) -> Option<String> {
    let target = feedback["targetKbps"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.)
        .map(bitrate_mbps);
    match strval(feedback, "state") {
        "locked" => Some(format!(
            "本地目标已锁定：{}。PS5 实际码率仍由主机与网络决定。",
            target.as_deref().unwrap_or("待引擎确认")
        )),
        "pending-reconnect" => Some(format!(
            "本地目标已锁定：{}；下次连接时发送。PS5 实际码率仍由主机与网络决定。",
            target.as_deref().unwrap_or("待引擎确认")
        )),
        "pending-confirmation" => Some(format!(
            "目标 {} 已发送，等待引擎确认码率与锁定状态。",
            target.as_deref().unwrap_or("待确认")
        )),
        "fallback" => Some(format!(
            "码率锁定回退：{}。{}",
            target.as_deref().unwrap_or("目标待引擎确认"),
            strval(feedback, "reason")
        )),
        _ => None,
    }
}
// In the playback protocol, width/height/fps refer to the video presented to
// the processor; capture* describe the capture device, output* the processed
// video. A target rate, display refresh, or saved input mode is not a measurement.
fn live_stats(st: &Value) -> &Value {
    st.get("stats")
        .filter(|v| v.is_object())
        .unwrap_or(&st["session"]["stats"])
}
fn capture_only(st: &Value, stats: &Value) -> bool {
    [
        &st["captureOnly"],
        &st["session"]["captureOnly"],
        &stats["captureOnly"],
    ]
    .iter()
    .any(|v| v.as_bool() == Some(true))
}
fn session_source(st: &Value, stats: &Value) -> (&'static str, bool) {
    if capture_only(st, stats) {
        return ("采集卡（仅采集）", true);
    }
    match strval(stats, "pictureSource") {
        "capture" => ("采集卡画面 + 网络控制", true),
        "stream" => ("网络串流", false),
        _ => ("正在识别", false),
    }
}
fn measured_mode(v: &Value, w: &str, h: &str, rate: &str, unit: &str) -> Option<String> {
    fn dimension(v: &Value) -> Option<u32> {
        v.as_f64()
            .filter(|n| *n >= 1. && *n <= f64::from(u32::MAX) && n.fract() == 0.)
            .map(|n| n as u32)
    }
    let (w, h) = (dimension(&v[w])?, dimension(&v[h])?);
    let mut mode = format!("{w} × {h}");
    if let Some(n) = v[rate].as_f64().filter(|n| n.is_finite() && *n > 0.) {
        let f = if n < 0.01 {
            "< 0.01".to_owned()
        } else {
            format!("{n:.2}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned()
        };
        mode.push_str(&format!(" · {f} {unit}"));
    }
    Some(mode)
}
fn nr_reason(reason: &str) -> String {
    let label = match reason {
        "driver-fault" => "驱动触发引擎保护",
        "driver-outdated" => "当前驱动不支持所需的神经渲染功能",
        "runtime-missing" => "缺少神经渲染运行库",
        "hdr" => "当前运行库无法处理此 HDR 输入",
        "gpu-unsupported" => "当前显卡不受支持",
        _ => return reason.to_owned(),
    };
    format!("{label}（{reason}）")
}
fn nr_frame_state(st: &Value, stats: &Value, gpu: &Value, requested: bool) -> Option<String> {
    if strval(st, "phase") != "streaming" {
        return None;
    }
    match strval(&st["renderFeedback"]["nrGuard"], "state") {
        "suspended" => return Some("保护暂停".into()),
        "error" => return Some("保护操作未获引擎确认".into()),
        "unconfirmed" => return Some("引擎未确认 NR 生效".into()),
        _ => {}
    }
    if st["renderFeedback"]["nrWaitingForFrame"].as_bool() == Some(true)
        || stats["recovering"].as_bool() == Some(true)
        || ["width", "height"]
            .iter()
            .any(|key| !stats[*key].as_f64().is_some_and(|n| n >= 1.))
    {
        return Some("等待新视频帧".into());
    }
    let value = strval(stats, "neuralRendering").trim();
    let name = match value {
        "on" if gpu["dlssNrSupported"].as_bool() == Some(false) => {
            "帧状态与当前可用性不一致，未确认生效"
        }
        "on" if !strval(stats, "neuralRenderingReason").trim().is_empty() => {
            "引擎同时报告了状态原因，尚不能确认已生效"
        }
        "on" => "生效",
        "unavailable" => "未生效",
        "preparing" => "准备中，尚未确认生效",
        "off" if requested => "未生效（引擎关闭）",
        "off" => "关闭",
        "" => return None,
        other => return Some(format!("引擎回报：{other}（未确认生效）")),
    };
    Some(name.into())
}
fn upscaler_name(raw: &str) -> &str {
    match raw {
        "off" => "关闭",
        "fsr1" => "AMD FSR 1",
        "rtx-vsr" => "NVIDIA RTX 视频超分",
        "artcnn" => "ArtCNN",
        "integer" => "整数缩放",
        "vt-sr" => "Apple 视频超分",
        _ => raw,
    }
}
fn upscaler_frame_state(st: &Value, stats: &Value, requested: &str) -> Option<String> {
    if strval(st, "phase") != "streaming"
        || stats["recovering"].as_bool() == Some(true)
        || ["width", "height"]
            .iter()
            .any(|key| !stats[*key].as_f64().is_some_and(|n| n >= 1.))
    {
        return None;
    }
    let feedback = &st["renderFeedback"]["upscaler"];
    let state = strval(feedback, "state");
    if matches!(state, "idle" | "waiting" | "waiting-for-frame")
        || (!strval(feedback, "requested").is_empty() && strval(feedback, "requested") != requested)
        || stats["requestedUpscaler"]
            .as_str()
            .is_some_and(|actual| actual != requested)
    {
        return None;
    }
    let reason = strval(feedback, "reason").trim();
    if matches!(state, "suspended" | "error" | "no-scaling") {
        let message = match state {
            "suspended" => "所选超分已暂停",
            "error" => "引擎未确认超分保护操作",
            _ => "当前画面无需放大",
        };
        return Some(if reason.is_empty() {
            message.to_owned()
        } else {
            format!("{message}：{reason}")
        });
    }
    let actual = match strval(feedback, "actual") {
        "" => strval(stats, "activeUpscaler"),
        actual => actual,
    };
    if actual.is_empty() {
        return None;
    }
    let actual_name = upscaler_name(actual);
    let mut message = if actual == requested {
        format!("实际使用：{actual_name}")
    } else if actual == "off" {
        format!("已选择 {}，当前实际未启用超分", upscaler_name(requested))
    } else {
        format!(
            "已选择 {}，当前实际使用：{actual_name}",
            upscaler_name(requested)
        )
    };
    if !reason.is_empty() {
        message.push_str("。 ");
        message.push_str(reason);
    }
    Some(message)
}
fn pending_session_settings(st: &Value) -> Option<String> {
    let keys = st["renderFeedback"]["pendingSettings"].as_array()?;
    if keys.is_empty() {
        return None;
    }
    let labels = keys
        .iter()
        .filter_map(Value::as_str)
        .map(|key| match key {
            "resolution" => "串流分辨率",
            "fps" => "串流帧率",
            "codec" => "编码格式",
            "hdr" => "HDR",
            "networkAdapter" => "网络适配器",
            "pictureSource" => "画面来源",
            "captureDeviceId" => "采集设备",
            "captureWidth"
            | "captureHeight"
            | "captureFps"
            | "captureFormat"
            | "captureHdrMode"
            | "captureAudioDeviceId" => "采集参数",
            _ => "其他会话参数",
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    (!labels.is_empty())
        .then(|| format!("已保存；当前会话需断开并重连后生效：{}", labels.join("、")))
}
fn valid_account_id(s: &str) -> bool {
    let s = s.trim();
    (!s.is_empty() && s.as_bytes().iter().all(|b| b.is_ascii_digit()) && s.parse::<u64>().is_ok())
        || (s.len() == 12
            && s.ends_with('=')
            && b"AEIMQUYcgkosw048".contains(&s.as_bytes()[10])
            && s.as_bytes()[..10]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+/".contains(b)))
}

impl Panels {
    pub fn track_request(&mut self, name: &str, args: &Value, id: u64) {
        if name == "settings" && args.get("neuralRendering").is_some() {
            self.nr_request = Some(id);
        }
        if (name == "settings" && args.get("upscaler").is_some()) || name == "retry-upscaler" {
            self.upscaler_request = Some(id);
        }
    }
    pub fn finish_request(&mut self, id: u64) {
        if self.nr_request == Some(id) {
            self.nr_request = None;
        }
        if self.upscaler_request == Some(id) {
            self.upscaler_request = None;
        }
    }
    pub fn clear_requests(&mut self) {
        self.nr_request = None;
        self.upscaler_request = None;
    }
    pub fn on_result(&mut self, name: &str, result: &Value) {
        match name {
            "discover" => self.lan_hosts = Some(items(&result["consoles"]).to_vec()),
            "psn-consoles" => self.remote_hosts = Some(items(result).to_vec()),
            "sign-in" => {
                self.credential.clear();
                self.remote_hosts = None;
                self.duid.clear();
                self.remote_name.clear();
                self.account_id.clear();
                self.manual_account = false;
                self.auth_started = false;
            }
            "sign-out" => {
                self.manual_account = false;
                self.auth_started = false;
                self.credential.clear();
                self.account_id.clear();
                self.remote_hosts = None;
                self.duid.clear();
                self.remote_name.clear();
            }
            "pair" => {
                self.pin.clear();
                self.paired = strval(result, "hostId").to_owned();
                self.pair_open = false;
            }
            "remove" => {
                self.paired.clear();
                self.remove_prompt = false;
            }
            "pin" => self.unlock_pin.clear(),
            "settings" => self.draft.retain(|k, v| !setting_matches(result.get(k), v)),
            _ => {}
        }
    }
    pub fn render(&mut self, ui: &mut Ui, tab: Tab, state: &Value) -> Actions {
        let mut out = Vec::new();
        ui.spacing_mut().item_spacing = egui::vec2(8., 6.);
        ui.push_id(tab, |ui| match tab {
            Tab::Connect => self.connect(ui, state, &mut out),
            Tab::Capture => self.capture(ui, state, &mut out),
            Tab::Picture => self.picture(ui, state, &mut out),
            Tab::Controls => self.controls(ui, state, &mut out),
            Tab::Diagnostics => self.diagnostics(ui, state),
        });
        out
    }
    fn get<'a>(&'a self, s: &'a Value, k: &str) -> &'a Value {
        self.draft.get(k).unwrap_or(&s["settings"][k])
    }
    fn stage(&mut self, s: &Value, k: &str, v: Value) {
        if setting_matches(s["settings"].get(k), &v) {
            self.draft.remove(k);
        } else {
            self.draft.insert(k.into(), v);
        }
    }
    fn patch(&self, tab: Tab) -> Map<String, Value> {
        self.draft
            .iter()
            .filter(|(k, _)| page_key(tab, k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
    fn apply(&mut self, ui: &mut Ui, tab: Tab, s: &Value, out: &mut Actions) {
        let patch = self.patch(tab);
        let changed = !patch.is_empty();
        ui.add_space(4.);
        ui.horizontal_wrapped(|ui| {
            if main_btn(ui, "应用设置", changed && can_play(s)) {
                cmd(out, "settings", Value::Object(patch));
            }
            if btn(ui, "撤销更改", changed && !busy(s)) {
                self.draft.retain(|k, _| !page_key(tab, k));
                if tab == Tab::Capture {
                    self.selected_capture = None;
                }
            }
            if changed {
                note(ui, "有未应用的更改");
            }
        });
        if let Some(message) = pending_session_settings(&s["status"]) {
            note(ui, message);
        }
    }
    fn setting_choice(
        &mut self,
        ui: &mut Ui,
        s: &Value,
        k: &str,
        label: &str,
        opts: &[(&str, &str)],
    ) {
        let cur = self.get(s, k);
        let numeric = cur.is_number();
        let text = if numeric {
            cur.to_string()
        } else {
            cur.as_str().unwrap_or("").to_string()
        };
        row(ui, label, |ui| {
            if let Some(v) = choose(ui, k, &text, &literal_options(opts), "请选择") {
                self.stage(
                    s,
                    k,
                    if numeric {
                        json!(v.parse::<i64>().unwrap_or(0))
                    } else {
                        Value::String(v)
                    },
                );
            }
        });
    }
    fn check(&mut self, ui: &mut Ui, s: &Value, k: &str, label: &str) {
        let mut v = self.get(s, k).as_bool().unwrap_or(false);
        if ui.checkbox(&mut v, label).changed() {
            self.stage(s, k, json!(v));
        }
    }
    fn slider(
        &mut self,
        ui: &mut Ui,
        s: &Value,
        k: &str,
        label: &str,
        min: f64,
        max: f64,
        step: f64,
    ) {
        let mut v = self.get(s, k).as_f64().unwrap_or(min);
        row(ui, label, |ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 49.).clamp(40., 220.);
            if ui
                .add(
                    egui::Slider::new(&mut v, min..=max)
                        .step_by(step)
                        .max_decimals(if step == 1. { 0 } else { 2 }),
                )
                .changed()
            {
                self.stage(
                    s,
                    k,
                    if step == 1. {
                        json!(v.round() as i64)
                    } else {
                        json!((v * 100.).round() / 100.)
                    },
                );
            }
        });
    }
    fn number(
        &mut self,
        ui: &mut Ui,
        s: &Value,
        k: &str,
        label: &str,
        min: i64,
        max: i64,
        speed: f64,
        unit: &str,
    ) {
        let mut v = self
            .get(s, k)
            .as_i64()
            .unwrap_or_else(|| self.get(s, k).as_f64().unwrap_or(0.) as i64);
        row(ui, label, |ui| {
            if ui
                .add(
                    egui::DragValue::new(&mut v)
                        .range(min..=max)
                        .speed(speed)
                        .suffix(unit),
                )
                .changed()
            {
                self.stage(s, k, json!(v));
            }
        });
    }
    fn bitrate(&mut self, ui: &mut Ui, s: &Value) -> egui::Response {
        let kbps = self
            .get(s, "bitrateKbps")
            .as_f64()
            .filter(|n| n.is_finite())
            .unwrap_or(30_000.);
        let mut mbps = kbps / 1000.;
        let label = "目标码率（2–200 Mbps）";
        let response = ui
            .horizontal_wrapped(|ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 124.).clamp(40., 200.);
                let response = ui.add(
                    egui::Slider::new(&mut mbps, 2.0..=200.0)
                        .step_by(1.)
                        .integer()
                        .show_value(false)
                        .text(label)
                        .clamping(egui::SliderClamping::Edits),
                );
                ui.label(if response.changed() {
                    format!("{} Mbps", mbps.round() as i64)
                } else {
                    bitrate_mbps(kbps)
                });
                response
            })
            .inner;
        if response.changed() {
            self.stage(s, "bitrateKbps", json!(mbps.round() as i64 * 1000));
        }
        response
    }
    fn connect(&mut self, ui: &mut Ui, s: &Value, out: &mut Actions) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.remote_tab, false, "局域网");
            ui.selectable_value(&mut self.remote_tab, true, "PSN 异地");
        });
        divider(ui);
        if self.remote_tab {
            self.remote(ui, s, out);
            return;
        }
        let paired = items(&s["consoles"]);
        if paired.is_empty() {
            self.paired.clear();
            self.remove_prompt = false;
        } else if !paired.iter().any(|h| strval(h, "hostId") == self.paired) {
            self.paired = strval(&paired[0], "hostId").to_owned();
            self.remove_prompt = false;
        }
        let current = paired.iter().find(|c| strval(c, "hostId") == self.paired);
        title(ui, "已配对主机");
        if let Some(c) = current {
            let opts = paired
                .iter()
                .map(|v| {
                    (
                        strval(v, "hostId").to_owned(),
                        format!("{} · {}", strval(v, "hostName"), strval(v, "ipAddress")),
                    )
                })
                .collect::<Vec<_>>();
            if let Some(id) = choose(ui, "stored-host", &self.paired, &opts, "选择主机") {
                self.paired = id;
                self.remove_prompt = false;
            }
            let args = json!({"hostId":self.paired});
            ui.horizontal_wrapped(|ui| {
                if main_btn(ui, "开始串流", can_play(s)) {
                    cmd(out, "connect", args.clone());
                }
                if btn(ui, "唤醒", can_play(s)) {
                    cmd(out, "wake", args.clone());
                }
                if btn(ui, "移除…", !busy(s)) {
                    self.remove_prompt = !self.remove_prompt;
                }
            });
            if self.remove_prompt {
                note(
                    ui,
                    format!("移除「{}」后需要重新配对。", strval(c, "hostName")),
                );
                ui.horizontal(|ui| {
                    if btn(ui, "确认移除", !busy(s)) {
                        cmd(out, "remove", args.clone());
                        self.remove_prompt = false;
                    }
                    if ui.button("取消").clicked() {
                        self.remove_prompt = false;
                    }
                });
            }
            egui::CollapsingHeader::new("使用其他连接地址")
                .id_salt("override")
                .show(ui, |ui| {
                    edit(
                        ui,
                        "主机地址",
                        &mut self.ip,
                        "输入主机可达的 IP 或域名",
                        false,
                        253,
                    );
                    if btn(
                        ui,
                        "按此地址连接",
                        can_play(s) && !self.ip.trim().is_empty(),
                    ) {
                        cmd(
                            out,
                            "connect",
                            json!({"hostId":self.paired,"ipAddress":self.ip.trim()}),
                        );
                    }
                });
            divider(ui);
            if ui
                .selectable_label(self.pair_open, "添加另一台主机")
                .clicked()
            {
                self.pair_open = !self.pair_open;
            }
        } else {
            note(ui, "还没有配对记录。先在 PS5 上启用遥控游玩，再进行配对。");
        }
        if paired.is_empty() || self.pair_open {
            title(ui, "发现与配对");
            if btn(ui, "搜索局域网主机", can_play(s)) {
                cmd(out, "discover", json!({}));
            }
            if let Some(hosts) = &self.lan_hosts {
                if hosts.is_empty() {
                    note(ui, "没有发现主机。可以手动填写 IP 地址。");
                }
                for c in hosts.iter().take(24) {
                    let ip = strval(c, "ipAddress");
                    if !ip.is_empty()
                        && ui
                            .selectable_label(
                                self.ip == ip,
                                format!("{} · {}", strval(c, "hostName"), ip),
                            )
                            .clicked()
                    {
                        self.ip = ip.into();
                        self.ps4 = strval(c, "hostType") == "PS4";
                    }
                }
            }
            edit(
                ui,
                "主机地址",
                &mut self.ip,
                "输入 PS5 的局域网 IP 地址",
                false,
                253,
            );
            ui.horizontal(|ui| {
                ui.label("型号");
                ui.selectable_value(&mut self.ps4, false, "PS5");
                ui.selectable_value(&mut self.ps4, true, "PS4");
            });
            edit(
                ui,
                "8 位配对码",
                &mut self.pin,
                if self.ps4 {
                    "设定 → 遥控操作连接设定 → 添加设备"
                } else {
                    "设定 → 系统 → 远程游玩 → 关联设备"
                },
                true,
                8,
            );
            self.pairing_account(ui, s, out);
            let args = self.pairing_args(s);
            if main_btn(ui, "配对主机", can_play(s) && args.is_ok()) {
                if let Ok(args) = args {
                    cmd(out, "pair", args);
                }
            } else if let Err(why) = args {
                note(ui, why);
            }
        }
    }
    fn browser_auth(&mut self, ui: &mut Ui, s: &Value, out: &mut Actions, lan: bool) {
        if btn(ui, "在浏览器登录 Sony", can_play(s)) {
            self.auth_started = true;
            cmd(out, "open-auth", json!({}));
        }
        if !lan || self.auth_started {
            edit(
                ui,
                "授权结果",
                &mut self.credential,
                "粘贴登录后浏览器地址栏中的完整回跳网址",
                true,
                4096,
            );
            if main_btn(
                ui,
                "完成授权",
                !self.credential.trim().is_empty() && can_play(s),
            ) {
                cmd(out, "sign-in", json!({"credential":self.credential.trim()}));
                self.credential.clear();
            }
        }
    }
    fn account_details(ui: &mut Ui, acct: &Value) {
        egui::CollapsingHeader::new("查看账号 ID")
            .id_salt("account-details")
            .show(ui, |ui| {
                for (label, key) in [("数字 ID", "userId"), ("Base64", "accountIdBase64")] {
                    let value = strval(acct, key);
                    if !value.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            ui.add(
                                egui::Label::new(RichText::new(value).monospace()).selectable(true),
                            );
                        });
                    }
                }
            });
    }
    fn pairing_account(&mut self, ui: &mut Ui, s: &Value, out: &mut Actions) {
        let account = &s["account"];
        let signed_in = valid_account_id(strval(account, "accountIdBase64"));
        if self.manual_account {
            edit(
                ui,
                "PSN 账号 ID",
                &mut self.account_id,
                "数字或 Base64 格式，不是在线昵称",
                false,
                32,
            );
            if ui
                .link(if signed_in {
                    "使用已登录账号"
                } else {
                    "改用 Sony 登录"
                })
                .clicked()
            {
                self.manual_account = false;
            }
        } else {
            if signed_in {
                let name = strval(account, "onlineId");
                ui.label(if name.is_empty() {
                    "配对账号：已登录 Sony".to_owned()
                } else {
                    format!("配对账号：{name}")
                });
                Self::account_details(ui, account);
            } else {
                note(ui, "首次配对需要确认主机上的用户。用同一账号登录 Sony 即可，不用查 ID；画面仍通过局域网传输。");
                self.browser_auth(ui, s, out, true);
            }
            if ui.link("手动输入账号 ID").clicked() {
                self.manual_account = true;
                self.credential.clear();
                self.auth_started = false;
            }
        }
    }
    // A manual ID is sent only when the user explicitly chose that path. Otherwise
    // the backend can use the signed-in identity it already holds.
    fn pairing_args(&self, s: &Value) -> Result<Value, &'static str> {
        if self.ip.trim().is_empty() {
            return Err("先选择主机或填写主机地址。");
        }
        if self.pin.len() != 8 || !self.pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err("请输入主机当前显示的 8 位配对码。");
        }
        let mut args = json!({"ipAddress":self.ip.trim(),"pin":self.pin,"hostType":if self.ps4 {"PS4"} else {"PS5"}});
        if self.manual_account {
            let id = self.account_id.trim();
            if !valid_account_id(id) {
                return Err("手动配对需填写账号的数字 ID 或 Base64 ID。");
            }
            args["accountId"] = json!(id);
        } else if !valid_account_id(strval(&s["account"], "accountIdBase64")) {
            return Err("请先完成 Sony 授权，或选择手动输入账号 ID。");
        }
        Ok(args)
    }
    fn remote(&mut self, ui: &mut Ui, s: &Value, out: &mut Actions) {
        title(ui, "PlayStation 账号");
        let acct = &s["account"];
        if acct.is_object() {
            ui.label(strval(acct, "onlineId"));
            Self::account_details(ui, acct);
            note(
                ui,
                if acct["remoteAvailable"].as_bool() == Some(true) {
                    "已获得远程连接授权"
                } else {
                    "当前没有远程授权，请重新登录"
                },
            );
            if btn(ui, "退出账号…", !busy(s)) {
                self.signout_prompt = !self.signout_prompt;
            }
            if self.signout_prompt {
                note(ui, "退出会断开当前会话并清除本地 PSN 授权。");
                ui.horizontal(|ui| {
                    if btn(ui, "确认退出", !busy(s)) {
                        cmd(out, "sign-out", json!({}));
                        self.signout_prompt = false;
                    }
                    if ui.button("取消").clicked() {
                        self.signout_prompt = false;
                    }
                });
            }
        } else {
            note(
                ui,
                "在 Sony 页面登录，再将 Remote Play 回跳网址粘贴到下方。",
            );
            self.browser_auth(ui, s, out, false);
            egui::CollapsingHeader::new("需要自动读取账号的主机列表？").id_salt("credential-options").show(ui,|ui|{
                note(ui,"Sony 登录后，可使用 Sony 返回的 npsso 代替回跳网址；回跳授权可直接填写 DUID。");if btn(ui,"打开 Sony 凭据页面",can_play(s)){cmd(out,"open-cookie",json!({}));}
            });
        }
        divider(ui);
        title(ui, "远程主机");
        let library = acct["libraryAvailable"].as_bool() == Some(true);
        if btn(ui, "读取 PSN 主机列表", can_play(s) && library) {
            cmd(out, "psn-consoles", json!({}));
        }
        if acct.is_object() && !library {
            note(
                ui,
                "当前授权不能读取主机列表。若已知 DUID，可在下方手动连接。",
            );
        }
        if let Some(hosts) = &self.remote_hosts {
            if hosts.is_empty() {
                note(ui, "此账号没有返回主机。可以手动填写 DUID。");
            }
            for c in hosts.iter().take(24) {
                let id = strval(c, "duid");
                let name = strval(c, "name");
                if ui.selectable_label(self.duid == id, name).clicked() {
                    self.duid = id.into();
                    self.remote_name = name.into();
                }
            }
        }
        let before = self.duid.clone();
        edit(
            ui,
            "主机 DUID",
            &mut self.duid,
            "64 位十六进制 ID",
            false,
            64,
        );
        if before != self.duid {
            self.remote_name.clear();
        }
        let valid =
            self.duid.len() == 64 && self.duid.as_bytes().iter().all(|b| b.is_ascii_hexdigit());
        if main_btn(
            ui,
            "远程连接",
            can_play(s) && valid && acct["remoteAvailable"].as_bool() == Some(true),
        ) {
            cmd(
                out,
                "remote",
                json!({"duid":self.duid.to_ascii_lowercase(),"name":if self.remote_name.is_empty(){"PS5"}else{&self.remote_name}}),
            );
        }
        note(ui, "主机需启用遥控游玩；待机连接还需要允许从网络唤醒。");
    }
    fn capture(&mut self, ui: &mut Ui, s: &Value, out: &mut Actions) {
        title(ui, "视频输入");
        let devs = items(&s["devices"]["devices"]);
        let current = self
            .selected_capture
            .clone()
            .unwrap_or_else(|| strval(&s["settings"], "captureDeviceId").into());
        let options = std::iter::once((String::new(), "选择采集设备".to_owned()))
            .chain(
                devs.iter()
                    .filter(|v| v["usable"].as_bool() != Some(false))
                    .map(|d| {
                        let name = strval(d, "name");
                        let note = if is_camera(d) { " · 摄像头" } else { "" };
                        (strval(d, "id").to_owned(), format!("{name}{note}"))
                    }),
            )
            .collect::<Vec<_>>();
        let chosen = choose(ui, "input-device", &current, &options, "选择采集设备");
        let mut id = current;
        if let Some(v) = chosen {
            if v != id {
                self.selected_capture = Some(v.clone());
                id = v;
                self.stage(s, "captureDeviceId", json!(id));
            }
        }
        let device = devs
            .iter()
            .find(|d| strval(d, "id") == id && d["usable"].as_bool() != Some(false));
        let unsaved_mode = self
            .draft
            .keys()
            .any(|k| page_key(Tab::Capture, k) && k != "captureDeviceId");
        ui.horizontal_wrapped(|ui| {
            if main_btn(
                ui,
                "开始预览",
                device.is_some() && !unsaved_mode && can_play(s),
            ) {
                cmd(out, "capture", json!({"deviceId":id}));
            }
            if btn(ui, "刷新设备", can_play(s)) {
                cmd(out, "devices", json!({}));
            }
        });
        if unsaved_mode {
            note(ui, "应用采集参数后即可预览。");
        } else if device.is_none() && !id.is_empty() {
            note(ui, "所选设备当前未连接。重新接入后刷新列表。");
        } else if device.is_none() {
            if s["ready"].as_bool() == Some(true)
                && devs
                    .iter()
                    .filter(|d| d["usable"].as_bool() != Some(false))
                    .all(is_camera)
            {
                note(
                    ui,
                    "未发现可识别的 HDMI 采集卡。无需采集卡也可在「连接」中使用 PS5 网络串流。",
                );
            } else {
                note(
                    ui,
                    "选定视频输入后再开始预览。使用采集卡时，将 PS5 的 HDMI 输出接至采集卡 IN。",
                );
            }
        } else {
            note(ui,"纯采集模式下，请将手柄连接 PS5。需使用电脑手柄时，在画质页选择「采集画面 + 网络控制」。");
        }
        title(ui, "输入模式");
        if let Some(d) = device {
            let modes = items(&d["formats"]);
            if !modes.is_empty() {
                let (w, h, fps, fmt) = (
                    self.get(s, "captureWidth").as_f64().unwrap_or(0.),
                    self.get(s, "captureHeight").as_f64().unwrap_or(0.),
                    self.get(s, "captureFps").as_f64().unwrap_or(0.),
                    self.get(s, "captureFormat")
                        .as_str()
                        .unwrap_or("auto")
                        .to_owned(),
                );
                let selected =
                    if w == 0. && h == 0. && fps == 0. && matches!(fmt.as_str(), "auto" | "") {
                        "auto".into()
                    } else {
                        modes
                            .iter()
                            .position(|m| {
                                m["width"].as_f64() == Some(w)
                                    && m["height"].as_f64() == Some(h)
                                    && (m["fps"].as_f64().unwrap_or(-1.) - fps).abs() < 0.02
                                    && strval(m, "format") == fmt
                            })
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "custom".into())
                    };
                let mut options = vec![("auto".into(), "自动协商".into())];
                for (i, m) in modes.iter().enumerate() {
                    options.push((
                        i.to_string(),
                        format!(
                            "{} × {} · {} fps · {}",
                            short_stat(&m["width"]),
                            short_stat(&m["height"]),
                            short_stat(&m["fps"]),
                            strval(m, "format")
                        ),
                    ));
                }
                if let Some(choice) =
                    choose(ui, "input-mode", &selected, &options, "自定义采集参数")
                {
                    if choice == "auto" {
                        for key in ["captureWidth", "captureHeight", "captureFps"] {
                            self.stage(s, key, json!(0));
                        }
                        self.stage(s, "captureFormat", json!("auto"));
                    } else if let Some(m) = choice.parse::<usize>().ok().and_then(|n| modes.get(n))
                    {
                        for (key, input) in [
                            ("captureWidth", "width"),
                            ("captureHeight", "height"),
                            ("captureFps", "fps"),
                            ("captureFormat", "format"),
                        ] {
                            self.stage(s, key, m[input].clone());
                        }
                    }
                }
            }
        }
        egui::CollapsingHeader::new("手动指定宽高、帧率与格式")
            .default_open(device.is_none())
            .show(ui, |ui| {
                self.number(ui, s, "captureWidth", "宽度", 0, 7680, 2., " px");
                self.number(ui, s, "captureHeight", "高度", 0, 4320, 2., " px");
                let mut fps = self.get(s, "captureFps").as_f64().unwrap_or_default();
                row(ui, "帧率", |ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut fps)
                                .range(0.0..=240.0)
                                .speed(0.25)
                                .max_decimals(3)
                                .suffix(" fps"),
                        )
                        .changed()
                    {
                        self.stage(s, "captureFps", json!(fps));
                    }
                });
                self.setting_choice(
                    ui,
                    s,
                    "captureFormat",
                    "格式",
                    &[
                        ("auto", "自动"),
                        ("NV12", "NV12"),
                        ("YUY2", "YUY2"),
                        ("MJPG", "MJPG"),
                        ("P010", "P010"),
                        ("RGB32", "RGB32"),
                        ("RGB24", "RGB24"),
                    ],
                );
                note(ui, "宽、高或帧率设为 0 时由引擎自动选择。");
            });
        self.setting_choice(
            ui,
            s,
            "captureColorRange",
            "色彩范围",
            &[("auto", "自动"), ("limited", "有限"), ("full", "完整")],
        );
        self.setting_choice(
            ui,
            s,
            "captureHdrMode",
            "HDR 输入",
            &[("auto", "自动"), ("off", "关闭"), ("hdr10", "HDR10")],
        );
        self.check(ui, s, "captureSkipWebcams", "自动选源时跳过摄像头");
        divider(ui);
        title(ui, "采集音频");
        let mut audio = vec![
            ("auto".into(), "自动匹配采集卡".into()),
            ("off".into(), "关闭采集音频".into()),
        ];
        audio.extend(
            items(&s["devices"]["audioDevices"])
                .iter()
                .map(|d| (strval(d, "id").into(), strval(d, "name").into())),
        );
        let a = self
            .get(s, "captureAudioDeviceId")
            .as_str()
            .unwrap_or("auto")
            .to_owned();
        if let Some(v) = choose(ui, "audio-input", &a, &audio, "输入设备未连接") {
            self.stage(s, "captureAudioDeviceId", json!(v));
        }
        self.slider(ui, s, "captureAudioVolume", "输入增益", 0., 2., 0.05);
        self.number(
            ui,
            s,
            "captureAudioOffsetMs",
            "音频偏移",
            -200,
            500,
            1.,
            " ms",
        );
        self.apply(ui, Tab::Capture, s, out);
    }
    fn picture(&mut self, ui: &mut Ui, s: &Value, out: &mut Actions) {
        title(ui, "画面风格");
        ui.horizontal_wrapped(|ui| {
            for (key, label) in [(0, "原样"), (1, "鲜艳"), (2, "电影"), (3, "竞技")] {
                if ui.button(label).clicked() {
                    let values = match key {
                        1 => [1.0, 1.06, 1., 0.5, 0., 0.3, 0., 0.],
                        2 => [0.95, 1.08, 1., 0.2, 0., 0., 0.3, 0.3],
                        3 => [1., 1., 1., 0.3, 0.6, 0.4, 0., 0.],
                        _ => [1., 1., 1., 0., 0., 0., 0., 0.],
                    };
                    for (k, v) in [
                        "saturation",
                        "contrast",
                        "gamma",
                        "vibrance",
                        "shadowBoost",
                        "clarity",
                        "colorTemperature",
                        "filmGrain",
                    ]
                    .into_iter()
                    .zip(values)
                    {
                        self.stage(s, k, json!(v));
                    }
                }
            }
        });
        egui::CollapsingHeader::new("微调颜色与质感").show(ui, |ui| {
            for (k, l, lo, hi) in [
                ("saturation", "饱和度", 0.5, 1.5),
                ("contrast", "对比度", 0.8, 1.2),
                ("gamma", "伽马", 0.8, 1.2),
                ("vibrance", "鲜艳度", -1., 1.),
                ("shadowBoost", "暗部提升", 0., 1.),
                ("clarity", "清晰度", 0., 1.),
                ("colorTemperature", "色温", -1., 1.),
                ("filmGrain", "颗粒", 0., 1.),
            ] {
                self.slider(ui, s, k, l, lo, hi, 0.01);
            }
            self.check(ui, s, "antiAliasing", "抗锯齿");
            self.check(ui, s, "debanding", "去色带");
            self.setting_choice(
                ui,
                s,
                "debandingStrength",
                "去色带强度",
                &[("low", "低"), ("medium", "中"), ("high", "高")],
            );
        });
        divider(ui);
        title(ui, "超分辨率");
        self.setting_choice(
            ui,
            s,
            "upscaler",
            "算法",
            &[
                ("off", "关闭"),
                ("fsr1", "AMD FSR 1"),
                ("rtx-vsr", "NVIDIA RTX 视频超分"),
                ("artcnn", "ArtCNN"),
                ("integer", "整数缩放"),
            ],
        );
        if let Some(actual) = upscaler_frame_state(
            &s["status"],
            live_stats(&s["status"]),
            strval(&s["settings"], "upscaler"),
        ) {
            note(ui, actual);
        } else if self.upscaler_request.is_some() {
            note(
                ui,
                "设置已发送，等待引擎确认并处理新视频帧。所选设置不代表已生效。",
            );
        } else {
            note(
                ui,
                "播放后会按引擎回报显示实际启用的超分算法。所选设置不代表已生效。",
            );
        }
        if s["status"]["renderFeedback"]["upscaler"]["canRetry"].as_bool() == Some(true)
            && btn(
                ui,
                "重试所选超分算法",
                can_play(s) && !self.draft.contains_key("upscaler"),
            )
        {
            cmd(out, "retry-upscaler", json!({}));
        }
        self.setting_choice(
            ui,
            s,
            "upscalerTarget",
            "输出尺寸",
            &[
                ("auto", "随窗口自动"),
                ("1080p", "1080p"),
                ("1440p", "1440p"),
                ("2160p", "2160p / 4K"),
            ],
        );
        if self.get(s, "upscaler").as_str() == Some("rtx-vsr") {
            self.setting_choice(
                ui,
                s,
                "upscalerQuality",
                "质量",
                &[
                    ("0", "自动"),
                    ("1", "1 · 性能"),
                    ("2", "2 · 均衡"),
                    ("3", "3 · 高"),
                    ("4", "4 · 最高"),
                ],
            );
        }
        if self.get(s, "upscaler").as_str() == Some("artcnn") {
            self.setting_choice(
                ui,
                s,
                "artcnnStyle",
                "风格",
                &[("auto", "自动"), ("sharp", "清晰"), ("denoise", "降噪")],
            );
        }
        self.slider(ui, s, "upscalerSharpness", "锐度", 0., 1., 0.05);
        self.slider(ui, s, "textureRecovery", "纹理恢复", 0., 1., 0.05);
        divider(ui);
        title(ui, "DLSS 5 神经渲染");
        // The requested flag belongs to the backend, shared with the transport bar.
        // Do not stage this flag behind unrelated uncommitted picture changes.
        let requested = s["settings"]["neuralRendering"].as_bool().unwrap_or(false);
        let mut nr = requested;
        let gpu = &s["gpu"];
        let capable = gpu["dlssNrSupported"].as_bool() == Some(true);
        if ui
            .add_enabled(
                can_play(s) && (capable || nr),
                egui::Checkbox::new(&mut nr, "启用 DLSS 5 NR"),
            )
            .changed()
        {
            cmd(out, "settings", json!({"neuralRendering":nr}));
        }
        let blocker = strval(gpu, "nrUnavailableReason").trim();
        if !blocker.is_empty() {
            note(ui, nr_reason(blocker));
        } else if !capable {
            note(
                ui,
                if gpu["dlssNrSupported"].as_bool() == Some(false) {
                    "当前显卡或驱动未报告 NR 支持。"
                } else {
                    "等待引擎确认 NR 硬件能力。"
                },
            );
        }
        let st = &s["status"];
        let stats = live_stats(st);
        let reported = if self.nr_request.is_some() {
            Some("应用中，等待新视频帧确认".to_owned())
        } else {
            playing(s)
                .then(|| nr_frame_state(st, stats, gpu, requested))
                .flatten()
        };
        if let Some(actual) = reported {
            let reason = strval(stats, "neuralRenderingReason").trim();
            let detail = if reason.is_empty() {
                String::new()
            } else {
                format!(" · {}", nr_reason(reason))
            };
            note(ui, format!("实际 NR：{actual}{detail}"));
        } else if requested && gpu["dlssNrSupported"].as_bool() == Some(false) {
            note(ui, "保存的 NR 偏好仍在，当前不能启用；可关闭此开关。");
        } else if requested {
            note(ui, "已选 NR；等待引擎回报实际帧状态。");
        } else if capable {
            note(
                ui,
                "此开关与底栏同步，立即应用。播放后可在状态页查看实际效果。",
            );
        }
        self.setting_choice(
            ui,
            s,
            "neuralRenderingPreset",
            "风格",
            &[
                ("default", "默认"),
                ("natural", "自然"),
                ("cinematic", "电影"),
            ],
        );
        self.slider(ui, s, "neuralRenderingIntensity", "强度", 0., 3., 0.1);
        self.setting_choice(
            ui,
            s,
            "neuralRenderingHudProtection",
            "HUD 保护",
            &[("off", "关闭"), ("smart", "智能"), ("strict", "严格")],
        );
        self.check(ui, s, "neuralRenderingColorPreserve", "保留原始色彩");
        divider(ui);
        egui::CollapsingHeader::new("串流画质与画面来源").show(ui, |ui| {
            self.setting_choice(
                ui,
                s,
                "pictureSource",
                "画面来源",
                &[
                    ("stream", "网络串流"),
                    ("capture", "采集画面 + 网络控制"),
                    ("auto", "自动"),
                ],
            );
            self.setting_choice(
                ui,
                s,
                "resolution",
                "分辨率",
                &[("1080p", "1080p"), ("720p", "720p"), ("540p", "540p")],
            );
            self.setting_choice(ui, s, "fps", "帧率", &[("60", "60 fps"), ("30", "30 fps")]);
            self.bitrate(ui, s);
            self.check(ui, s, "lockBitrate", "锁定本地目标码率");
            if let Some(message) =
                bitrate_lock_message(&s["status"]["renderFeedback"]["bitrateLock"])
            {
                note(ui, message);
            } else {
                note(ui, "只锁定 Relay 的本地目标请求，不保证 PS5 实际输出码率。");
            }
            note(
                ui,
                "200 Mbps 为实验性目标；修改后需重新连接。实际吞吐以引擎回报为准。",
            );
            self.setting_choice(
                ui,
                s,
                "codec",
                "视频编码",
                &[("hevc", "HEVC / H.265"), ("h264", "H.264")],
            );
            self.setting_choice(
                ui,
                s,
                "framePacing",
                "显示节奏",
                &[
                    ("lowest-latency", "最低延迟"),
                    ("smooth-vsync", "平滑 VSync"),
                    ("auto", "自动"),
                ],
            );
            self.number(ui, s, "fecRatioPercent", "纠错冗余", 0, 50, 1., " %");
            let locked = self.get(s, "lockBitrate").as_bool().unwrap_or(false);
            ui.add_enabled_ui(!locked, |ui| {
                self.check(ui, s, "adaptiveBitrateRestoration", "自动恢复码率");
            });
            if locked {
                note(ui, "锁定期间自动恢复码率已关闭；解锁后恢复此项设置。");
            }
            self.check(ui, s, "hdr", "请求 HDR");
            self.check(ui, s, "autoHdr", "自动 HDR");
            self.setting_choice(
                ui,
                s,
                "toneMapMode",
                "色调映射",
                &[
                    ("auto", "自动"),
                    ("hdr10-passthrough", "HDR10 直通"),
                    ("bt2390-sdr", "BT.2390 转 SDR"),
                ],
            );
            self.check(ui, s, "ambientFill", "填充画面边缘");
        });
        egui::CollapsingHeader::new("补帧").show(ui, |ui| {
            self.check(ui, s, "frameGeneration", "启用帧生成");
            self.setting_choice(
                ui,
                s,
                "frameGenerationBackend",
                "补帧引擎",
                &[
                    ("dlssg", "DLSS 帧生成"),
                    ("nvidia", "NVIDIA 视频补帧"),
                    ("warp", "轻量 Warp"),
                ],
            );
            self.setting_choice(
                ui,
                s,
                "frameGenerationMultiplier",
                "倍率",
                &[("2", "2×"), ("3", "3×"), ("4", "4×")],
            );
            self.setting_choice(
                ui,
                s,
                "frameDedup",
                "重复帧",
                &[
                    ("off", "关闭"),
                    ("auto", "自动识别"),
                    ("60-to-30", "60 → 30 后补帧"),
                ],
            );
        });
        self.apply(ui, Tab::Picture, s, out);
    }
    fn controls(&mut self, ui: &mut Ui, s: &Value, out: &mut Actions) {
        title(ui, "手柄");
        self.setting_choice(
            ui,
            s,
            "hapticsTier",
            "触觉反馈",
            &[
                ("auto", "自动"),
                ("usb-quad", "USB 精细触觉"),
                ("bt-3khz", "蓝牙触觉"),
                ("rumble", "普通振动"),
                ("off", "关闭"),
            ],
        );
        self.check(ui, s, "adaptiveTriggers", "自适应扳机");
        title(ui, "键盘与鼠标");
        self.check(ui, s, "keyboardMouseEnabled", "使用键鼠控制主机");
        self.slider(ui, s, "mouseSensitivity", "鼠标灵敏度", 0.2, 3., 0.1);
        self.check(ui, s, "mouseInvertY", "反转鼠标 Y 轴");
        self.apply(ui, Tab::Controls, s, out);
        divider(ui);
        title(ui, "会话操作");
        let active = playing(s) && !busy(s);
        ui.horizontal_wrapped(|ui| {
            for (action, label) in [
                ("ps", "PS 键"),
                ("options", "Options"),
                ("create", "Create"),
                ("touchpad-click", "触摸板"),
                ("mute-mic", "麦克风静音"),
                ("request-idr", "刷新关键帧"),
            ] {
                if btn(ui, label, active) {
                    cmd(out, "virtual", json!({"action":action}));
                }
            }
        });
        if s["status"]["phase"].as_str() == Some("pin-required") || !self.unlock_pin.is_empty() {
            title(ui, "主机解锁");
            edit(
                ui,
                "解锁 PIN",
                &mut self.unlock_pin,
                "输入 PS5 请求的解锁码",
                true,
                8,
            );
            let valid = (4..=8).contains(&self.unlock_pin.len())
                && self
                    .unlock_pin
                    .as_bytes()
                    .iter()
                    .all(|b| b.is_ascii_digit());
            if btn(ui, "提交解锁 PIN", can_play(s) && valid) {
                cmd(out, "pin", json!({"pin":self.unlock_pin}));
                self.unlock_pin.clear();
            }
        }
        note(ui, "纯采集模式下，手柄直接连接 PS5。");
    }
    fn diagnostics(&mut self, ui: &mut Ui, s: &Value) {
        let st = &s["status"];
        let stats = live_stats(st);
        let gpu = &s["gpu"];
        title(ui, "引擎与显示");
        row(ui, "显卡", |ui| {
            ui.label(gpu["adapterName"].as_str().unwrap_or("等待引擎"));
        });
        row(ui, "驱动", |ui| {
            ui.label(short_stat(&gpu["driverVersion"]));
        });
        if let Some(mode) = measured_mode(gpu, "displayWidth", "displayHeight", "displayHz", "Hz") {
            row(ui, "显示器", |ui| {
                ui.label(mode);
            });
        }
        row(ui, "NR 硬件支持", |ui| {
            let raw = gpu.get("dlssNrReported").unwrap_or(&gpu["dlssNrSupported"]);
            ui.label(match raw.as_bool() {
                Some(true) => "支持",
                Some(false) => "未报告支持",
                None => "未探测",
            });
        });
        row(ui, "NR 当前可用", |ui| {
            ui.label(match gpu["dlssNrSupported"].as_bool() {
                Some(true) => "可启用",
                Some(false) => "不可启用",
                None => "待探测",
            });
        });
        let blocker = strval(gpu, "nrUnavailableReason").trim();
        if !blocker.is_empty() {
            note(ui, nr_reason(blocker));
        }
        divider(ui);
        title(ui, "当前会话");
        if let Some(reason) = st["errorCode"].as_str().or(st["errorMessage"].as_str()) {
            if !reason.is_empty() {
                ui.label(reason);
            }
        }
        if playing(s) {
            let (source, capture) = session_source(st, stats);
            if let Some(message) = bitrate_lock_message(&st["renderFeedback"]["bitrateLock"]) {
                note(ui, message);
            }
            if let Some(pending) = pending_session_settings(st) {
                note(ui, pending);
            }
            row(ui, "画面来源", |ui| {
                ui.label(source);
            });
            let capture_state = strval(stats, "captureState");
            for (label, wk, hk, fk, show) in [
                ("视频输入", "width", "height", "fps", true),
                (
                    "采集卡输入",
                    "captureWidth",
                    "captureHeight",
                    "captureFps",
                    capture_state == "active" || (capture && capture_state.is_empty()),
                ),
                (
                    "处理后输出",
                    "outputWidth",
                    "outputHeight",
                    "outputFps",
                    true,
                ),
            ] {
                if let Some(mode) = show
                    .then(|| measured_mode(stats, wk, hk, fk, "fps"))
                    .flatten()
                {
                    row(ui, label, |ui| {
                        ui.label(mode);
                    });
                }
            }
            let requested = s["settings"]["neuralRendering"].as_bool().unwrap_or(false);
            if let Some(actual) = nr_frame_state(st, stats, gpu, requested) {
                row(ui, "实际 NR", |ui| {
                    ui.label(actual);
                });
            }
            if let Some(actual) =
                upscaler_frame_state(st, stats, strval(&s["settings"], "upscaler"))
            {
                row(ui, "超分状态", |ui| {
                    ui.label(actual);
                });
            }
            let nr_note = strval(stats, "neuralRenderingReason").trim();
            if !nr_note.is_empty() {
                row(ui, "NR 状态原因", |ui| {
                    ui.label(nr_reason(nr_note));
                });
            }
            let nr_guard = &st["renderFeedback"]["nrGuard"];
            if matches!(strval(nr_guard, "state"), "suspended" | "error")
                && !strval(nr_guard, "reason").is_empty()
            {
                row(ui, "NR 保护", |ui| {
                    ui.label(strval(nr_guard, "reason"));
                });
            }
            for (key, label, unit) in [
                ("decodeMs", "解码延迟", " ms"),
                ("renderMs", "渲染延迟", " ms"),
                ("netRttMs", "网络往返", " ms"),
                ("lossPct", "丢包率", " %"),
                ("bitrateKbps", "输入码率", ""),
                ("activeUpscaler", "实际超分", ""),
                ("neuralRenderingMs", "NR 处理耗时", " ms"),
                ("frameGeneration", "实际补帧", ""),
                ("captureState", "采集状态", ""),
                ("captureAudioActive", "采集音频", ""),
                ("controllerSummary", "手柄", ""),
            ] {
                if !stats[key].is_null() {
                    row(ui, label, |ui| {
                        let value = if key == "bitrateKbps" {
                            stats[key]
                                .as_f64()
                                .filter(|n| n.is_finite())
                                .map(bitrate_mbps)
                                .unwrap_or_else(|| short_stat(&stats[key]))
                        } else if key == "activeUpscaler" {
                            upscaler_name(strval(stats, key)).to_owned()
                        } else {
                            format!("{}{unit}", short_stat(&stats[key]))
                        };
                        ui.label(value);
                    });
                }
            }
            if let Some(focused) = stats["focused"].as_bool() {
                row(ui, "引擎窗口焦点", |ui| {
                    ui.label(if focused { "已聚焦" } else { "未聚焦" });
                });
            }
        } else {
            note(ui, "开始播放后，这里将显示实际输出、延迟、超分和 NR 状态。");
        }
        divider(ui);
        egui::CollapsingHeader::new("引擎原始状态").show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(serde_json::to_string_pretty(st).unwrap_or_default())
                        .monospace()
                        .small(),
                )
                .wrap(),
            );
        });
        egui::CollapsingHeader::new("最近运行记录").show(ui, |ui| {
            let logs = items(&s["logs"]);
            if logs.is_empty() {
                note(ui, "暂无记录");
            }
            for log in logs.iter().rev().take(35) {
                let time = strval(log, "time");
                let clock = time.get(11..19).unwrap_or("");
                ui.label(RichText::new(format!("{clock}  {}", strval(log, "message"))).small());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn applying_camera_edits_preserves_other_tabs_and_quick_nr() {
        let s = json!({"settings":{"captureFps":0,"saturation":1,"neuralRendering":false}});
        let mut p = Panels::default();
        p.stage(&s, "captureFps", json!(60));
        p.stage(&s, "saturation", json!(1.3));
        assert_eq!(
            Value::Object(p.patch(Tab::Capture)),
            json!({"captureFps":60})
        );
        p.on_result(
            "settings",
            &json!({"captureFps":0,"saturation":1,"neuralRendering":true}),
        );
        assert_eq!(
            Value::Object(p.patch(Tab::Picture)),
            json!({"saturation":1.3})
        );
        p.on_result(
            "settings",
            &json!({"captureFps":60,"saturation":1,"neuralRendering":true}),
        );
        assert!(p.patch(Tab::Capture).is_empty());
        assert_eq!(
            Value::Object(p.patch(Tab::Picture)),
            json!({"saturation":1.3})
        );
    }
    #[test]
    fn upscaler_feedback_uses_live_engine_report_and_names_fallbacks() {
        let streaming = json!({"phase":"streaming"});
        let frame = json!({"width":1920,"height":1080,"activeUpscaler":"fsr1"});
        assert_eq!(
            upscaler_frame_state(&streaming, &frame, "rtx-vsr"),
            Some("已选择 NVIDIA RTX 视频超分，当前实际使用：AMD FSR 1".into())
        );
        assert_eq!(
            upscaler_frame_state(
                &streaming,
                &json!({"width":1920,"height":1080,"activeUpscaler":"off"}),
                "rtx-vsr"
            ),
            Some("已选择 NVIDIA RTX 视频超分，当前实际未启用超分".into())
        );
        assert_eq!(
            upscaler_frame_state(
                &streaming,
                &json!({"width":1920,"height":1080,"activeUpscaler":"rtx-vsr"}),
                "rtx-vsr"
            ),
            Some("实际使用：NVIDIA RTX 视频超分".into())
        );
        assert_eq!(
            upscaler_frame_state(
                &json!({"phase":"streaming","renderFeedback":{"upscaler":{"reason":"fixture fallback"}}}),
                &frame,
                "rtx-vsr"
            ),
            Some("已选择 NVIDIA RTX 视频超分，当前实际使用：AMD FSR 1。 fixture fallback".into())
        );
        assert_eq!(
            upscaler_frame_state(
                &json!({"phase":"idle"}),
                &json!({"activeUpscaler":"fsr1"}),
                "rtx-vsr"
            ),
            None
        );
        assert_eq!(
            upscaler_frame_state(
                &json!({"phase":"streaming","renderFeedback":{"upscaler":{"state":"waiting","requested":"rtx-vsr"}}}),
                &frame,
                "rtx-vsr"
            ),
            None,
            "a prior frame must not be reported while the engine awaits the selected algorithm"
        );
        assert_eq!(
            upscaler_frame_state(&streaming, &json!({"activeUpscaler":"fsr1"}), "rtx-vsr"),
            None,
            "cached statistics without valid input dimensions are not a live frame"
        );
    }
    #[test]
    fn bitrate_lock_feedback_distinguishes_active_pending_and_fallback_without_using_input_rate() {
        assert_eq!(
            bitrate_lock_message(&json!({"state":"locked","targetKbps":125000})),
            Some("本地目标已锁定：125 Mbps。PS5 实际码率仍由主机与网络决定。".into())
        );
        assert_eq!(
            bitrate_lock_message(&json!({"state":"pending-reconnect","targetKbps":88000})),
            Some(
                "本地目标已锁定：88 Mbps；下次连接时发送。PS5 实际码率仍由主机与网络决定。".into()
            )
        );
        assert_eq!(
            bitrate_lock_message(&json!({"state":"pending-confirmation","targetKbps":88000})),
            Some("目标 88 Mbps 已发送，等待引擎确认码率与锁定状态。".into())
        );
        assert_eq!(
            bitrate_lock_message(
                &json!({"state":"fallback","targetKbps":88000,"reason":"引擎拒绝锁定"})
            ),
            Some("码率锁定回退：88 Mbps。引擎拒绝锁定".into())
        );
        assert_eq!(
            bitrate_lock_message(&json!({"state":"unlocked","targetKbps":30000})),
            None
        );
    }
    #[test]
    fn settings_feedback_tracks_requests_from_every_panel() {
        let mut p = Panels::default();
        p.track_request("settings", &json!({"neuralRendering":true}), 7);
        p.track_request("settings", &json!({"upscaler":"rtx-vsr"}), 8);
        assert_eq!(p.nr_request, Some(7));
        assert_eq!(p.upscaler_request, Some(8));
        p.finish_request(7);
        assert_eq!(p.nr_request, None);
        assert_eq!(p.upscaler_request, Some(8));
        p.clear_requests();
        assert_eq!(p.upscaler_request, None);
    }
    #[test]
    fn pending_session_options_are_reported_until_reconnect() {
        assert_eq!(
            pending_session_settings(
                &json!({"renderFeedback":{"pendingSettings":["fps","resolution","codec"]}})
            ),
            Some("已保存；当前会话需断开并重连后生效：串流分辨率、串流帧率、编码格式".into())
        );
        assert_eq!(
            pending_session_settings(&json!({"renderFeedback":{"pendingSettings":[]}})),
            None
        );
    }
    fn draw_panel(
        ctx: &egui::Context,
        p: &mut Panels,
        tab: Tab,
        state: &Value,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Actions) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(364., 1500.),
            )),
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let view = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                actions.extend(p.render(ui, tab, state));
            });
        });
        (view, actions)
    }
    fn text_labels(view: &egui::FullOutput) -> Vec<&str> {
        view.shapes
            .iter()
            .filter_map(|item| match &item.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.as_str()),
                _ => None,
            })
            .collect()
    }
    fn click_label(
        ctx: &egui::Context,
        p: &mut Panels,
        tab: Tab,
        s: &Value,
        label: &str,
    ) -> Actions {
        let (view, _) = draw_panel(ctx, p, tab, s, vec![]);
        let point = view
            .shapes
            .iter()
            .find_map(|item| match &item.shape {
                egui::Shape::Text(t) if t.galley.job.text == label => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing control: {label}"));
        let button = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(draw_panel(
            ctx,
            p,
            tab,
            s,
            vec![egui::Event::PointerMoved(point), button(true)]
        )
        .1
        .is_empty());
        draw_panel(ctx, p, tab, s, vec![button(false)]).1
    }
    fn click_nr(ctx: &egui::Context, p: &mut Panels, s: &Value) -> Actions {
        click_label(ctx, p, Tab::Picture, s, "启用 DLSS 5 NR")
    }
    #[test]
    fn lan_authorization_preserves_host_and_pin_and_uses_saved_identity_only_after_click() {
        let ctx = egui::Context::default();
        let mut state = json!({"ready":true});
        let mut p = Panels {
            ip: "192.0.2.51".into(),
            pin: "00001234".into(),
            ..Default::default()
        };
        let (initial, actions) = draw_panel(&ctx, &mut p, Tab::Connect, &state, vec![]);
        let initial = text_labels(&initial);
        assert!(actions.is_empty());
        assert!(initial.contains(&"在浏览器登录 Sony") && !initial.contains(&"PSN 账号 ID"));
        assert!(click_label(&ctx, &mut p, Tab::Connect, &state, "配对主机").is_empty());
        assert_eq!(
            click_label(&ctx, &mut p, Tab::Connect, &state, "在浏览器登录 Sony"),
            vec![("open-auth".to_owned(), json!({}))]
        );
        p.credential = "synthetic return value".into();
        assert_eq!(
            click_label(&ctx, &mut p, Tab::Connect, &state, "完成授权"),
            vec![(
                "sign-in".to_owned(),
                json!({"credential":"synthetic return value"})
            )]
        );
        assert!(p.credential.is_empty());
        state["account"] =
            json!({"onlineId":"SamplePlayer", "accountIdBase64":"AQAAAAAAAAA=", "userId":"1"});
        p.on_result("sign-in", &state["account"]);
        assert!(!p.remote_tab && !p.manual_account && p.account_id.is_empty());
        let (signed, actions) = draw_panel(&ctx, &mut p, Tab::Connect, &state, vec![]);
        assert!(actions.is_empty());
        let signed = text_labels(&signed);
        assert!(
            signed.contains(&"配对账号：SamplePlayer")
                && signed.contains(&"查看账号 ID")
                && !signed.contains(&"PSN 账号 ID")
        );
        assert_eq!(
            click_label(&ctx, &mut p, Tab::Connect, &state, "配对主机"),
            vec![(
                "pair".to_owned(),
                json!({"ipAddress":"192.0.2.51","pin":"00001234","hostType":"PS5"})
            )]
        );
    }
    #[test]
    fn lan_manual_identity_is_opt_in_and_never_silently_replaces_a_saved_user() {
        let ctx = egui::Context::default();
        let guest = json!({"ready":true});
        let mut p = Panels {
            ip: "192.0.2.52".into(),
            pin: "12345678".into(),
            ..Default::default()
        };
        assert!(click_label(&ctx, &mut p, Tab::Connect, &guest, "手动输入账号 ID").is_empty());
        let form = draw_panel(&ctx, &mut p, Tab::Connect, &guest, vec![]).0;
        assert!(text_labels(&form).contains(&"PSN 账号 ID"));
        p.account_id = "Nickname".into();
        assert!(click_label(&ctx, &mut p, Tab::Connect, &guest, "配对主机").is_empty());
        p.account_id = " 18446744073709551615 ".into();
        assert_eq!(
            click_label(&ctx, &mut p, Tab::Connect, &guest, "配对主机"),
            vec![(
                "pair".to_owned(),
                json!({"ipAddress":"192.0.2.52","pin":"12345678","hostType":"PS5","accountId":"18446744073709551615"})
            )]
        );
        let saved = json!({"ready":true,"account":{"accountIdBase64":"AQAAAAAAAAA=","userId":"1"}});
        assert!(click_label(&ctx, &mut p, Tab::Connect, &saved, "使用已登录账号").is_empty());
        assert!(p.pairing_args(&saved).unwrap().get("accountId").is_none());
        assert!(p.pairing_args(&guest).is_err());
        assert!(valid_account_id("//////////8="));
        assert!(!valid_account_id("AAAAAAAAAAB="));
    }
    #[test]
    fn nr_only_enables_with_a_positive_probe_and_always_allows_clearing_a_saved_request_when_ready()
    {
        for capability in [Value::Null, json!(false)] {
            let ctx = egui::Context::default();
            let mut p = Panels::default();
            let mut state = json!({"ready":true,"gpu":{"dlssNrSupported":capability,"nrUnavailableReason":"本机驱动触发了组件保护"},"settings":{"neuralRendering":false}});
            assert!(click_nr(&ctx, &mut p, &state).is_empty());
            let picture = draw_panel(&ctx, &mut p, Tab::Picture, &state, vec![]).0;
            assert!(text_labels(&picture).contains(&"本机驱动触发了组件保护"));
            state["settings"]["neuralRendering"] = json!(true);
            if capability == json!(false) {
                state["gpu"]["dlssNrReported"] = json!(true);
                let d = draw_panel(&ctx, &mut p, Tab::Diagnostics, &state, vec![]).0;
                let raw = text_labels(&d);
                assert!(raw.contains(&"支持") && raw.contains(&"不可启用"));
                let d = draw_panel(&ctx, &mut p, Tab::Picture, &state, vec![]).0;
                assert!(
                    text_labels(&d).contains(&"保存的 NR 偏好仍在，当前不能启用；可关闭此开关。")
                );
            }
            assert_eq!(
                click_nr(&ctx, &mut p, &state),
                vec![("settings".into(), json!({"neuralRendering":false}))]
            );
            state["operation"] = json!("connect");
            assert!(click_nr(&ctx, &mut p, &state).is_empty());
        }
    }
    #[test]
    fn live_diagnostics_separate_source_capture_and_output_without_inventing_a_display_or_target_rate(
    ) {
        let ctx = egui::Context::default();
        let mut p = Panels::default();
        let mut s = json!({"status":{"phase":"streaming","session":{"captureOnly":true},"stats": {
            "pictureSource":"stream","captureState":"active", "width":1280, "height":720,"fps":59.94,"bitrateKbps":30555,
            "captureWidth":1920,"captureHeight":1080,"captureFps":119.88,
            "outputWidth":2560,"outputHeight":1440,"outputFps":117.5, "targetFps":240,
            "neuralRendering":"unavailable","neuralRenderingReason":"hdr"}},"gpu":{"displayWidth":0,"displayHeight":1080,"displayHz":240},"settings":{"neuralRendering":true}});
        let v = draw_panel(&ctx, &mut p, Tab::Diagnostics, &s, vec![]).0;
        let labels = text_labels(&v);
        for text in [
            "采集卡（仅采集）",
            "视频输入",
            "1280 × 720 · 59.94 fps",
            "采集卡输入",
            "1920 × 1080 · 119.88 fps",
            "处理后输出",
            "2560 × 1440 · 117.5 fps",
            "未生效",
            "当前运行库无法处理此 HDR 输入（hdr）",
            "30.555 Mbps",
        ] {
            assert!(labels.contains(&text), "missing {text}: {labels:?}");
        }
        let pic = draw_panel(&ctx, &mut p, Tab::Picture, &s, vec![]).0;
        assert!(
            text_labels(&pic).contains(&"实际 NR：未生效 · 当前运行库无法处理此 HDR 输入（hdr）")
        );
        for text in ["显示器", "生效", "网络串流", "240 fps"] {
            assert!(!labels.contains(&text));
        }
        s["status"]["session"]["captureOnly"] = json!(false);
        let stats = &mut s["status"]["stats"];
        stats["captureOnly"] = json!(true);
        stats["outputFps"] = Value::Null;
        stats["width"] = json!(0);
        stats["captureHeight"] = json!(-5);
        let v = draw_panel(&ctx, &mut p, Tab::Diagnostics, &s, vec![]).0;
        let labels = text_labels(&v);
        assert!(labels.contains(&"采集卡（仅采集）") && labels.contains(&"2560 × 1440"));
        assert!(
            !labels.contains(&"视频输入")
                && !labels.contains(&"采集卡输入")
                && !labels.iter().any(|l| l.contains("240 fps"))
        );
        s["status"]["stats"]["captureOnly"] = json!(false);
        s["status"]["stats"]["pictureSource"] = json!("capture");
        let v = draw_panel(&ctx, &mut p, Tab::Diagnostics, &s, vec![]).0;
        assert!(text_labels(&v).contains(&"采集卡画面 + 网络控制"));
    }
    #[test]
    fn opening_each_view_does_not_start_camera_or_network_and_nr_follows_shared_state() {
        let ctx = egui::Context::default();
        let mut p = Panels::default();
        let mut s = json!({"ready":true,"settings":{"neuralRendering":false, "saturation":1},"gpu":{"dlssNrSupported":true},"devices":{"devices":[{"id":"test-camera","name":"Webcam","usable":true}]}});
        for tab in [
            Tab::Connect,
            Tab::Capture,
            Tab::Controls,
            Tab::Diagnostics,
            Tab::Picture,
        ] {
            assert!(draw_panel(&ctx, &mut p, tab, &s, vec![]).1.is_empty());
        }
        assert!(p.selected_capture.is_none());
        p.stage(&s, "saturation", json!(1.1));
        for requested in [true, false] {
            let actions = click_nr(&ctx, &mut p, &s);
            assert_eq!(
                actions,
                vec![("settings".to_owned(), json!({"neuralRendering":requested}))]
            );
            s["settings"]["neuralRendering"] = json!(requested);
            assert_eq!(
                Value::Object(p.patch(Tab::Picture)),
                json!({"saturation":1.1})
            );
        }
    }
    #[test]
    fn settings_acknowledge_integral_slider_values_without_losing_newer_edits() {
        let state = json!({"settings": {"saturation": 1, "gamma": 0.9, "captureFps": 60}});
        let mut p = Panels::default();
        p.stage(&state, "saturation", json!(1.0));
        assert!(p.patch(Tab::Picture).is_empty());
        p.stage(&state, "gamma", json!(1.0));
        p.stage(&state, "captureFps", json!(59.94));
        p.on_result("settings", &json!({"gamma": 1, "captureFps": 60}));
        assert!(p.patch(Tab::Picture).is_empty());
        assert_eq!(
            Value::Object(p.patch(Tab::Capture)),
            json!({"captureFps":59.94})
        );
    }
    #[test]
    fn bitrate_slider_preserves_legacy_fraction_and_commits_only_after_an_accessible_edit() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut panels = Panels::default();
        let state = json!({"settings":{"bitrateKbps":30555,"saturation":1.0}});
        let draw = |panels: &mut Panels, events: Vec<egui::Event>| {
            let mut response = None;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(308., 180.),
                )),
                events,
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    response = Some(panels.bitrate(ui, &state));
                });
            });
            (output, response.unwrap())
        };

        let (output, response) = draw(&mut panels, vec![]);
        let labels = text_labels(&output);
        assert!(labels.contains(&"目标码率（2–200 Mbps）"));
        assert!(labels.contains(&"30.555 Mbps"));
        assert!(!response.changed());
        assert!(panels.draft.is_empty());

        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        let (slider_id, slider_node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == egui::accesskit::Role::Slider)
            .expect("the target bitrate is an accessible slider");
        let slider_id = *slider_id;
        assert_eq!(slider_node.label(), Some("目标码率（2–200 Mbps）"));

        let (output, response) = draw(
            &mut panels,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::SetValue,
                    target: slider_id,
                    data: Some(egui::accesskit::ActionData::NumericValue(75.0)),
                },
            )],
        );
        assert!(response.changed());
        assert!(text_labels(&output).contains(&"75 Mbps"));
        assert_eq!(panels.draft.get("bitrateKbps"), Some(&json!(75000)));
        assert_eq!(
            Value::Object(panels.patch(Tab::Picture)),
            json!({"bitrateKbps":75000})
        );
    }
    #[test]
    fn host_search_does_not_autoselect_or_connect_an_input() {
        let mut p = Panels::default();
        p.on_result(
            "discover",
            &json!({"consoles":[{"hostName":"Lab PS5","ipAddress":"192.0.2.3"}]}),
        );
        assert!(p.ip.is_empty());
        assert!(p.selected_capture.is_none());
        assert!(is_camera(
            &json!({"name":"手机 (Windows 虚拟摄像头)","likelyWebcam":false})
        ));
        assert!(!is_camera(
            &json!({"name":"HDMI Video Capture","likelyWebcam":false})
        ));
        assert!(!can_play(&json!({"ready":false,"settings":{}})));
        assert!(valid_account_id("AQAAAAAAAAA="));
        assert!(!valid_account_id("nickname"));
    }
}
