//! Embeds the public playback window in host-client physical pixels and routes volume
//! exclusively to the process launched by the host. The backend must own its own
//! surface; process IDs supplied in JSON are never used for window or audio selection.
#[derive(Debug, Clone, Default)]
pub struct SurfaceStatus {
    pub embedded: bool,
    pub native_window: bool,
    pub audio_sessions: usize,
    pub video_message: String,
    pub audio_message: String,
}
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, Result as WinResult},
    Win32::{
        Foundation::{GetLastError, SetLastError, BOOL, ERROR_SUCCESS, HWND, LPARAM, POINT, RECT},
        Graphics::Gdi::ScreenToClient,
        Media::Audio::{
            eRender, AudioSessionStateExpired, IAudioSessionControl2, IAudioSessionManager2,
            IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        },
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_F11},
            WindowsAndMessaging::*,
        },
    },
};

// Part of the public playback contract. A title or a matching class in another
// process is never sufficient to select a window.
const PLAYBACK_CLASS: &str = "RelayPlaybackWindow";
type PixelRect = (i32, i32, i32, i32);

fn pid_of(window: HWND) -> u32 {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
    }
    pid
}
fn style_bits(window: HWND, index: WINDOW_LONG_PTR_INDEX) -> u32 {
    unsafe { GetWindowLongPtrW(window, index) as u32 }
}
fn set_style(window: HWND, index: WINDOW_LONG_PTR_INDEX, bits: u32) -> WinResult<()> {
    unsafe {
        SetLastError(ERROR_SUCCESS);
        let previous = SetWindowLongPtrW(window, index, (bits as i32) as isize);
        if previous == 0 && GetLastError() != ERROR_SUCCESS {
            return Err(windows::core::Error::from_win32());
        }
    }
    Ok(())
}
fn matches_class(window: HWND) -> bool {
    let mut name = [0u16; 128];
    let len = unsafe { GetClassNameW(window, &mut name) };
    len > 0
        && name[..len as usize]
            .iter()
            .copied()
            .eq(PLAYBACK_CLASS.encode_utf16())
}
fn matches_target(window: HWND, pid: u32) -> bool {
    !window.is_invalid()
        && unsafe { IsWindow(window).as_bool() }
        && pid > 0
        && pid_of(window) == pid
        && matches_class(window)
}
struct Search {
    pid: u32,
    result: Option<(HWND, u32)>,
}
unsafe extern "system" fn enum_window(window: HWND, cookie: LPARAM) -> BOOL {
    // EnumWindows is synchronous; cookie points to the caller's live, exclusive stack value.
    let search = unsafe { &mut *(cookie.0 as *mut Search) };
    let pid = pid_of(window);
    if pid == 0
        || pid != search.pid
        || !matches_class(window)
        || !unsafe { IsWindowVisible(window).as_bool() }
        || style_bits(window, GWL_STYLE) & WS_CHILD.0 != 0
        || unsafe { GetWindow(window, GW_OWNER) }.is_ok()
    {
        return BOOL(1);
    }
    if search.result.is_none() {
        search.result = Some((window, pid));
    }
    BOOL(1)
}
fn find_video(pid: u32) -> WinResult<Option<(HWND, u32)>> {
    let mut search = Search { pid, result: None };
    unsafe {
        EnumWindows(
            Some(enum_window),
            LPARAM(&mut search as *mut Search as isize),
        )?;
    }
    Ok(search.result)
}
fn parent_of(window: HWND) -> HWND {
    unsafe { GetParent(window) }.unwrap_or_default()
}
fn reparent(window: HWND, target: HWND) -> WinResult<()> {
    // windows-rs represents a null *previous* parent as an Error, even when SetParent
    // succeeded for a top-level window. Check the actual postcondition instead.
    let r = unsafe { SetParent(window, target) };
    let actual = parent_of(window);
    // A detached window still carrying WS_CHILD names the desktop as its parent
    // until we restore the original top-level styles. Both results are correct.
    if actual == target || (target.is_invalid() && actual == unsafe { GetDesktopWindow() }) {
        Ok(())
    } else {
        Err(r
            .err()
            .unwrap_or_else(|| windows::core::Error::from_win32()))
    }
}
fn embedded_style(original: u32) -> u32 {
    let strip = WS_CAPTION.0
        | WS_THICKFRAME.0
        | WS_POPUP.0
        | WS_SYSMENU.0
        | WS_MAXIMIZEBOX.0
        | WS_MINIMIZEBOX.0
        | WS_MAXIMIZE.0
        | WS_MINIMIZE.0;
    (original & !strip) | WS_CHILD.0 | WS_CLIPSIBLINGS.0
}
fn embedded_ex_style(original: u32) -> u32 {
    original
        & !(WS_EX_APPWINDOW.0
            | WS_EX_WINDOWEDGE.0
            | WS_EX_CLIENTEDGE.0
            | WS_EX_STATICEDGE.0
            | WS_EX_DLGMODALFRAME.0
            | WS_EX_TOPMOST.0)
}
struct Attached {
    hwnd: HWND,
    pid: u32,
    style: u32,
    ex_style: u32,
    placement: WINDOWPLACEMENT,
    rect: RECT,
    placed: Option<PixelRect>,
    hidden: bool,
}
impl Attached {
    fn attach(host: HWND, window: HWND, pid: u32) -> WinResult<Self> {
        if !matches_target(window, pid) || !parent_of(window).is_invalid() {
            return Err(windows::core::Error::from_win32());
        }
        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(window, &mut rect)?;
            GetWindowPlacement(window, &mut placement)?;
        }
        let item = Self {
            hwnd: window,
            pid,
            style: style_bits(window, GWL_STYLE),
            ex_style: style_bits(window, GWL_EXSTYLE),
            placement,
            rect,
            placed: None,
            hidden: false,
        };
        set_style(window, GWL_STYLE, embedded_style(item.style))?;
        if let Err(e) = set_style(window, GWL_EXSTYLE, embedded_ex_style(item.ex_style)) {
            let _ = set_style(window, GWL_STYLE, item.style);
            return Err(e);
        }
        if let Err(e) = reparent(window, host) {
            let _ = set_style(window, GWL_STYLE, item.style);
            let _ = set_style(window, GWL_EXSTYLE, item.ex_style);
            return Err(e);
        }
        Ok(item)
    }
    fn place(&mut self, rect: Option<PixelRect>) -> WinResult<()> {
        let same_style = style_bits(self.hwnd, GWL_STYLE);
        if same_style != embedded_style(same_style) {
            set_style(self.hwnd, GWL_STYLE, embedded_style(same_style))?;
            self.placed = None;
        }
        match rect {
            Some((x, y, w, h)) => {
                if self.hidden || self.placed != rect {
                    unsafe {
                        SetWindowPos(
                            self.hwnd,
                            HWND_TOP,
                            x,
                            y,
                            w,
                            h,
                            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
                        )?;
                    }
                    self.hidden = false;
                    self.placed = rect;
                }
            }
            None => {
                if !self.hidden {
                    unsafe {
                        SetWindowPos(
                            self.hwnd,
                            HWND_TOP,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOACTIVATE
                                | SWP_NOZORDER
                                | SWP_NOMOVE
                                | SWP_NOSIZE
                                | SWP_HIDEWINDOW,
                        )?;
                    }
                    self.hidden = true;
                    self.placed = None;
                }
            }
        }
        Ok(())
    }
    fn restore(self, host: HWND) {
        if !matches_target(self.hwnd, self.pid) || parent_of(self.hwnd) != host {
            return;
        }
        // Detaching one of our engine's windows must not disrupt a different HWND
        // reused by Windows. Never destroy foreign-owned handles or run code in the child.
        if reparent(self.hwnd, HWND::default()).is_err() {
            return;
        }
        let _ = set_style(self.hwnd, GWL_STYLE, self.style);
        let _ = set_style(self.hwnd, GWL_EXSTYLE, self.ex_style);
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOP,
                self.rect.left,
                self.rect.top,
                self.rect.right - self.rect.left,
                self.rect.bottom - self.rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
            );
            let _ = SetWindowPlacement(self.hwnd, &self.placement);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AudioTarget {
    pid: u32,
    level: u16,
}
enum AudioCommand {
    Target(AudioTarget),
    Quit,
}
#[derive(Default)]
struct AudioReport {
    count: usize,
    text: String,
}
struct ComApartment;
impl ComApartment {
    fn new() -> WinResult<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn discover_audio(
    devices: &IMMDeviceEnumerator,
    target: AudioTarget,
) -> WinResult<Vec<ISimpleAudioVolume>> {
    let mut controls = Vec::new();
    unsafe {
        let outputs = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for d in 0..outputs.GetCount()? {
            let Ok(endpoint) = outputs.Item(d) else {
                continue;
            };
            let Ok(manager) = endpoint.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else {
                continue;
            };
            for s in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(session) = sessions.GetSession(s) else {
                    continue;
                };
                if session.GetState().ok() == Some(AudioSessionStateExpired) {
                    continue;
                }
                let Ok(id) = session.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // GetProcessId's other success code denotes a session shared by
                // multiple processes. The high-level binding normalizes it to Ok.
                // Check S_OK explicitly; do not adjust a shared or system session.
                let mut pid = 0u32;
                let hr = (id.vtable().GetProcessId)(id.as_raw(), &mut pid);
                if hr.0 != 0 || pid == 0 || pid != target.pid || id.IsSystemSoundsSession().0 != 1 {
                    continue;
                }
                if let Ok(control) = session.cast::<ISimpleAudioVolume>() {
                    controls.push(control);
                }
            }
        }
    }
    Ok(controls)
}
fn write_audio(controls: &[ISimpleAudioVolume], level: f32) -> AudioReport {
    let mut ok = 0;
    let mut failed = None;
    let mut muted = false;
    for control in controls {
        let res = unsafe {
            control.GetMasterVolume().and_then(|cur| {
                if (cur - level).abs() < 0.0005 {
                    Ok(())
                } else {
                    control.SetMasterVolume(level, std::ptr::null())
                }
            })
        };
        match res {
            Ok(()) => {
                ok += 1;
                muted |= unsafe { control.GetMute() }.is_ok_and(|yes| yes.as_bool());
            }
            Err(e) => failed = Some(e),
        }
    }
    let text = if let Some(e) = failed {
        format!("音频会话更新失败：{e}")
    } else if ok == 0 {
        "等待播放器音频输出".into()
    } else if muted {
        format!("已调节 {ok} 个播放会话；Windows 混音器已将其中的会话静音")
    } else {
        format!("播放器会话音量 · {}%", (level * 100.).round() as i32)
    };
    AudioReport { count: ok, text }
}
fn audio_worker(rx: mpsc::Receiver<AudioCommand>, tx: mpsc::Sender<AudioReport>) {
    let _apartment = match ComApartment::new() {
        Ok(a) => a,
        Err(e) => {
            let _ = tx.send(AudioReport {
                count: 0,
                text: format!("音频会话初始化失败：{e}"),
            });
            return;
        }
    };
    let mut devices: Option<IMMDeviceEnumerator> = None;
    let mut controls = Vec::new();
    let mut target = AudioTarget {
        pid: 0,
        level: 1000,
    };
    let mut last_scan = None;
    loop {
        let cmd = if target.pid == 0 {
            rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(Duration::from_millis(750))
        };
        let mut next = match cmd {
            Ok(AudioCommand::Target(t)) => t,
            Err(mpsc::RecvTimeoutError::Timeout) => target,
            _ => break,
        };
        loop {
            match rx.try_recv() {
                Ok(AudioCommand::Target(t)) => next = t,
                Ok(AudioCommand::Quit) | Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        let changed_process = next.pid != target.pid;
        target = next;
        if changed_process {
            controls.clear();
            last_scan = None;
        }
        if target.pid == 0 {
            continue;
        }
        if devices.is_none() {
            devices = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok();
        }
        if last_scan.is_none_or(|t: Instant| t.elapsed() >= Duration::from_millis(700)) {
            match devices.as_ref().map(|d| discover_audio(d, target)) {
                Some(Ok(found)) => controls = found,
                Some(Err(e)) => {
                    controls.clear();
                    devices = None;
                    let _ = tx.send(AudioReport {
                        count: 0,
                        text: format!("音频输出设备不可用：{e}"),
                    });
                    continue;
                }
                None => {
                    let _ = tx.send(AudioReport {
                        count: 0,
                        text: "无法读取 Windows 混音器".into(),
                    });
                    continue;
                }
            }
            last_scan = Some(Instant::now());
        }
        let _ = tx.send(write_audio(&controls, target.level as f32 / 1000.));
    }
}

pub struct PlayerSurface {
    host: HWND,
    attached: Option<Attached>,
    last_search: Option<Instant>,
    native_mode: bool,
    external: Option<(HWND, u32)>,
    clip_added: bool,
    last_audio: Option<AudioTarget>,
    audio_tx: mpsc::Sender<AudioCommand>,
    audio_rx: mpsc::Receiver<AudioReport>,
    audio_worker: Option<thread::JoinHandle<()>>,
    shortcut_down: [bool; 2],
    pub status: SurfaceStatus,
}
impl PlayerSurface {
    pub fn new(host_hwnd: isize) -> Self {
        let host = HWND(host_hwnd as *mut _);
        let (cmd, rx) = mpsc::channel();
        let (tx, read) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("relay-session-volume".into())
            .spawn(move || audio_worker(rx, tx))
            .ok();
        Self {
            host,
            attached: None,
            last_search: None,
            native_mode: false,
            external: None,
            clip_added: false,
            last_audio: None,
            audio_tx: cmd,
            audio_rx: read,
            audio_worker: worker,
            shortcut_down: [false; 2],
            status: SurfaceStatus {
                video_message: "等待播放".into(),
                audio_message: "连接后应用音量".into(),
                ..Default::default()
            },
        }
    }
    fn valid_host(&self) -> bool {
        !self.host.is_invalid()
            && unsafe { IsWindow(self.host).as_bool() }
            && pid_of(self.host) == std::process::id()
    }
    fn detach(&mut self) {
        if let Some(child) = self.attached.take() {
            child.restore(self.host);
        }
        self.status.embedded = false;
    }
    pub fn set_native_window(&mut self, enabled: bool) {
        if self.native_mode != enabled {
            if enabled {
                self.detach();
                self.external = None;
            } else {
                // The next tick will revalidate the current PID, class and owner.
                self.external = None;
            }
            self.last_search = None;
            self.status.native_window = false;
            self.native_mode = enabled;
        }
    }
    fn clipped(&self, rect: Option<PixelRect>) -> Option<PixelRect> {
        let (x, y, w, h) = rect?;
        let mut client = RECT::default();
        unsafe {
            GetClientRect(self.host, &mut client).ok()?;
        }
        let l = x.max(0);
        let t = y.max(0);
        let r = x.saturating_add(w).min(client.right);
        let b = y.saturating_add(h).min(client.bottom);
        (r > l && b > t).then_some((l, t, r - l, b - t))
    }
    /// rect is (left, top, width, height) in host-client physical pixels. Pass None to
    /// hide an already-attached surface, e.g. while an over-video dialog is shown.
    pub fn tick(
        &mut self,
        backend_pid: u32,
        active: bool,
        rect: Option<PixelRect>,
        volume: f32,
    ) -> &SurfaceStatus {
        let playing = active && backend_pid > 0;
        self.status.embedded = false;
        self.status.native_window = false;
        if !playing || !self.valid_host() {
            self.external = None;
            self.detach();
            self.last_search = None;
            self.status.video_message = if playing {
                "播放器原生窗口尚未就绪"
            } else {
                "等待播放"
            }
            .into();
        } else if self.native_mode {
            // Keep the independently displayed window top-level without stealing focus.
            self.detach();
            self.external = self.external.filter(|(h, p)| {
                *p == backend_pid
                    && matches_target(*h, *p)
                    && style_bits(*h, GWL_STYLE) & WS_CHILD.0 == 0
            });
            if self.external.is_none()
                && self
                    .last_search
                    .is_none_or(|t| t.elapsed() >= Duration::from_millis(350))
            {
                self.last_search = Some(Instant::now());
                self.external = find_video(backend_pid).ok().flatten();
            }
            self.status.native_window = self.external.is_some();
            self.status.video_message = if self.status.native_window {
                "画面在独立窗口中播放。"
            } else {
                "等待引擎打开独立串流窗口。"
            }
            .into();
        } else {
            if self.attached.as_ref().is_some_and(|v| {
                !matches_target(v.hwnd, v.pid)
                    || v.pid != backend_pid
                    || parent_of(v.hwnd) != self.host
            }) {
                self.detach();
                self.last_search = None;
            }
            if self.attached.is_none()
                && self
                    .last_search
                    .is_none_or(|i| i.elapsed() >= Duration::from_millis(350))
            {
                self.last_search = Some(Instant::now());
                match find_video(backend_pid) {
                    Ok(Some((hwnd, pid))) => {
                        let host_style = style_bits(self.host, GWL_STYLE);
                        if host_style & WS_CLIPCHILDREN.0 == 0
                            && set_style(self.host, GWL_STYLE, host_style | WS_CLIPCHILDREN.0)
                                .is_ok()
                        {
                            self.clip_added = true;
                        }
                        match Attached::attach(self.host, hwnd, pid) {
                            Ok(child) => self.attached = Some(child),
                            Err(e) => self.status.video_message = format!("无法嵌入引擎画面：{e}"),
                        }
                    }
                    Ok(None) => self.status.video_message = "等待引擎建立播放窗口".into(),
                    Err(e) => self.status.video_message = format!("读取播放窗口失败：{e}"),
                }
            }
            let view = self.clipped(rect);
            if let Some(child) = self.attached.as_mut() {
                match child.place(view) {
                    Ok(()) => {
                        self.status.embedded = true;
                        self.status.video_message = if child.hidden {
                            "画面已暂时隐藏"
                        } else {
                            "原生画面已嵌入"
                        }
                        .into();
                    }
                    Err(e) => {
                        self.detach();
                        self.status.video_message = format!("调整画面失败：{e}");
                    }
                }
            }
        }
        let level = if volume.is_finite() {
            volume.clamp(0., 1.)
        } else {
            1.
        };
        let target = AudioTarget {
            pid: if playing { backend_pid } else { 0 },
            level: (level * 1000.).round() as u16,
        };
        if self.last_audio != Some(target) {
            let _ = self.audio_tx.send(AudioCommand::Target(target));
            self.last_audio = Some(target);
        }
        while let Ok(r) = self.audio_rx.try_recv() {
            self.status.audio_sessions = r.count;
            self.status.audio_message = r.text;
        }
        if !playing {
            self.status.audio_sessions = 0;
            self.status.audio_message = "连接后应用音量".into();
        }
        &self.status
    }
    /// Egui does not get pointer events over the cross-process child. Read the cursor
    /// only over the host or one of its children, never over other applications.
    pub fn cursor_in_host(&self) -> Option<(i32, i32)> {
        if !self.valid_host() {
            return None;
        }
        let mut p = POINT::default();
        unsafe {
            GetCursorPos(&mut p).ok()?;
            let under = WindowFromPoint(p);
            if under != self.host && !IsChild(self.host, under).as_bool() {
                return None;
            }
            if !ScreenToClient(self.host, &mut p).as_bool() {
                return None;
            }
            let mut r = RECT::default();
            GetClientRect(self.host, &mut r).ok()?;
            (p.x >= 0 && p.y >= 0 && p.x < r.right && p.y < r.bottom).then_some((p.x, p.y))
        }
    }
    /// Edge-triggered (F11, Escape) only when the playback child has keyboard focus.
    /// The host handles its usual keyboard events itself. No system-wide hotkey/hook.
    pub fn video_shortcuts(&mut self) -> (bool, bool) {
        let focused = self.attached.as_ref().is_some_and(|v| unsafe {
            let fg = GetForegroundWindow();
            if fg != self.host && GetAncestor(fg, GA_ROOT) != self.host {
                return false;
            }
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            GetGUIThreadInfo(0, &mut info).is_ok()
                && (info.hwndFocus == v.hwnd || IsChild(v.hwnd, info.hwndFocus).as_bool())
        });
        if !focused {
            self.shortcut_down = [false; 2];
            return (false, false);
        }
        let keys = unsafe {
            [
                GetAsyncKeyState(VK_F11.0 as i32) < 0,
                GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0,
            ]
        };
        let edges = (
            keys[0] && !self.shortcut_down[0],
            keys[1] && !self.shortcut_down[1],
        );
        self.shortcut_down = keys;
        edges
    }
    pub fn shutdown(&mut self) {
        self.detach();
        if self.clip_added && self.valid_host() {
            let _ = set_style(
                self.host,
                GWL_STYLE,
                style_bits(self.host, GWL_STYLE) & !WS_CLIPCHILDREN.0,
            );
            self.clip_added = false;
        }
        if let Some(worker) = self.audio_worker.take() {
            let _ = self.audio_tx.send(AudioCommand::Quit);
            let _ = worker.join();
        }
    }
}
impl Drop for PlayerSurface {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
