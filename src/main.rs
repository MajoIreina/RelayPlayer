//! Windows 11-style Rust player. Rust owns the window, command bar, stage and transport.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod backend;
mod chiaki;
mod chiaki_ui;
mod connections;
mod design;
mod migration;
#[cfg(test)]
mod migration_settings_tests;
mod panels;
mod platform;
#[allow(dead_code)]
#[path = "playback.rs"]
mod playback;
use backend::{Backend, BackendEvent};
use design::*;
use egui::{
    pos2, vec2, Align, Align2, Color32 as C, Context, Id, Layout, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, ViewportCommand,
};
use panels::{Panels, Tab};
use platform::PlayerSurface;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::HWND,
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
};

const TABS: [(Tab, &str, &str, Icon); 5] = [
    (Tab::Connect, "连接", "主机连接", Icon::Monitor),
    (Tab::Capture, "采集", "采集卡输入", Icon::Capture),
    (Tab::Picture, "画质", "画质与滤镜", Icon::Picture),
    (Tab::Controls, "手柄", "手柄与操作", Icon::Gamepad),
    (Tab::Diagnostics, "状态", "运行状态", Icon::Activity),
];
fn tabinfo(tab: Tab) -> (&'static str, Icon) {
    TABS.iter()
        .find(|(t, _, _, _)| *t == tab)
        .map(|(_, _, s, i)| (*s, *i))
        .unwrap()
}
fn phase(v: &Value) -> &str {
    v["status"]["phase"].as_str().unwrap_or("idle")
}
fn active(v: &Value) -> bool {
    matches!(
        phase(v),
        "streaming" | "reconnecting" | "connecting" | "waking" | "pin-required"
    )
}
fn phase_name(v: &Value) -> &str {
    match phase(v) {
        "idle" => "待机",
        "streaming" => "播放中",
        "reconnecting" => "重连中",
        "waking" => "正在唤醒",
        "connecting" => "正在连接",
        "pin-required" => "等待解锁",
        "error" | "failed" => "连接异常",
        _ => "处理中",
    }
}
fn nr_display(state: &Value, pending: bool) -> &'static str {
    if pending {
        return "应用中";
    }
    let requested = state["settings"]["neuralRendering"]
        .as_bool()
        .unwrap_or(false);
    let st = &state["status"];
    if phase(state) != "streaming" {
        return if requested { "已选" } else { "关闭" };
    }
    match st["renderFeedback"]["nrGuard"]["state"]
        .as_str()
        .unwrap_or("")
    {
        "suspended" => return "保护暂停",
        "error" => return "保护异常",
        "unconfirmed" => return "未确认",
        _ => {}
    }
    let stats = session_stats(st);
    let has_frame = !st["renderFeedback"]["nrWaitingForFrame"]
        .as_bool()
        .unwrap_or(false)
        && stats["recovering"].as_bool() != Some(true)
        && ["width", "height"]
            .iter()
            .all(|key| stats[*key].as_f64().is_some_and(|n| n >= 1.));
    if !has_frame {
        return "等待画面";
    }
    match stats["neuralRendering"].as_str() {
        Some("on") => "生效",
        Some("unavailable") => "未生效",
        Some("preparing") => "准备中",
        Some("off") if requested => "未生效",
        Some("off") => "关闭",
        _ if requested => "等待帧",
        _ => "关闭",
    }
}
fn text_at(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_owned()
}

