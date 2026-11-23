use crate::{
    chiaki::{Open, Player, Stage, Update},
    design::*,
};
use egui::{
    vec2, Align, CentralPanel, Context, Frame, Layout, RichText, ScrollArea, TopBottomPanel,
};
use std::{path::PathBuf, time::Duration};

pub struct PublicApp {
    player: Player,
    note: String,
    failed: bool,
    progress: Option<f32>,
    offline_archive: bool,
    close_after_cancel: bool,
}
impl PublicApp {
    pub fn new(
        ctx: &Context,
        install: PathBuf,
        profile: Option<PathBuf>,
        archive: Option<PathBuf>,
        auto_connect: bool,
    ) -> Self {
        setup(ctx);
        let offline_archive = archive.is_some();
        let mut app = Self {
            player: Player::new(install, profile, archive),
            note: "尚未连接主机。".into(),
            failed: false,
            progress: None,
            offline_archive,
            close_after_cancel: false,
        };
        if auto_connect {
            app.open();
        }
        app
    }
    fn open(&mut self) {
        match self.player.open() {
            Ok(state) => {
                self.failed = false;
                self.note = match state {
                    Open::Preparing | Open::AlreadyPreparing => "正在检查串流组件……",
                    Open::Existing { raised: true } => "已切换到连接窗口。",
                    Open::Existing { raised: false } => {
                        "连接窗口正在运行，可从 Windows 任务栏切换到 chiaki-ng。"
                    }
                }
                .into();
            }
            Err(e) => {
                self.note = e.to_string();
                self.failed = true;
                self.progress = None;
            }
        }
    }
}
impl eframe::App for PublicApp {
    fn update(&mut self, ctx: &Context, _: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) && self.player.preparing() {
            self.close_after_cancel = true;
            self.player.cancel_preparation();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        match self.player.poll() {
            Ok(Some(Update::Cancelled)) => {
                self.note = "已取消后端准备。".into();
                self.failed = false;
                self.progress = None;
            }
            Ok(Some(Update::Progress(_))) if self.player.cancelling() => (),
            Ok(Some(Update::Progress(p))) => {
                self.progress = Some(p.done as f32 / p.total as f32);
                self.note = match p.stage {
                    Stage::Waiting => "另一实例正在准备串流组件，请稍候……".into(),
                    Stage::Download => format!(
                        "正在从 chiaki-ng 官方 GitHub 下载：{:.0} / {:.0} MiB",
                        p.done as f64 / 1048576.,
                        p.total as f64 / 1048576.
                    ),
                    Stage::Extract => format!("正在解压：{} / {} 个文件", p.done, p.total),
                    Stage::Verify => format!("正在校验组件：{} / {} 个文件", p.done, p.total),
                    Stage::Profile => "正在准备此 Windows 用户的独立配置……".into(),
                    Stage::Ready => "串流组件已校验。".into(),
                };
            }
            Ok(Some(Update::Opened)) => {
                self.note = "已打开 chiaki-ng。请在连接窗口中配对或选择你的 PS5。".into();
                self.progress = None;
                self.failed = false;
            }
            Ok(Some(Update::Closed(exit))) if exit.success() => {
                self.note = "连接窗口已关闭。可再次打开，选择已配对的 PS5。".into();
                self.failed = false;
            }
            Ok(Some(Update::Closed(exit))) => {
                let code = exit
                    .code()
                    .map(|n| format!("0x{:08X}", n as u32))
                    .unwrap_or_else(|| exit.to_string());
                self.note = format!(
                    "连接窗口异常退出（{code}）。请检查显卡驱动后重试；若持续退出，请记录此代码。"
                );
                self.failed = true;
            }
            Ok(None) => {}
            Err(e) => {
                self.note = e.to_string();
                self.progress = None;
                self.failed = true;
            }
        }
        if self.player.cancelling() {
            self.note = "正在取消准备并清理临时文件……".into();
        }
        if self.close_after_cancel && !self.player.preparing() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let running = self.player.running();
        let preparing = self.player.preparing();
        if preparing {
            ctx.request_repaint_after(Duration::from_millis(120));
        } else if running {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        TopBottomPanel::top("public_header")
            .frame(
                Frame::NONE
                    .fill(BAR)
                    .inner_margin(egui::Margin::symmetric(22, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(33., 33.), egui::Sense::hover());
                    brand(ui.painter(), r.center());
                    ui.add_space(5.);
                    ui.label(RichText::new("Relay").font(strong(20.)));
                    ui.label(RichText::new(" /  PS5 播放器").color(MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).color(DIM));
                    });
                });
            });
        TopBottomPanel::bottom("public_state")
            .frame(
                Frame::NONE
                    .fill(BAR)
                    .inner_margin(egui::Margin::symmetric(22, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("状态").color(if self.failed { WARN } else { ACCENT }));
                    ui.label(RichText::new(&self.note).color(if self.failed {
                        WARN
                    } else {
                        MUTED
                    }));
                });
            });
        CentralPanel::default().frame(Frame::NONE.fill(BG).inner_margin(egui::Margin::symmetric(28, 18))).show(ctx, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let compact = ui.available_height() < 445.;
                    ui.add_space(if compact { 2. } else { 12. });
                    let (art, _) = ui.allocate_exact_size(vec2(180., if compact { 60. } else { 83. }), egui::Sense::hover());
                    idle_art(ui.painter(), art.center(), if compact { 0.56 } else { 0.75 });
                    ui.heading(RichText::new("连接你的 PS5").font(strong(26.)));
                    ui.label(RichText::new("使用网络遥控游玩，无需采集卡。").color(MUTED));
                    ui.add_space(8.);
                    let caption = if preparing { "正在准备连接窗口…" } else if running { "返回连接窗口" } else { "配对 / 连接 PS5" };
                    if ui.add_enabled(!preparing, egui::Button::new(RichText::new(caption).font(strong(16.)).color(INK))
                        .fill(ACCENT).min_size(vec2(204., 41.)).corner_radius(7)).clicked() { self.open(); ctx.request_repaint(); }
                    if preparing {
                        if let Some(f) = self.progress { ui.add(egui::ProgressBar::new(f).desired_width(204.).fill(ACCENT)); }
                        if ui.add_enabled(!self.player.cancelling(), egui::Button::new("取消准备")).clicked() { self.player.cancel_preparation(); ctx.request_repaint(); }
                    } else {
                        let hint = if self.offline_archive { "使用指定的官方离线包；校验通过后打开。" } else { "首次自动从官方 GitHub 下载约 113 MiB；以后使用本地副本。" };
                        ui.label(RichText::new(hint).size(12.).color(DIM));
                    }
                    ui.add_space(if compact { 12. } else { 21. });
                    let width = ui.available_width().min(760.);
                    ui.allocate_ui_with_layout(vec2(width, 0.), Layout::top_down(Align::Min), |ui| {
                        Frame::NONE.fill(BAR).corner_radius(10).inner_margin(egui::Margin::symmetric(18, 13)).show(ui, |ui| {
                            ui.set_width((width - 36.).max(1.));
                            ui.label(RichText::new("首次配对").font(strong(16.)));
                            for (n, text) in [
                                ("1", "让电脑和 PS5 连接同一网络，在 PS5 上打开「设定 → 系统 → 远程游玩」，选择「启动远程游玩」。"),
                                ("2", "打开连接窗口并选择发现的 PS5。首次注册按提示确认账户；若需要配对码，在 PS5 上选择「配对设备」查看 8 位码。"),
                                ("3", "注册后选择主机开始播放。视频、音频、全屏和手柄设置均在 chiaki-ng 窗口中调整。"),
                            ] { ui.horizontal_top(|ui| { ui.label(RichText::new(n).font(strong(14.)).color(ACCENT)); ui.label(RichText::new(text).color(MUTED)); }); }
                        });
                    });
                });
            });
        });
    }
}