#[derive(Clone)]
struct Prefs {
    volume: f32,
    muted: bool,
    auto_hide: bool,
    native_remote_window: bool,
}
impl Prefs {
    fn load(root: &Path) -> Self {
        let v: Value = fs::read(root.join("data/player.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        Self {
            volume: v["volume"].as_f64().unwrap_or(1.).clamp(0., 1.) as f32,
            muted: v["muted"].as_bool().unwrap_or(false),
            auto_hide: v["autoHide"].as_bool().unwrap_or(true),
            native_remote_window: v["nativeRemoteWindow"].as_bool().unwrap_or(false),
        }
    }
    fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = root.join("data");
        fs::create_dir_all(&dir)?;
        let tmp = dir.join("player.tmp");
        fs::write(
            &tmp,
            json!({"volume":self.volume,"muted":self.muted,"autoHide":self.auto_hide,"nativeRemoteWindow":self.native_remote_window}).to_string(),
        )?;
        fs::rename(tmp, dir.join("player.json"))
    }
}
struct Notice {
    text: String,
    bad: bool,
    at: Instant,
}
struct Relay {
    install_root: PathBuf,
    profile_root: PathBuf,
    backend: Option<Backend>,
    public: Option<chiaki_ui::PublicPlayback>,
    close_after_cancel: bool,
    online: bool,
    state: Value,
    unavailable: Option<String>,
    hwnd: isize,
    surface: PlayerSurface,
    panels: Panels,
    tab: Option<Tab>,
    prefs: Prefs,
    prefs_changed: Option<Instant>,
    notice: Option<Notice>,
    fullscreen: bool,
    last_top: Instant,
    last_phase: String,
    nr_request: Option<u64>,
    source_intent: Tab,
    last_capture: bool,
}
impl Relay {
    fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        setup(&cc.egui_ctx);
        let hwnd = match cc.window_handle().ok().map(|h| h.as_raw()) {
            Some(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        let migration_error = startup.import_if_requested().err();
        let prefs = Prefs::load(&startup.profile_root);
        let (backend, unavailable) = if startup.ipc {
            match Backend::spawn(&startup.install_root, &startup.profile_root.join("data")) {
                Ok(b) => (Some(b), None),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };
        let mut public = (!startup.ipc).then(|| {
            chiaki_ui::PublicPlayback::new(
                startup.install_root.clone(),
                startup.profile_root.clone(),
                startup.custom_data_root,
                startup.backend_archive,
            )
        });
        if let Some(p) = &mut public {
            if startup.connect_nr {
                let _ = p.save(&json!({"neuralRendering":true}));
            }
            if startup.connect {
                if let Err(e) = p.connections.request("discover", json!({})) {
                    p.note = e.to_string();
                }
            }
        }
        let online = public.is_some();
        let state = public.as_ref().map_or(
            Value::Null,
            |p| json!({"ready":true,"publicRuntime":true,"settings":p.settings,"status":{"phase":"idle"}}),
        );
        Self {
            install_root: startup.install_root,
            profile_root: startup.profile_root,
            backend,
            public,
            close_after_cancel: false,
            online,
            state,
            unavailable,
            hwnd,
            surface: PlayerSurface::new(hwnd),
            panels: Panels::default(),
            tab: startup.connect.then_some(Tab::Connect),
            prefs,
            prefs_changed: None,
            notice: migration_error.map(|e| Notice {
                text: format!("旧版授权或设置导入未完成：{e}。旧版数据未修改。"),
                bad: true,
                at: Instant::now(),
            }),
            fullscreen: false,
            last_top: Instant::now(),
            last_phase: "idle".into(),
            nr_request: None,
            source_intent: Tab::Connect,
            last_capture: false,
        }
    }
    fn message(&mut self, txt: impl Into<String>, bad: bool) {
        self.notice = Some(Notice {
            text: txt.into(),
            bad,
            at: Instant::now(),
        });
    }
    fn selected(&mut self, t: Tab) {
        self.tab = if self.tab == Some(t) { None } else { Some(t) };
        self.last_top = Instant::now();
    }
    fn request(&mut self, name: &str, arg: Value) -> Option<u64> {
        if self.public.is_some() {
            if name == "stop" {
                self.surface.hide_video();
            }
            if name == "capture"
                || (name == "settings"
                    && arg.as_object().is_some_and(|o| {
                        o.iter().any(|(k, v)| {
                            playback::session_key(k)
                                && self.public.as_ref().unwrap().settings[k] != *v
                        })
                    }))
            {
                self.surface.release_video();
            }
            let p = self.public.as_mut().unwrap();
            let result = match name {
                "connect" => p.connect(&arg),
                "capture" => p.capture(&arg),
                "devices" => p.devices(),
                "pin" => p.unlock(&arg),
                "open-auth" => p
                    .connections
                    .authorization_url()
                    .and_then(|url| open_sony(&url).map_err(std::io::Error::other)),
                "discover" | "pair" | "sign-in" | "sign-out" | "wake" | "remove" => {
                    p.connections.request(name, arg)
                }
                "settings" => p.save(&arg),
                "stop" => {
                    let result = p.stop();
                    if result.is_ok() {
                        p.note = "正在断开本次播放……".into();
                    }
                    result
                }
                _ => Err(std::io::Error::other("当前公开后端尚未接入此操作。")),
            };
            let message = match result {
                Ok(()) => (p.note.clone(), false),
                Err(e) => (e.to_string(), true),
            };
            self.state["settings"] = p.settings.clone();
            if !message.1 && name == "settings" {
                self.panels.on_result("settings", &p.settings);
            }
            self.message(message.0, message.1);
            return None;
        }
        if name == "open-cookie" {
            if let Err(e) = open_sony("https://ca.account.sony.com/api/v1/ssocookie") {
                self.message(e, true);
            }
            return None;
        }
        let action = if name == "open-auth" {
            "auth-url"
        } else {
            name
        };
        if !self.online {
            self.message("引擎尚未就绪，可在运行状态中重试。", true);
            return None;
        }
        if action == "capture" {
            self.source_intent = Tab::Capture;
        } else if matches!(action, "connect" | "remote") {
            self.source_intent = Tab::Connect;
        }
        match self.backend.as_ref().map(|b| b.send(action, arg.clone())) {
            Some(Ok(id)) => {
                self.panels.track_request(action, &arg, id);
                if action == "settings" && arg.get("neuralRendering").is_some() {
                    self.nr_request = Some(id);
                }
                Some(id)
            }
            Some(Err(e)) => {
                self.message(e.to_string(), true);
                None
            }
            None => {
                self.message("引擎连接已关闭。", true);
                None
            }
        }
    }
    fn restart(&mut self) {
        if self.public.is_some() {
            self.request("discover", json!({}));
            return;
        }
        self.surface.shutdown();
        self.surface = PlayerSurface::new(self.hwnd);
        if let Some(mut b) = self.backend.take() {
            b.shutdown();
        }
        self.online = false;
        self.unavailable = None;
        self.state = Value::Null;
        self.nr_request = None;
        self.panels.clear_requests();
        self.notice = None;
        match Backend::spawn(&self.install_root, &self.profile_root.join("data")) {
            Ok(b) => self.backend = Some(b),
            Err(e) => self.unavailable = Some(e.to_string()),
        }
    }
    fn poll_backend(&mut self) {
        if let Some(p) = &mut self.public {
            let old = p.note.clone();
            let result = p.poll();
            let connection_event = p.connections.poll();
            let metadata = p.connections.metadata();
            self.state["consoles"] = metadata["consoles"].clone();
            self.state["account"] = metadata["account"].clone();
            self.state["settings"] = p.settings.clone();
            // GUI readiness is not a video-frame measurement. Never fabricate
            // streaming statistics or a positive NR hardware probe.
            self.state["status"]["phase"] = json!(if p.player.video_received() {
                "streaming"
            } else if p.player.stopping() {
                "disconnecting"
            } else if p.player.running() {
                "connecting"
            } else {
                "idle"
            });
            self.state["operation"] = json!(if p.player.preparing() {
                "connect"
            } else {
                &p.connections.operation
            });
            self.state["connectionError"] = json!(p.connections.startup_error);
            let render_feedback = p.player.render_feedback();
            self.state["status"]["stats"] = render_feedback["stats"].clone();
            self.state["gpu"] = render_feedback["stats"]["gpu"].clone();
            self.state["status"]["telemetryStale"] = render_feedback["telemetryStale"].clone();
            if render_feedback["pinRequired"]["required"] == true
                && p.player.running()
                && !p.player.stopping()
            {
                self.state["status"]["phase"] = json!("pin-required");
            }
            if render_feedback["devices"]["type"] == "devices" {
                self.state["devices"] = render_feedback["devices"].clone();
            }
            self.state["status"]["renderFeedback"] = p.feedback();
            if let Some((action, event)) = connection_event {
                match event {
                    Ok(value) => {
                        self.panels.on_result(&action, &value);
                        let text = match action.as_str() {
                            "discover" => format!(
                                "发现 {} 台主机。",
                                value["consoles"].as_array().map_or(0, Vec::len)
                            ),
                            "pair" => "配对已保存，可以开始串流。".into(),
                            "sign-in" => "Sony 授权已保存，无需手动填写账号 ID。".into(),
                            "wake" => "已发送唤醒请求。".into(),
                            "remove" => "已移除本地主机配对。".into(),
                            _ => "操作已完成。".into(),
                        };
                        self.message(text, false);
                    }
                    Err(e) => self.message(e, true),
                }
            } else if let Err(e) = result {
                self.message(e.to_string(), true);
            } else if old != p.note {
                let note = p.note.clone();
                self.message(note, false);
            }
            return;
        }
        while let Some(ev) = self.backend.as_ref().and_then(|b| b.try_recv()) {
            match ev {
                BackendEvent::State(v) => {
                    let p = phase(&v).to_string();
                    if p != self.last_phase {
                        if p == "pin-required" {
                            self.tab = Some(Tab::Controls);
                            self.message("PS5 请求解锁，请在手柄面板中输入主机 PIN。", false);
                        } else if p == "error" || p == "failed" {
                            let code = v["status"]["errorMessage"]
                                .as_str()
                                .or(v["status"]["errorCode"].as_str())
                                .unwrap_or("查看运行状态了解原因");
                            self.message(format!("连接异常：{code}"), true);
                        } else if p == "streaming" {
                            self.last_top = Instant::now();
                        }
                    }
                    self.last_phase = p;
                    self.online = v["ready"].as_bool().unwrap_or(false);
                    self.unavailable = None;
                    self.state = v;
                }
                BackendEvent::Result { id, name, value } => {
                    self.panels.finish_request(id);
                    self.panels.on_result(&name, &value);
                    let was_nr = self.nr_request == Some(id);
                    if was_nr {
                        self.nr_request = None;
                    }
                    match name.as_str() {
                        "auth-url" => {
                            if let Err(e) = open_sony(value["url"].as_str().unwrap_or("")) {
                                self.message(e, true)
                            }
                        }
                        "connect" | "capture" | "remote" => {
                            self.last_capture = name == "capture";
                            self.tab = None;
                            self.last_top = Instant::now();
                            self.notice = None;
                        }
                        "settings" => {
                            if let Some(s) = value.as_object() {
                                if s.contains_key("upscaler") {
                                    self.state["settings"] = value.clone();
                                }
                            }
                            if was_nr {
                                self.message(
                                    if self.state["settings"]["neuralRendering"].as_bool()
                                        == Some(true)
                                    {
                                        "已启用 NR 设置；收到画面后显示实际状态。"
                                    } else {
                                        "已关闭 NR 设置；收到画面后确认实际状态。"
                                    },
                                    false,
                                );
                            } else {
                                self.message(
                                    "已保存设置；画质页和状态页会显示实际效果及需重连的参数。",
                                    false,
                                );
                            }
                        }
                        "retry-upscaler" => {
                            self.message("已重新请求所选算法，等待新的视频帧确认。", false)
                        }
                        "pair" => self.message("已配对，可开始串流。", false),
                        "wake" => self.message("已发送唤醒请求。", false),
                        "stop" => self.message("已断开播放。", false),
                        "devices" => self.message("视频和音频设备列表已刷新。", false),
                        "discover" => self.message(
                            format!(
                                "发现 {} 台主机。",
                                value["consoles"].as_array().map_or(0, Vec::len)
                            ),
                            false,
                        ),
                        "sign-in" => self.message("PSN 授权已保存。", false),
                        "sign-out" => self.message("已退出 PSN。", false),
                        "remove" => self.message("已移除本地配对。", false),
                        _ => {}
                    }
                }
                BackendEvent::Error { id, name, message } => {
                    self.panels.finish_request(id);
                    if self.nr_request == Some(id) {
                        self.nr_request = None;
                    }
                    self.message(format!("{}：{}", action_label(&name), message), true);
                }
                BackendEvent::Exited(reason) => {
                    self.online = false;
                    self.unavailable = Some(reason);
                    self.state = Value::Null;
                    self.nr_request = None;
                    self.panels.clear_requests();
                    self.last_phase = "idle".into();
                }
            }
        }
    }
    fn toggle_full(&mut self, ctx: &Context) {
        if !self.fullscreen && self.surface.status.native_window {
            self.message("请在独立视频窗口内切换全屏。", false);
            return;
        }
        self.fullscreen = !self.fullscreen;
        if self.fullscreen {
            // Keep the settings page and its draft for the return to windowed mode,
            // but don't leave a hidden popup or text field consuming game input.
            ctx.memory_mut(|m| {
                m.close_popup();
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
        self.last_top = Instant::now();
        ctx.request_repaint();
    }
    fn input(&mut self, ctx: &Context) {
        let (video_f11, video_esc) = self.surface.video_shortcuts();
        let pressed = |key| {
            ctx.input(|i| {
                i.events.iter().any(|e| {
                    matches!(e,
            egui::Event::Key { key: k, pressed: true, repeat: false, .. } if *k == key)
                })
            })
        };
        let f11 = video_f11 || pressed(egui::Key::F11);
        let esc = video_esc || pressed(egui::Key::Escape);
        if f11 || (self.fullscreen && esc) {
            self.toggle_full(ctx);
        } else if esc && self.tab.is_some() && !ctx.memory(|m| m.any_popup_open()) {
            self.tab = None;
        }
    }
    fn should_show_top(&mut self, ctx: &Context) -> bool {
        let near_top = self
            .surface
            .cursor_in_host()
            .is_some_and(|(_, y)| y >= 0 && (y as f32) < 54.0 * ctx.pixels_per_point());
        let playing = phase(&self.state) == "streaming";
        if !playing
            || self.tab.is_some()
            || near_top
            || ctx.input(|i| i.key_pressed(egui::Key::Tab))
        {
            self.last_top = Instant::now();
        }
        toolbar_visible(
            self.prefs.auto_hide,
            playing,
            self.tab.is_some() || near_top,
            self.last_top.elapsed(),
        )
    }
    fn header(&mut self, ui: &mut Ui, r: Rect) {
        let p = ui.painter().clone();
        p.rect_filled(r, 0, BAR);
        p.line_segment(
            [
                r.left_bottom() - vec2(0., 0.5),
                r.right_bottom() - vec2(0., 0.5),
            ],
            Stroke::new(1.0_f32, LINE),
        );
        let mid = r.center().y;
        brand(&p, pos2(r.left() + 30., mid));
        label(
            &p,
            pos2(r.left() + 57., mid - 1.),
            Align2::LEFT_CENTER,
            "Relay",
            strong(22.),
            TEXT,
        );
        let mut x = r.left() + 135.;
        p.vline(
            x - 10.,
            (mid - 11.)..=(mid + 11.),
            Stroke::new(1.0_f32, LINE),
        );
        let w = if r.width() < 950. { 66. } else { 80. };
        for (tab, name, desc, icon) in TABS {
            let rect = elem(x, mid - 16., w, 32.);
            let selected = self.tab == Some(tab);
            let res = command(
                ui,
                name,
                rect,
                icon,
                name,
                desc,
                if selected {
                    ButtonStyle::Selected
                } else {
                    ButtonStyle::Ghost
                },
                true,
            )
            .on_hover_text(desc);
            if res.clicked() {
                self.selected(tab);
            }
            x += w + 3.;
        }
        let right = r.right() - 15.;
        let on = self.prefs.auto_hide;
        let tooltip = if on {
            "固定顶部工具栏"
        } else {
            "播放时自动隐藏顶部工具栏"
        };
        let res = command(
            ui,
            "pin-chrome",
            elem(right - 34., mid - 16., 34., 32.),
            if on { Icon::Unpin } else { Icon::Pin },
            "",
            tooltip,
            ButtonStyle::Ghost,
            true,
        )
        .on_hover_text(tooltip);
        if res.clicked() {
            self.prefs.auto_hide = !on;
            self.prefs_changed = Some(Instant::now());
            self.last_top = Instant::now();
        }
        if r.width() > 755. {
            let t = if self.unavailable.is_some() {
                "引擎离线"
            } else if !self.online {
                "启动中"
            } else {
                phase_name(&self.state)
            };
            badge(
                &p,
                elem(right - 128., mid - 11., 77., 22.),
                t,
                if active(&self.state) { ACCENT } else { MUTED },
            );
        }
    }
    fn notice_bar(&mut self, ui: &mut Ui, r: Rect) {
        let (msg, bad) = match self.notice.as_ref() {
            Some(n) => (n.text.clone(), n.bad),
            None => return,
        };
        let p = ui.painter();
        p.rect_filled(
            r,
            0,
            if bad {
                C::from_rgb(55, 41, 48)
            } else {
                C::from_rgb(42, 41, 60)
            },
        );
        let area = r.shrink2(vec2(16., 0.));
        let clipped = p.with_clip_rect(Rect::from_min_max(
            area.min,
            pos2(area.right() - 32., area.bottom()),
        ));
        label(
            &clipped,
            pos2(area.left(), r.center().y),
            Align2::LEFT_CENTER,
            &msg,
            font(13.),
            if bad { WARN } else { TEXT },
        );
        let x = command(
            ui,
            "close-note",
            elem(area.right() - 24., area.center().y - 12., 24., 24.),
            Icon::Close,
            "",
            "关闭通知",
            ButtonStyle::Ghost,
            true,
        )
        .on_hover_text(&msg);
        if x.clicked() {
            self.notice = None;
        }
    }
    fn transport(&mut self, ui: &mut Ui, r: Rect, ctx: &Context) {
        let p = ui.painter().clone();
        p.rect_filled(r, 0, BAR);
        p.hline(r.x_range(), r.top() + 0.5, Stroke::new(1.0_f32, LINE));
        let y = r.center().y;
        let is_active = active(&self.state)
            || matches!(
                self.state["operation"].as_str(),
                Some("remote" | "connect" | "capture")
            );
        let (icon, hint) = if is_active {
            (Icon::Stop, "结束本次播放")
        } else {
            (Icon::Play, "连接最近使用的主机")
        };
        let play = command(
            ui,
            "transport-play",
            elem(r.left() + 16., y - 18., 36., 36.),
            icon,
            "",
            hint,
            ButtonStyle::Primary,
            true,
        )
        .on_hover_text(hint);
        if play.clicked() {
            if is_active {
                self.request("stop", json!({}));
            } else {
                let recent = self
                    .public
                    .as_ref()
                    .and_then(|p| p.connections.recent_target());
                if let Some(target) = recent {
                    self.source_intent = Tab::Connect;
                    self.request("connect", target);
                    self.tab = None;
                } else {
                    self.source_intent = Tab::Connect;
                    self.tab = Some(Tab::Connect);
                }
            }
        }
        let status = &self.state["status"];
        let title = if is_active {
            status["hostName"]
                .as_str()
                .or(status["session"]["hostName"].as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(if capture_source(status) || self.last_capture {
                    "采集卡"
                } else {
                    "PS5"
                })
        } else {
            "未连接"
        };
        let session_text = if is_active {
            input_geometry(status)
                .map(|(w, h, f)| {
                    if let Some(f) = f {
                        format!("输入  {w} × {h}   ·   {f:.0} fps")
                    } else {
                        format!("输入  {w} × {h}")
                    }
                })
                .unwrap_or_else(|| phase_name(&self.state).to_string())
        } else {
            "选择主机或 HDMI 输入".into()
        };
        let x = r.left() + 65.;
        let clip = p.with_clip_rect(Rect::from_min_max(
            pos2(x, r.top()),
            pos2(r.right() - 430., r.bottom()),
        ));
        label(
            &clip,
            pos2(x, y - 8.),
            Align2::LEFT_CENTER,
            title,
            strong(14.),
            TEXT,
        );
        label(
            &clip,
            pos2(x, y + 11.),
            Align2::LEFT_CENTER,
            session_text,
            font(12.),
            DIM,
        );
        let full = elem(r.right() - 52., y - 18., 36., 36.);
        if command(
            ui,
            "fullscreen",
            full,
            if self.fullscreen {
                Icon::Restore
            } else {
                Icon::Full
            },
            "",
            "全屏切换（F11）",
            ButtonStyle::Ghost,
            true,
        )
        .on_hover_text("全屏播放  ·  F11；按 Esc 或 F11 返回")
        .clicked()
        {
            self.toggle_full(ctx);
        }
        let nrr = elem(full.left() - 156., y - 18., 143., 36.);
        let requested = self.state["settings"]["neuralRendering"]
            .as_bool()
            .unwrap_or(false);
        let nr = if let Some(p) = &self.public {
            if p.player.preparing() {
                "准备中"
            } else if p.pending {
                "待重连"
            } else {
                nr_display(&self.state, false)
            }
        } else {
            nr_display(&self.state, self.nr_request.is_some())
        };
        let capable = self.state["gpu"]["dlssNrSupported"]
            .as_bool()
            .unwrap_or(false);
        let busy = self.nr_request.is_some() || !text_at(&self.state, "operation").is_empty();
        let can = self.online && (self.public.is_some() || requested || capable) && !busy;
        let res = ui.interact(
            nrr,
            Id::new("transportNR"),
            if can { Sense::click() } else { Sense::hover() },
        );
        res.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                can,
                requested,
                "DLSS 5 NR 神经渲染",
            )
        });
        let fill = if requested {
            C::from_rgb(65, 54, 98)
        } else if res.hovered() {
            TILE
        } else {
            C::from_rgb(40, 42, 56)
        };
        p.rect(
            nrr,
            7,
            fill,
            Stroke::new(
                1.0_f32,
                if requested {
                    C::from_rgb(115, 99, 158)
                } else {
                    LINE
                },
            ),
            StrokeKind::Inside,
        );
        let c = if requested {
            ACCENT
        } else if can {
            TEXT
        } else {
            DIM
        };
        glyph(&p, Icon::Spark, pos2(nrr.left() + 16., y), 17., c);
        label(
            &p,
            pos2(nrr.left() + 30., y),
            Align2::LEFT_CENTER,
            "DLSS 5 NR",
            strong(13.),
            c,
        );
        label(
            &p,
            pos2(nrr.right() - 9., y),
            Align2::RIGHT_CENTER,
            if self.nr_request.is_some() {
                "应用中"
            } else {
                nr
            },
            font(11.5),
            if matches!(nr, "未生效" | "保护暂停" | "保护异常" | "未确认") {
                WARN
            } else {
                c
            },
        );
        if res.clicked() && can {
            self.request("settings", json!({"neuralRendering":!requested}));
        }
        res.on_hover_text(if self.public.is_some() {
            "选择社区 NR；重新打开连接后应用。实际状态在画面内 RTX 面板查看。"
        } else if !capable && !requested {
            self.state["gpu"]["nrUnavailableReason"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(if self.online {
                    "引擎未报告本机支持 DLSS 5 NR"
                } else {
                    "正在检测播放引擎与显卡"
                })
        } else {
            "一键切换神经渲染；“已选”表示已设置，“生效”表示引擎正在处理视频帧。"
        });
        let x = nrr.left() - 205.;
        p.vline(
            nrr.left() - 17.,
            y - 12. ..=y + 12.,
            Stroke::new(1.0_f32, LINE),
        );
        let muted = self.prefs.muted || self.prefs.volume <= 0.;
        if command(
            ui,
            "mute",
            elem(x, y - 16., 31., 32.),
            if muted { Icon::Mute } else { Icon::Speaker },
            "",
            if muted { "取消静音" } else { "静音" },
            ButtonStyle::Ghost,
            true,
        )
        .on_hover_text("播放器静音 / 取消静音")
        .clicked()
        {
            if self.prefs.volume <= 0. {
                self.prefs.volume = 0.5;
                self.prefs.muted = false;
            } else {
                self.prefs.muted = !self.prefs.muted;
            }
            self.prefs_changed = Some(Instant::now());
        }
        let vr = elem(x + 37., y - 15., 88., 30.);
        let slider = ui.interact(vr, Id::new("master-volume"), Sense::click_and_drag());
        let mut volume_change: Option<f32> = None;
        if slider.clicked() || slider.dragged() {
            if let Some(at) = slider.interact_pointer_pos() {
                volume_change = Some(((at.x - vr.left()) / vr.width()).clamp(0., 1.));
            }
        }
        use egui::accesskit::{Action, ActionData};
        let volume_focused = slider.has_focus();
        let (steps, number) = ctx.input(|i| {
            let kb = if volume_focused {
                i.num_presses(egui::Key::ArrowRight) as i32
                    - i.num_presses(egui::Key::ArrowLeft) as i32
            } else {
                0
            };
            let step = kb + i.num_accesskit_action_requests(slider.id, Action::Increment) as i32
                - i.num_accesskit_action_requests(slider.id, Action::Decrement) as i32;
            let num = i
                .accesskit_action_requests(slider.id, Action::SetValue)
                .filter_map(|r| match r.data {
                    Some(ActionData::NumericValue(n)) if n.is_finite() => Some(n),
                    _ => None,
                })
                .last();
            (step, num)
        });
        if steps != 0 {
            volume_change = Some(self.prefs.volume + 0.05 * steps as f32);
        }
        if let Some(n) = number {
            volume_change = Some(n as f32 / 100.);
        }
        if let Some(new) = volume_change {
            self.prefs.volume = (new.clamp(0., 1.) * 100.).round() / 100.;
            self.prefs.muted = false;
            self.prefs_changed = Some(Instant::now());
        }
        slider.widget_info(|| {
            egui::WidgetInfo::slider(
                true,
                (self.prefs.volume as f64 * 100.).round(),
                "播放器音量",
            )
        });
        ctx.accesskit_node_builder(slider.id, |n| {
            n.set_min_numeric_value(0.);
            n.set_max_numeric_value(100.);
            n.set_numeric_value_step(5.);
            n.add_action(Action::SetValue);
            n.add_action(Action::Increment);
            n.add_action(Action::Decrement);
        });
        let v = if self.prefs.muted {
            0.
        } else {
            self.prefs.volume
        };
        let end = vr.left() + vr.width() * v;
        p.line_segment(
            [pos2(vr.left(), y), pos2(vr.right(), y)],
            Stroke::new(3.0_f32, LINE),
        );
        p.line_segment(
            [pos2(vr.left(), y), pos2(end, y)],
            Stroke::new(3.0_f32, ACCENT),
        );
        p.circle_filled(pos2(end, y), if slider.hovered() { 7. } else { 6. }, TEXT);
        p.circle_filled(pos2(end, y), 3.3, ACCENT);
        if slider.has_focus() {
            p.rect_stroke(
                vr.expand(2.),
                4,
                Stroke::new(1.0_f32, ACCENT),
                StrokeKind::Inside,
            );
        }
        slider.on_hover_text("播放器音量（不改变系统音量）");
        label(
            &p,
            pos2(x + 172., y),
            Align2::RIGHT_CENTER,
            format!("{:.0}%", v * 100.),
            font(13.),
            MUTED,
        );
    }
    fn draw_stage(&mut self, ui: &mut Ui, ctx: &Context, stage: Rect) {
        let video = fit_stage(
            stage,
            input_geometry(&self.state["status"]).map(|(w, h, _)| (w, h)),
        );
        let physical = pixel_rect(video, ctx.screen_rect().min, ctx.pixels_per_point());
        // Only the launched process can own a playback window or an audio session.
        let pid = self.public.as_ref().map_or_else(
            || self.backend.as_ref().map_or(0, Backend::playback_pid),
            |p| p.player.playback_pid(),
        );
        let volume = if self.prefs.muted {
            0.
        } else {
            self.prefs.volume
        };
        let stopping = self.public.as_ref().is_some_and(|p| p.player.stopping());
        let playing = active(&self.state) || stopping;
        self.surface.set_public_runtime(self.public.is_some());
        if !stopping {
            self.surface.set_native_window(
                self.prefs.native_remote_window && !capture_source(&self.state["status"]),
            );
        }
        self.surface.tick(
            pid,
            playing,
            if stopping { None } else { Some(physical) },
            if stopping { 0. } else { volume },
        );
        if stopping {
            self.surface.hide_video();
        }
        if !self.surface.status.embedded {
            let mut viewport = ui.new_child(
                UiBuilder::new()
                    .id_salt("stage")
                    .max_rect(stage)
                    .layout(Layout::top_down(Align::Min)),
            );
            viewport.set_clip_rect(stage);
            self.waiting(&mut viewport, stage);
        }
    }
    fn waiting(&mut self, ui: &mut Ui, r: Rect) {
        let p = ui.painter().with_clip_rect(r);
        let c = r.center();
        let h = r.height();
        let wide = r.width() > 480.;
        let loading = !self.online && self.unavailable.is_none();
        let error = self.unavailable.is_some();
        let working = active(&self.state)
            || matches!(
                self.state["operation"].as_str(),
                Some("connect" | "remote" | "capture")
            );
        let mut mesh = egui::Mesh::default();
        let (nx, ny) = (12, 9);
        for j in 0..=ny {
            for i in 0..=nx {
                let (x, y) = (i as f32 / nx as f32, j as f32 / ny as f32);
                let glow =
                    (1. - (((x - 0.5) * 1.85).powi(2) + ((y - 0.46) * 1.7).powi(2)).sqrt()).max(0.);
                let k = glow * glow;
                mesh.colored_vertex(
                    pos2(r.left() + x * r.width(), r.top() + y * r.height()),
                    C::from_rgb(
                        (17. + 8. * k) as u8,
                        (19. + 7. * k) as u8,
                        (27. + 16. * k) as u8,
                    ),
                );
            }
        }
        for j in 0..ny {
            for i in 0..nx {
                let k = j * (nx + 1) + i;
                mesh.add_triangle(k, k + 1, k + nx + 1);
                mesh.add_triangle(k + 1, k + nx + 2, k + nx + 1);
            }
        }
        p.add(egui::Shape::mesh(mesh));
        let note = if self.fullscreen {
            "按 Esc 或 F11 返回窗口，选择视频来源。"
        } else if self.tab.is_some() {
            "选择设备并开始播放后，画面将在这里显示。"
        } else {
            "从网络连接 PlayStation，或使用 HDMI 采集卡。"
        };
        let (heading, desc) = if error {
            (
                "播放后端未启动",
                self.unavailable
                    .as_deref()
                    .unwrap_or("请检查程序目录内的 backend/relay-backend.exe。"),
            )
        } else if loading {
            ("正在准备播放器", "正在等待本地播放后端就绪。")
        } else if self.surface.status.native_window {
            (
                "独立窗口正在播放",
                "画面由播放后端在独立窗口中显示。手柄输入由该后端处理。",
            )
        } else if working {
            (
                "等待视频画面",
                if !self.surface.status.video_message.is_empty() {
                    &self.surface.status.video_message
                } else {
                    "正在建立播放会话。"
                },
            )
        } else {
            ("等待视频输入", note)
        };
        let scale = if h < 330. {
            0.70
        } else if wide {
            1.12
        } else {
            0.85
        };
        let base = c.y + if wide { 2. } else { 0. };
        idle_art(
            &p,
            pos2(c.x, base - if h > 350. { 58. } else { 47. }),
            scale,
        );
        let heading_y = base + 12.;
        label(
            &p,
            pos2(c.x, heading_y),
            Align2::CENTER_CENTER,
            heading,
            strong(if wide { 27. } else { 23. }),
            TEXT,
        );
        let avail = (r.width() - 36.).max(20.);
        let gal = p.layout(desc.to_owned(), font(13.), MUTED, avail.min(520.));
        let desc_y = heading_y + 24.;
        p.galley(pos2(c.x - gal.size().x / 2., desc_y), gal.clone(), MUTED);
        if !self.fullscreen && !working && !loading && h > 190. {
            let y = desc_y + gal.size().y + 18.;
            if error {
                if command(
                    ui,
                    "retry-engine",
                    elem(c.x - 62., y, 124., 34.),
                    Icon::Retry,
                    "重新启动",
                    "重试播放后端",
                    ButtonStyle::Primary,
                    true,
                )
                .clicked()
                {
                    self.restart();
                }
            } else if self.tab.is_none() {
                if command(
                    ui,
                    "stage-connect",
                    elem(c.x - 131., y, 128., 36.),
                    Icon::Monitor,
                    "连接主机",
                    "连接主机",
                    ButtonStyle::Primary,
                    true,
                )
                .clicked()
                {
                    self.selected(Tab::Connect);
                }
                if command(
                    ui,
                    "stage-capture",
                    elem(c.x + 7., y, 124., 36.),
                    Icon::Capture,
                    "打开采集卡",
                    "打开采集卡",
                    ButtonStyle::Secondary,
                    true,
                )
                .clicked()
                {
                    self.selected(Tab::Capture);
                }
            }
        }
        if r.width() > 375. && h > 320. {
            let pos = r.min + vec2(21., 20.);
            let tint = C::from_rgb(130, 136, 159);
            p.circle_stroke(pos + vec2(3., 0.), 3.0, Stroke::new(1.2_f32, tint));
            label(
                &p,
                pos + vec2(15., 0.),
                Align2::LEFT_CENTER,
                if working {
                    "正在接收信号"
                } else {
                    "输入待机"
                },
                font(12.),
                tint,
            );
        }
    }
}

fn session_stats(st: &Value) -> &Value {
    st.get("stats")
        .filter(|v| v.is_object())
        .unwrap_or(&st["session"]["stats"])
}
fn capture_source(st: &Value) -> bool {
    let stats = session_stats(st);
    st["captureOnly"].as_bool() == Some(true)
        || st["session"]["captureOnly"].as_bool() == Some(true)
        || stats["captureOnly"].as_bool() == Some(true)
        || stats["pictureSource"].as_str() == Some("capture")
}
fn input_geometry(st: &Value) -> Option<(u32, u32, Option<f64>)> {
    let stats = session_stats(st);
    let read = |wk: &str, hk: &str, fk: &str| {
        let size = |k: &str| {
            stats[k]
                .as_f64()
                .filter(|n| *n >= 1. && *n <= u32::MAX as f64 && n.fract() == 0.)
                .map(|n| n as u32)
        };
        Some((
            size(wk)?,
            size(hk)?,
            stats[fk].as_f64().filter(|n| n.is_finite() && *n > 0.),
        ))
    };
    if capture_source(st) {
        if let Some(mode) = read("captureWidth", "captureHeight", "captureFps") {
            return Some(mode);
        }
    }
    read("width", "height", "fps")
}
fn fit_stage(stage: Rect, source: Option<(u32, u32)>) -> Rect {
    if let Some((w, h)) = source.filter(|(w, h)| *w > 0 && *h > 0) {
        let scale = (stage.width() / w as f32).min(stage.height() / h as f32);
        Rect::from_center_size(stage.center(), vec2(w as f32 * scale, h as f32 * scale))
    } else {
        stage
    }
}
fn toolbar_visible(auto_hide: bool, playing: bool, engaged: bool, elapsed: Duration) -> bool {
    !auto_hide || !playing || engaged || elapsed < Duration::from_secs(3)
}
fn pixel_rect(r: Rect, origin: Pos2, scale: f32) -> (i32, i32, i32, i32) {
    let a = ((r.left() - origin.x) * scale).round() as i32;
    let b = ((r.top() - origin.y) * scale).round() as i32;
    let c = ((r.right() - origin.x) * scale).round() as i32;
    let d = ((r.bottom() - origin.y) * scale).round() as i32;
    (a, b, (c - a).max(0), (d - b).max(0))
}
impl eframe::App for Relay {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            if let Some(p) = &self.public {
                if p.player.preparing() {
                    self.close_after_cancel = true;
                    p.player.cancel_preparation();
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                }
            }
        }
        self.poll_backend();
        if self.close_after_cancel && self.public.as_ref().is_some_and(|p| !p.player.preparing()) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        self.input(ctx);
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.at.elapsed() > Duration::from_secs(if n.bad { 12 } else { 5 }))
        {
            self.notice = None;
        }
        if self
            .prefs_changed
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(400))
        {
            self.prefs_changed = None;
            if let Err(e) = self.prefs.save(&self.profile_root) {
                self.message(format!("无法保存播放器偏好：{e}"), true);
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ctx, |ui| {
                let all = ui.max_rect();
                // Fullscreen is picture-only, regardless of pinning, pointer position,
                // notifications or an open settings page. Hidden controls receive no input.
                if self.fullscreen {
                    self.draw_stage(ui, ctx, all);
                    return;
                }
                let show_top = self.should_show_top(ctx);
                let top_h = if show_top { 54. } else { 0. };
                let bottom_h = 64.;
                let mut stage_top = all.top() + top_h;
                if show_top {
                    self.header(ui, elem(all.left(), all.top(), all.width(), top_h));
                }
                if self.notice.is_some() {
                    let h = 31.;
                    self.notice_bar(ui, elem(all.left(), stage_top, all.width(), h));
                    stage_top += h;
                }
                let bottom = Rect::from_min_max(pos2(all.left(), all.bottom() - bottom_h), all.max);
                self.transport(ui, bottom, ctx);
                let mid = Rect::from_min_max(
                    pos2(all.left(), stage_top),
                    pos2(all.right(), bottom.top().max(stage_top)),
                );
                let side_width = if self.tab.is_some() {
                    (all.width() * 0.35)
                        .clamp(340., 405.)
                        .min((all.width() - 165.).max(0.))
                } else {
                    0.
                };
                let stage =
                    Rect::from_min_max(mid.min, pos2(mid.right() - side_width, mid.bottom()));
                self.draw_stage(ui, ctx, stage);
                if let Some(tab) = self.tab {
                    let right = Rect::from_min_max(pos2(stage.right(), mid.top()), mid.max);
                    let p = ui.painter().clone();
                    p.rect_filled(right, 0, SIDE);
                    p.vline(
                        right.left() + 0.5,
                        right.y_range(),
                        Stroke::new(1.0_f32, LINE),
                    );
                    let head = elem(right.left() + 18., right.top(), right.width() - 36., 55.);
                    let (title, icon) = tabinfo(tab);
                    glyph(
                        &p,
                        icon,
                        pos2(head.left() + 11., head.center().y),
                        20.,
                        ACCENT,
                    );
                    label(
                        &p,
                        pos2(head.left() + 34., head.center().y),
                        Align2::LEFT_CENTER,
                        title,
                        strong(19.),
                        TEXT,
                    );
                    if command(
                        ui,
                        "close-settings",
                        elem(head.right() - 31., head.center().y - 15., 31., 30.),
                        Icon::Close,
                        "",
                        "关闭设置面板",
                        ButtonStyle::Ghost,
                        true,
                    )
                    .on_hover_text("关闭设置  ·  Esc")
                    .clicked()
                    {
                        self.tab = None;
                    }
                    p.hline(right.x_range(), head.bottom(), Stroke::new(1.0_f32, LINE));
                    let body = Rect::from_min_max(
                        pos2(right.left() + 18., head.bottom() + 10.),
                        pos2(right.right() - 12., right.bottom() - 6.),
                    );
                    if body.is_positive() {
                        let mut side = ui.new_child(
                            UiBuilder::new()
                                .id_salt(("panel-body", tab))
                                .max_rect(body)
                                .layout(Layout::top_down(Align::Min)),
                        );
                        side.set_clip_rect(body);
                        let out = egui::ScrollArea::vertical()
                            .id_salt(("panel-scroll", tab))
                            .auto_shrink([false, false])
                            .show(&mut side, |ui| {
                                ui.set_width(ui.available_width().min(body.width()).max(32.) - 2.);
                                if tab == Tab::Controls {
                                    if ui.checkbox(&mut self.prefs.native_remote_window, "独立串流窗口").changed() {
                                        self.prefs_changed = Some(Instant::now());
                                    }
                                    ui.label("需要单独显示串流画面时可启用独立窗口；采集卡画面保持嵌入。实体手柄和后台输入取决于当前播放后端。");
                                    ui.separator();
                                }
                                if let Some(p) = &self.public {
                                    ui.small(&p.note);
                                    if p.player.preparing() {
                                        if let Some(progress) = p.progress { ui.add(egui::ProgressBar::new(progress)); }
                                        if ui.add_enabled(!p.player.cancelling(),egui::Button::new("取消准备")).clicked() { p.player.cancel_preparation(); }
                                    }
                                    ui.separator();
                                }
                                let out = self.panels.render(ui, tab, &self.state);
                                if tab == Tab::Diagnostics {
                                    ui.add_space(10.);
                                    ui.separator();
                                    ui.label(
                                        egui::RichText::new("播放窗口与声音").font(strong(15.)),
                                    );
                                    ui.label(&self.surface.status.video_message);
                                    ui.label(&self.surface.status.audio_message);
                                    if let Some(reason) = self.unavailable.clone() {
                                        ui.colored_label(WARN, reason);
                                        if ui.button("重试播放后端").clicked() {
                                            self.restart();
                                        }
                                    }
                                }
                                ui.add_space(10.);
                                out
                            })
                            .inner;
                        for (name, params) in out {
                            self.request(&name, params);
                        }
                    }
                }
            });
        ctx.request_repaint_after(Duration::from_millis(if active(&self.state) {
            150
        } else {
            300
        }));
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(public) = &mut self.public {
            public.player.shutdown();
        }
        self.surface.shutdown();
        if let Some(mut worker) = self.backend.take() {
            worker.shutdown();
        }
        if self.prefs_changed.is_some() {
            let _ = self.prefs.save(&self.profile_root);
        }
    }
}

fn action_label(name: &str) -> &str {
    match name {
        "settings" => "应用设置",
        "devices" => "刷新设备",
        "discover" => "搜索主机",
        "pair" => "配对主机",
        "remove" => "移除主机",
        "connect" => "连接主机",
        "remote" => "公网连接",
        "capture" => "打开采集卡",
        "stop" => "停止播放",
        "wake" => "唤醒主机",
        "pin" => "解锁主机",
        "virtual" => "手柄操作",
        "sign-in" => "Sony 登录",
        "auth-url" => "打开授权页",
        "psn-consoles" => "获取 PSN 主机",
        "sign-out" => "退出 PSN",
        _ => "播放器",
    }
}
fn trusted_sony(url: &str) -> bool {
    url.len() < 8192
        && !url.chars().any(char::is_control)
        && (url == "https://ca.account.sony.com/api/v1/ssocookie"
            || url
                .starts_with("https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize?"))
}
fn open_sony(url: &str) -> Result<(), String> {
    if !trusted_sony(url) {
        return Err("无法识别 Sony 授权地址。".into());
    }
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let opened = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if (opened.0 as isize) <= 32 {
        Err("无法打开默认浏览器，请检查 Windows 的默认应用设置。".into())
    } else {
        Ok(())
    }
}
fn native_chrome(hwnd: isize) {
    use windows::Win32::Graphics::Dwm::*;
    if hwnd == 0 {
        return;
    }
    let host = HWND(hwnd as *mut _);
    for (kind, color) in [
        (DWMWA_USE_IMMERSIVE_DARK_MODE, 1u32),
        (DWMWA_CAPTION_COLOR, 0x00231b1au32),
        (DWMWA_TEXT_COLOR, 0x00faf0efu32),
        (DWMWA_BORDER_COLOR, 0x00503e3bu32),
    ] {
        unsafe {
            let _ = DwmSetWindowAttribute(host, kind, (&color as *const u32).cast(), 4);
        }
    }
}
struct Startup {
    install_root: PathBuf,
    profile_root: PathBuf,
    import_legacy: bool,
    ipc: bool,
    custom_data_root: bool,
    connect: bool,
    connect_nr: bool,
    backend_archive: Option<PathBuf>,
}
impl Startup {
    fn from_options(
        install_root: PathBuf,
        override_root: Option<PathBuf>,
        args: impl IntoIterator<Item = std::ffi::OsString>,
    ) -> std::io::Result<Self> {
        let custom_data_root = override_root.is_some();
        let mut import_legacy = false;
        let mut ipc = false;
        let mut connect = false;
        let mut connect_nr = false;
        let mut backend_archive = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--ipc-backend" {
                ipc = true;
            } else if arg == "--import-legacy" {
                // Preserve the explicit v0.1.7 CLI. Its encrypted vault is only meaningful to IPC backends.
                ipc = true;
                import_legacy = !custom_data_root;
            } else if arg == "--connect" {
                connect = true;
            } else if arg == "--connect-nr" {
                connect = true;
                connect_nr = true;
            } else if arg == "--backend-archive" {
                let path = args.next().filter(|s| !s.is_empty()).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "--backend-archive 需要官方 ZIP 文件路径。",
                    )
                })?;
                backend_archive = Some(std::path::absolute(path)?);
            }
        }
        if ipc && (connect || backend_archive.is_some()) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
                "--connect / --connect-nr / --backend-archive 适用于 chiaki-ng，不能与 --ipc-backend / --import-legacy 同用。"));
        }
        let profile_root = match override_root {
            Some(path) if !path.as_os_str().is_empty() => std::path::absolute(path)?,
            Some(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "RELAY_PLAYER_ROOT 不能为空。",
                ))
            }
            None => install_root.clone(),
        };
        Ok(Self {
            install_root,
            profile_root,
            import_legacy,
            ipc,
            custom_data_root,
            connect,
            connect_nr,
            backend_archive,
        })
    }
    fn import_if_requested(&self) -> std::io::Result<bool> {
        if self.import_legacy {
            migration::inherit(&self.install_root)
        } else {
            Ok(false)
        }
    }
    fn current() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        let root = exe
            .parent()
            .ok_or_else(|| std::io::Error::other("无法确定播放器所在目录。"))?;
        Self::from_options(
            root.to_owned(),
            std::env::var_os("RELAY_PLAYER_ROOT").map(PathBuf::from),
            std::env::args_os().skip(1),
        )
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let startup = Startup::current()?;
    let icon = egui::IconData {
        rgba: include_bytes!("../assets/icon.rgba").to_vec(),
        width: 64,
        height: 64,
    };
    let opt = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100., 680.])
            .with_min_inner_size([760., 490.])
            .with_icon(icon),
        centered: true,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Relay · PS5 播放器",
        opt,
        Box::new(move |cc| {
            let app = Relay::new(cc, startup);
            native_chrome(app.hwnd);
            Ok(Box::new(app))
        }),
    )
    .map_err(Into::into)
}
#[cfg(test)]
mod ui_tests {
    use super::*;
    #[test]
    fn public_startup_uses_a_verified_gui_for_normal_and_direct_connect() {
        let root = std::env::temp_dir().join("relay-cli-test");
        let def = Startup::from_options(root.clone(), None, []).unwrap();
        assert!(!def.ipc && !def.import_legacy && !def.connect && def.backend_archive.is_none());
        let profile = root.join("separate profile");
        let zip = root.join("official archive.zip");
        let direct = Startup::from_options(
            root.clone(),
            Some(profile.clone()),
            [
                "--connect".into(),
                "--backend-archive".into(),
                zip.clone().into_os_string(),
            ],
        )
        .unwrap();
        assert!(direct.connect && !direct.ipc && !direct.import_legacy && direct.custom_data_root);
        assert_eq!(direct.install_root, root);
        assert_eq!(direct.profile_root, profile);
        assert_eq!(direct.backend_archive.as_deref(), Some(zip.as_path()));
        let nr = Startup::from_options(root.clone(), None, ["--connect-nr".into()]).unwrap();
        assert!(nr.connect && nr.connect_nr && !nr.ipc);
        for flags in [
            vec!["--connect", "--import-legacy"],
            vec!["--ipc-backend", "--connect"],
            vec!["--ipc-backend", "--connect-nr"],
            vec!["--backend-archive"],
        ] {
            assert!(
                Startup::from_options(root.clone(), None, flags.into_iter().map(Into::into))
                    .is_err()
            );
        }
    }
    #[test]
    fn settings_are_not_frame_confirmation() {
        let saved = json!({"settings":{"neuralRendering":true},"status":{"phase":"idle"}});
        assert_eq!(nr_display(&saved, false), "已选");
        assert_eq!(nr_display(&saved, true), "应用中");
        let live = json!({"settings":{"neuralRendering":true},"status":{"phase":"streaming","stats":{"width":1920,"height":1080,"neuralRendering":"on"}}});
        assert_eq!(nr_display(&live, false), "生效");
        let without_frame = json!({"settings":{"neuralRendering":true},"status":{"phase":"streaming","stats":{"neuralRendering":"on"}}});
        assert_eq!(nr_display(&without_frame, false), "等待画面");
        let unavailable = json!({"settings":{"neuralRendering":true},"status":{"phase":"streaming","stats":{"width":1920,"height":1080,"neuralRendering":"unavailable"}}});
        assert_eq!(nr_display(&unavailable, false), "未生效");
        let suspended = json!({"settings":{"neuralRendering":true},"status":{"phase":"streaming","renderFeedback":{"nrGuard":{"state":"suspended"}},"stats":{"width":1920,"height":1080,"neuralRendering":"off"}}});
        assert_eq!(nr_display(&suspended, false), "保护暂停");
        let off = json!({"settings":{"neuralRendering":false},"status":{"phase":"streaming","stats":{"width":1920,"height":1080,"neuralRendering":"off"}}});
        assert_eq!(nr_display(&off, false), "关闭");
    }
    #[test]
    fn toolbar_returns_at_upper_edge_and_stays_during_setup() {
        let idle = Duration::from_secs(8);
        assert!(!toolbar_visible(true, true, false, idle));
        assert!(toolbar_visible(true, true, true, idle));
        assert!(toolbar_visible(true, false, false, idle));
        assert!(toolbar_visible(false, true, false, idle));
        assert!(toolbar_visible(true, true, false, Duration::from_secs(2)));
    }
    #[test]
    fn preserve_input_aspect_when_sidebar_opens() {
        let area = Rect::from_min_size(pos2(0., 0.), vec2(640., 480.));
        let st = json!({"session":{"captureOnly":true},"stats":{"captureWidth":1920,"captureHeight":1080,"captureFps":59.94,"outputWidth":3840,"outputHeight":2160}});
        assert_eq!(input_geometry(&st), Some((1920, 1080, Some(59.94))));
        assert_eq!(
            fit_stage(area, Some((1920, 1080))),
            Rect::from_min_max(pos2(0., 60.), pos2(640., 420.))
        );
        assert_eq!(fit_stage(area, None), area);
    }
    #[test]
    fn actual_output_is_never_used_as_input() {
        assert!(
            input_geometry(&json!({"stats":{"outputWidth":3840,"outputHeight":2160}})).is_none()
        );
        assert_eq!(
            input_geometry(&json!({"stats":{"width":1920,"height":1080,"fps":0}})),
            Some((1920, 1080, None))
        );
    }
    #[test]
    fn native_stage_at_fractional_dpi() {
        let stage = Rect::from_min_max(pos2(3., 59.), pos2(637.8, 525.));
        assert_eq!(pixel_rect(stage, pos2(3., 5.), 1.25), (0, 68, 794, 582));
    }
    #[test]
    fn only_known_sony_pages_leave_the_player() {
        assert!(trusted_sony("https://ca.account.sony.com/api/v1/ssocookie"));
        assert!(trusted_sony(
            "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize?service_entity=psn"
        ));
        assert!(!trusted_sony(
            "https://ca.account.sony.com@other.example/api/v1/ssocookie"
        ));
        assert!(!trusted_sony("file:///C:/temp/page.html"));
    }
}
