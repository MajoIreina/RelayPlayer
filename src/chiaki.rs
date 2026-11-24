//! The upstream GUI speaks its own protocol. Verify its distribution before launching it;
//! the optional Relay JSON IPC backend has no part in this path.
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    os::windows::{
        ffi::OsStringExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::core::HSTRING;
use windows::Win32::{
    Foundation::{GetLastError, BOOL, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM},
    Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    },
    System::{
        JobObjects::AssignProcessToJobObject,
        SystemInformation::GetSystemDirectoryW,
        Threading::{CreateEventW, SetEvent, WaitForSingleObject, CREATE_NO_WINDOW},
    },
    UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GetParent, GetWindowTextLengthW, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindowAsync, SW_RESTORE,
        WM_CLOSE,
    },
};

const INSTALL_SCRIPT: &str = include_str!("../tools/ensure-chiaki.ps1");
const RENDER_INSTALL_SCRIPT: &str = include_str!("../tools/ensure-relay-render.ps1");
const PROFILE_SCRIPT: &str = include_str!("../tools/Initialize-ChiakiProfile.ps1");
const INSTALL_ROOT: &str = "RELAY_PREPARE_DIR";
const ARCHIVE: &str = "RELAY_PREPARE_ARCHIVE";
const DATA_ROOT: &str = "RELAY_PREPARE_PROFILE";
const NR: &str = "RELAY_PREPARE_NR";
const MODEL: &str = "RELAY_NR_MODEL";
const SETTINGS: &str = "RELAY_PLAYBACK_SETTINGS";
const CANCEL: &str = "RELAY_PREPARE_CANCEL";
// Only these constant instructions are parsed from the command line. The reviewed installer arrives
// over stdin from our executable. Paths stay OS strings in the child environment, never shell text.
const RUN_SCRIPT: &str = r#"$ErrorActionPreference='Stop';[Console]::InputEncoding=[Text.UTF8Encoding]::new($false);[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);try{$s=[ScriptBlock]::Create([Console]::In.ReadToEnd().TrimStart([char]0xfeff));$parameters=@{InstallRoot=$env:RELAY_PREPARE_DIR};if($env:RELAY_PREPARE_NR -eq '1'){$parameters.CommunityNr=$true};if($env:RELAY_RENDER_PREPARE -eq '1'){$parameters.RelayRender=$true};if($env:RELAY_PREPARE_ARCHIVE){$parameters.ArchivePath=$env:RELAY_PREPARE_ARCHIVE};if($env:RELAY_PREPARE_PROFILE){$parameters.ProfileRoot=$env:RELAY_PREPARE_PROFILE};& $s @parameters}catch{[Console]::Error.WriteLine('Could not run the embedded backend installer.');exit 1}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Waiting,
    Download,
    Extract,
    Verify,
    Profile,
    Ready,
}
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub stage: Stage,
    pub done: u64,
    pub total: u64,
}
impl Progress {
    fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_ascii_whitespace();
        if words.next()? != "RELAY" {
            return None;
        }
        let stage = match words.next()? {
            "waiting" => Stage::Waiting,
            "download" => Stage::Download,
            "extract" => Stage::Extract,
            "verify" => Stage::Verify,
            "ready" => Stage::Ready,
            _ => return None,
        };
        let done = words.next()?.parse().ok()?;
        let total = words.next()?.parse().ok()?;
        if total == 0 || done > total || words.next().is_some() {
            return None;
        }
        Some(Self { stage, done, total })
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
struct Proof([u8; 32]);
impl Proof {
    fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_ascii_whitespace();
        if words.next()? != "RELAY_PROFILE" {
            return None;
        }
        let ns = words.next()?;
        let hash = words.next()?.as_bytes();
        let suffix = ns.strip_prefix('R')?;
        if words.next().is_some()
            || suffix.len() != 5
            || !suffix
                .bytes()
                .all(|c| c.is_ascii_uppercase() || (b'2'..=b'7').contains(&c))
            || hash.len() != 64
        {
            return None;
        }
        let nibble = |c| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut digest = [0u8; 32];
        for (out, chars) in digest.iter_mut().zip(hash.chunks_exact(2)) {
            *out = nibble(chars[0])? * 16 + nibble(chars[1])?;
        }
        Some(Self(digest))
    }
}
enum InstallEvent {
    Progress(Progress),
    Finish(io::Result<Proof>),
}

struct Stop {
    event: OwnedHandle,
    name: String,
    requested: AtomicBool,
}
impl Stop {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!(r"Local\RelayPlayer.Prepare.{}.{}", std::process::id(), id);
        let handle = unsafe { CreateEventW(None, true, false, &HSTRING::from(name.as_str())) }
            .map_err(io::Error::other)?;
        let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let event = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        if existing {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "请重试后端准备。",
            ));
        }
        Ok(Self {
            event,
            name,
            requested: AtomicBool::new(false),
        })
    }
    fn set(&self) {
        self.requested.store(true, Ordering::Release);
        let _ = unsafe { SetEvent(HANDLE(self.event.as_raw_handle())) };
    }
    fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Standard,
    CommunityNr,
    RelayRender,
}
impl Provider {
    fn directory(self) -> &'static str {
        match self {
            Self::Standard => "chiaki-ng",
            Self::CommunityNr => "chiaki-nr",
            Self::RelayRender => "relay-render",
        }
    }
}
struct Preparation {
    provider: Provider,
    rx: Receiver<InstallEvent>,
    stop: Arc<Stop>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Preparation {
    fn drop(&mut self) {
        if let Some(thread) = self.worker.take() {
            if !thread.is_finished() {
                self.stop.set();
            }
            // Let both the provider's staging cleanup and the profile initializer's atomic write finish.
            let _ = thread.join();
        }
    }
}

fn powershell_path() -> io::Result<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    if n == 0 {
        return Err(io::Error::last_os_error());
    }
    if n >= buf.len() {
        return Err(io::Error::other("无法定位 Windows PowerShell。"));
    }
    Ok(PathBuf::from(OsString::from_wide(&buf[..n])).join("WindowsPowerShell/v1.0/powershell.exe"))
}
fn run_script(
    root: &Path,
    archive: Option<&Path>,
    profile: Option<&Path>,
    provider: Provider,
    model: Option<&Path>,
    settings: Option<&str>,
    script: &'static str,
    report: &Sender<InstallEvent>,
    stop: &Stop,
) -> io::Result<Option<Proof>> {
    if stop.requested() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "已取消准备。"));
    }
    let exe = powershell_path()?;
    let mut cmd = Command::new(&exe);
    cmd.current_dir(exe.parent().expect("system executable parent"))
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            RUN_SCRIPT,
        ])
        .env_remove("PSModulePath")
        .env(INSTALL_ROOT, root)
        .env(CANCEL, &stop.name)
        .env_remove(ARCHIVE)
        .env_remove(DATA_ROOT)
        .env_remove(NR)
        .env_remove(MODEL)
        .env_remove(SETTINGS)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if provider == Provider::RelayRender {
        cmd.env("RELAY_RENDER_PREPARE", "1");
    } else {
        cmd.env_remove("RELAY_RENDER_PREPARE");
    }
    if provider == Provider::CommunityNr {
        cmd.env(NR, "1");
        if let Some(path) = model {
            cmd.env(MODEL, path);
        }
    }
    if let Some(path) = archive {
        cmd.env(ARCHIVE, path);
    }
    if let Some(path) = profile {
        cmd.env(DATA_ROOT, path);
    }
    if let Some(value) = settings {
        cmd.env(SETTINGS, value);
    }
    let mut proc = cmd.spawn().map_err(|e| {
        io::Error::new(e.kind(), format!("无法启动系统 PowerShell 准备播放器：{e}"))
    })?;
    let mut input = proc.stdin.take().expect("piped stdin");
    let stderr = proc.stderr.take().expect("piped stderr");
    let output = proc.stdout.take().expect("piped stdout");
    // Drain both streams concurrently; never retain an unbounded transcript or print any URLs.
    let errors = thread::spawn(move || {
        let mut last = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !line.trim().is_empty() && !line.contains("://") {
                last = line
                    .trim()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(240)
                    .collect();
            }
        }
        last
    });
    let wrote = input.write_all(script.as_bytes());
    drop(input);
    let mut proof = None;
    for line in BufReader::new(output).lines().map_while(Result::ok) {
        if stop.requested() {
            break;
        }
        if let Some(progress) = Progress::parse(&line) {
            let _ = report.send(InstallEvent::Progress(progress));
        } else if let Some(value) = Proof::parse(&line) {
            proof = Some(value);
        }
    }
    let exit = proc.wait()?;
    let why = errors.join().unwrap_or_default();
    if !exit.success() {
        let detail = if why.is_empty() {
            String::new()
        } else {
            format!("（{why}）")
        };
        return Err(io::Error::other(format!(
            "后端准备失败。请检查 GitHub 网络、磁盘空间和播放器目录写入权限，然后重试。{detail}"
        )));
    }
    wrote?;
    Ok(proof)
}

// The caller and the optional IPC data directory must never be able to select an executable via PATH.
fn ordinary(root: &Path, relative: &[&str]) -> io::Result<PathBuf> {
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "播放器目录必须为绝对路径。",
        ));
    }
    let mut path = root.to_path_buf();
    for (i, part) in relative.iter().enumerate() {
        path.push(part);
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || (if i + 1 == relative.len() {
                !meta.is_file()
            } else {
                !meta.is_dir()
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "播放器路径不是普通本地文件或目录。",
            ));
        }
    }
    Ok(path)
}

struct Session {
    provider: Provider,
    child: Child,
    job: Option<OwnedHandle>,
    _image_lock: File,
    video_received: Arc<AtomicBool>,
    feedback: Arc<Mutex<serde_json::Value>>,
    stats_at: Arc<Mutex<Option<Instant>>>,
    closing_since: Option<Instant>,
    closing_standby: bool,
    revision: u64,
    drainers: Vec<JoinHandle<()>>,
}
// The embedded video has a foreign parent and is absent from EnumWindows.
// Walk that parent's children too, but message only roots belonging to our exact PID.
unsafe extern "system" fn close_owned_window(window: HWND, cookie: LPARAM) -> BOOL {
    let pid = cookie.0 as u32;
    let mut owner = 0;
    GetWindowThreadProcessId(window, Some(&mut owner));
    if owner == pid {
        let mut parent_pid = 0;
        if let Ok(parent) = GetParent(window) {
            GetWindowThreadProcessId(parent, Some(&mut parent_pid));
        }
        if parent_pid != pid {
            let _ = PostMessageW(window, WM_CLOSE, None, None);
        }
    }
    BOOL(1)
}
unsafe extern "system" fn close_owned_tree(window: HWND, cookie: LPARAM) -> BOOL {
    let _ = close_owned_window(window, cookie);
    let _ = EnumChildWindows(window, Some(close_owned_window), cookie);
    BOOL(1)
}
impl Session {
    fn request_close(&mut self, standby: bool) {
        if self.closing_since.is_some() && (!standby || self.closing_standby) {
            return;
        }
        self.closing_since = Some(Instant::now());
        self.closing_standby = standby;
        if self.provider == Provider::RelayRender {
            if let Some(input) = self.child.stdin.as_mut() {
                let _ = writeln!(input, "{{\"type\":\"stop\",\"standby\":{standby}}}")
                    .and_then(|_| input.flush());
            }
        } else {
            unsafe {
                let _ = EnumWindows(Some(close_owned_tree), LPARAM(self.child.id() as isize));
            }
        }
    }
    fn shutdown(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            if self.closing_since.is_none() {
                self.request_close(true);
            }
            // EOF also allows a headless or not-yet-created renderer to exit.
            self.child.stdin.take();
            unsafe {
                let remaining = Duration::from_millis(750)
                    .saturating_sub(self.closing_since.unwrap().elapsed());
                WaitForSingleObject(
                    HANDLE(self.child.as_raw_handle()),
                    remaining.as_millis() as u32,
                );
            }
        }
        // Close the private kill-on-close job before joining readers: a helper may
        // have inherited stdout even after the main renderer exited normally.
        self.job.take();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        for reader in self.drainers.drain(..) {
            let _ = reader.join();
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn video_packet_report(line: &str) -> bool {
    line.split_once("Switched to profile ")
        .is_some_and(|(_, rest)| {
            let Some((index, size)) = rest.split_once(", resolution: ") else {
                return false;
            };
            let Some((w, h)) = size.trim().split_once('x') else {
                return false;
            };
            index.parse::<u32>().is_ok()
                && w.parse::<u32>().is_ok_and(|v| v > 0 && v <= 16384)
                && h.parse::<u32>().is_ok_and(|v| v > 0 && v <= 16384)
        })
}
fn normalize_stats(mut stats: serde_json::Value) -> serde_json::Value {
    if stats["bitrateKbps"].is_null() {
        if let Some(mbps) = stats["bitrateMbps"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.)
        {
            stats["bitrateKbps"] = serde_json::json!(mbps * 1000.);
        }
    }
    if stats["outputFps"].is_null() {
        if let Some(fps) = stats["displayFps"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.)
        {
            stats["outputFps"] = serde_json::json!(fps);
        }
    }
    let legacy_backend = stats["frameGeneration"].as_i64().or_else(|| {
        stats["frameGeneration"]
            .as_str()
            .and_then(|v| v.parse::<i64>().ok())
    });
    if let Some(code) = legacy_backend {
        let backend = match code {
            0 => "off",
            1 => "dlssg",
            2 => "fsr",
            3 => "flow",
            4 => "nvidia-vfg",
            5 => "dlssg-dx12",
            _ => "unknown",
        };
        stats["frameGeneration"] = serde_json::json!(backend);
        stats["frameGenerationBackend"] = serde_json::json!(backend);
    }
    stats
}
fn stats_fresh(seen: Option<Instant>, stopping: bool) -> bool {
    !stopping && seen.is_some_and(|at| at.elapsed() < Duration::from_secs(4))
}
fn drain_feedback(
    output: impl Read + Send + 'static,
    flag: Arc<AtomicBool>,
    feedback: Arc<Mutex<serde_json::Value>>,
    stats_at: Arc<Mutex<Option<Instant>>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("relay-stream-feedback".into())
        .spawn(move || {
            let mut input = BufReader::new(output);
            // Keep no raw logs or credentials. Bound each line even for malformed output.
            loop {
                let mut line = Vec::new();
                let Ok(n) = input.by_ref().take(8193).read_until(b'\n', &mut line) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                if line.len() <= 8192 {
                    if let Some(raw) = std::str::from_utf8(&line)
                        .ok()
                        .and_then(|l| l.strip_prefix("RELAY_RENDER "))
                    {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
                            if matches!(
                                value["type"].as_str(),
                                Some(
                                    "stats"
                                        | "settingsAccepted"
                                        | "error"
                                        | "devices"
                                        | "pinRequired"
                                        | "sessionEnded"
                                )
                            ) {
                                let mut state = feedback.lock().unwrap();
                                if value["type"] == "stats" {
                                    if value["width"].as_u64().is_some_and(|n| n > 0)
                                        && value["height"].as_u64().is_some_and(|n| n > 0)
                                    {
                                        flag.store(true, Ordering::Release);
                                    }
                                    state["stats"] = normalize_stats(value);
                                    *stats_at.lock().unwrap() = Some(Instant::now());
                                } else {
                                    let kind = value["type"].as_str().unwrap_or("event").to_owned();
                                    state[&kind] = value.clone();
                                    if kind == "sessionEnded" && value["error"] == true {
                                        state["error"] = serde_json::json!({"type":"error","message":format!("串流会话已退出：{}",value["reason"].as_str().unwrap_or("未知原因"))});
                                    }
                                    state["event"] = value;
                                }
                            }
                        }
                    }
                }
                if line.len() <= 8192 && video_packet_report(&String::from_utf8_lossy(&line)) {
                    flag.store(true, Ordering::Release);
                }
                if line.last() != Some(&b'\n') {
                    loop {
                        line.clear();
                        let Ok(n) = input.by_ref().take(8193).read_until(b'\n', &mut line) else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        if line.last() == Some(&b'\n') {
                            break;
                        }
                    }
                }
            }
        })
        .expect("feedback reader thread")
}
pub struct Player {
    root: PathBuf,
    profile: Option<PathBuf>,
    archive: Option<PathBuf>,
    preparing: Option<Preparation>,
    session: Option<Session>,
    settings: Option<String>,
    prepare_only: bool,
    prepared: Option<(Proof, Provider)>,
    stream_args: Vec<OsString>,
    render_settings: serde_json::Value,
    last_feedback: serde_json::Value,
}
#[allow(dead_code)]
pub enum Open {
    Preparing,
    AlreadyPreparing,
    Existing { raised: bool },
}
pub enum Update {
    Progress(Progress),
    Opened,
    Ready,
    Closed(ExitStatus),
    Cancelled,
}

impl Player {
    pub fn new(root: PathBuf, profile: Option<PathBuf>, archive: Option<PathBuf>) -> Self {
        Self {
            root,
            profile,
            archive,
            preparing: None,
            session: None,
            settings: None,
            prepare_only: false,
            prepared: None,
            stream_args: Vec::new(),
            render_settings: serde_json::json!({}),
            last_feedback: serde_json::json!({}),
        }
    }
    pub fn running(&self) -> bool {
        self.session.is_some()
    }
    pub fn playback_pid(&self) -> u32 {
        self.session
            .as_ref()
            .map_or(0, |session| session.child.id())
    }
    pub fn video_received(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.closing_since.is_none() && s.video_received.load(Ordering::Acquire))
    }
    pub fn stopping(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.closing_since.is_some())
    }
    pub fn render_revision(&self) -> u64 {
        self.session.as_ref().map_or(0, |s| s.revision)
    }
    pub fn render_feedback(&self) -> serde_json::Value {
        let mut feedback = self
            .session
            .as_ref()
            .map(|s| s.feedback.lock().unwrap().clone())
            .unwrap_or_else(|| self.last_feedback.clone());
        let fresh = self
            .session
            .as_ref()
            .is_some_and(|s| stats_fresh(*s.stats_at.lock().unwrap(), s.closing_since.is_some()));
        if !fresh {
            let stale = self.session.is_some() && !feedback["stats"].is_null();
            feedback["stats"] = serde_json::Value::Null;
            feedback["telemetryStale"] = serde_json::json!(stale);
        }
        feedback
    }
    pub fn send_control(&mut self, command: &serde_json::Value) -> io::Result<()> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| io::Error::other("播放组件尚未启动。"))?;
        let input = session
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("渲染控制通道已关闭。"))?;
        writeln!(input, "{}", command)?;
        input.flush()
    }
    pub fn set_render_settings(&mut self, settings: &serde_json::Value) -> io::Result<()> {
        if let Some(session) = &mut self.session {
            session.revision += 1;
            let command = serde_json::json!({"type":"settings","revision":session.revision,"settings":settings});
            let input = session
                .child
                .stdin
                .as_mut()
                .ok_or_else(|| io::Error::other("渲染控制通道已关闭。"))?;
            writeln!(input, "{}", command)?;
            input.flush()?;
        }
        self.render_settings = settings.clone();
        Ok(())
    }
    pub fn set_playback_settings(&mut self, settings: String) -> io::Result<()> {
        if self.preparing() {
            return Err(io::Error::other("正在准备组件，请稍候。"));
        }
        self.settings = Some(settings);
        Ok(())
    }
    pub fn close(&mut self) -> io::Result<()> {
        self.close_with_standby(true)
    }
    pub fn close_for_reconnect(&mut self) -> io::Result<()> {
        self.close_with_standby(false)
    }
    fn close_with_standby(&mut self, standby: bool) -> io::Result<()> {
        if self.preparing() {
            self.cancel_preparation();
            return Ok(());
        }
        if let Some(session) = &mut self.session {
            session.request_close(standby);
        }
        Ok(())
    }
    pub fn shutdown(&mut self) {
        // End playback while its surface is still embedded; detaching first would
        // briefly restore a second, visible top-level playback window.
        if let Some(session) = &mut self.session {
            session.request_close(true);
        }
        self.session.take();
        self.preparing.take();
        self.prepared = None;
    }
    pub fn preparing(&self) -> bool {
        self.preparing.is_some()
    }
    pub fn cancel_preparation(&self) {
        if let Some(prep) = &self.preparing {
            prep.stop.set();
        }
    }
    pub fn cancelling(&self) -> bool {
        self.preparing.as_ref().is_some_and(|p| p.stop.requested())
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn open(&mut self) -> io::Result<Open> {
        self.begin(Provider::Standard, None, INSTALL_SCRIPT, PROFILE_SCRIPT)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn open_nr(&mut self, model: Option<PathBuf>) -> io::Result<Open> {
        self.begin(Provider::CommunityNr, model, INSTALL_SCRIPT, PROFILE_SCRIPT)
    }
    /// Provision without ever creating the upstream management UI.
    pub fn prepare_stream(&mut self, nr: bool) -> io::Result<()> {
        if self.running() || self.preparing() {
            return Err(io::Error::other("请先结束当前连接或组件准备。"));
        }
        self.prepare_only = true;
        self.prepared = None;
        let _ = nr;
        self.begin(Provider::RelayRender, None, INSTALL_SCRIPT, PROFILE_SCRIPT)?;
        Ok(())
    }
    pub fn launch_capture(&mut self, devices: bool) -> io::Result<()> {
        let (proof, provider) = self
            .prepared
            .take()
            .ok_or_else(|| io::Error::other("播放组件尚未准备。"))?;
        self.stream_args = vec![
            "--".into(),
            if devices {
                "relay-devices"
            } else {
                "relay-capture"
            }
            .into(),
        ];
        self.session = Some(self.start_verified(proof, provider)?);
        Ok(())
    }
    pub fn launch_stream(&mut self, nickname: &str, host: &str) -> io::Result<()> {
        if nickname.is_empty()
            || nickname.chars().any(char::is_control)
            || host.is_empty()
            || host.chars().any(char::is_control)
        {
            return Err(io::Error::other("主机名称或连接地址无效。"));
        }
        let (proof, provider) = self
            .prepared
            .take()
            .ok_or_else(|| io::Error::other("串流组件尚未校验。"))?;
        self.stream_args = vec![
            "--dualsense".into(),
            "--exit-app-on-stream-exit".into(),
            "--".into(),
            "stream".into(),
            nickname.into(),
            host.into(),
        ];
        self.session = Some(self.start_verified(proof, provider)?);
        Ok(())
    }
    fn begin(
        &mut self,
        provider: Provider,
        model: Option<PathBuf>,
        installer: &'static str,
        initializer: &'static str,
    ) -> io::Result<Open> {
        if let Some(prep) = &self.preparing {
            if prep.provider != provider {
                return Err(io::Error::other("请先取消当前串流组件的准备，再切换模式。"));
            }
            return Ok(Open::AlreadyPreparing);
        }
        if let Some(session) = &mut self.session {
            if session.child.try_wait()?.is_none() {
                if session.provider != provider {
                    return Err(io::Error::other("请先关闭当前连接窗口，再切换串流模式。"));
                }
                return Ok(Open::Existing {
                    raised: raise_window(session.child.id()),
                });
            }
            self.session.take();
        }
        let dest = self.root.join("backends");
        let archive = self.archive.clone();
        let profile = self.profile.clone();
        let settings = self.settings.clone();
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(Stop::new()?);
        let signal = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("relay-provider".into())
            .spawn(move || {
                let result = run_script(
                    &dest,
                    archive.as_deref(),
                    None,
                    provider,
                    model.as_deref(),
                    None,
                    installer,
                    &tx,
                    &signal,
                )
                .and_then(|_| {
                    if provider == Provider::RelayRender {
                        run_script(
                            &dest,
                            None,
                            profile.as_deref(),
                            provider,
                            None,
                            None,
                            RENDER_INSTALL_SCRIPT,
                            &tx,
                            &signal,
                        )?;
                    }
                    let _ = tx.send(InstallEvent::Progress(Progress {
                        stage: Stage::Profile,
                        done: 0,
                        total: 1,
                    }));
                    run_script(
                        &dest,
                        None,
                        profile.as_deref(),
                        provider,
                        model.as_deref(),
                        settings.as_deref(),
                        initializer,
                        &tx,
                        &signal,
                    )?
                    .ok_or_else(|| io::Error::other("未获得独立配置程序的校验值；连接窗口未启动。"))
                });
                let _ = tx.send(InstallEvent::Finish(result));
            })?;
        self.preparing = Some(Preparation {
            provider,
            rx,
            stop,
            worker: Some(worker),
        });
        Ok(Open::Preparing)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn begin_fixture(
        &mut self,
        installer: &'static str,
        initializer: &'static str,
    ) -> io::Result<Open> {
        self.begin(Provider::Standard, None, installer, initializer)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn begin_stream_fixture(
        &mut self,
        installer: &'static str,
        initializer: &'static str,
    ) -> io::Result<Open> {
        self.prepare_only = true;
        self.begin(Provider::Standard, None, installer, initializer)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn use_render_protocol_fixture(&mut self) {
        self.session.as_mut().expect("launched fixture").provider = Provider::RelayRender;
    }
    pub fn poll(&mut self) -> io::Result<Option<Update>> {
        if let Some(prep) = &self.preparing {
            let provider = prep.provider;
            let cancelled = prep.stop.requested();
            let mut latest = None;
            loop {
                match prep.rx.try_recv() {
                    Ok(InstallEvent::Progress(progress)) => {
                        latest = Some(Update::Progress(progress))
                    }
                    Ok(InstallEvent::Finish(result)) => {
                        self.preparing.take();
                        if cancelled {
                            return Ok(Some(Update::Cancelled));
                        }
                        let proof = result?;
                        if self.prepare_only {
                            self.prepared = Some((proof, provider));
                            return Ok(Some(Update::Ready));
                        }
                        self.session = Some(self.start_verified(proof, provider)?);
                        return Ok(Some(Update::Opened));
                    }
                    Err(TryRecvError::Empty) => return Ok(latest),
                    Err(TryRecvError::Disconnected) => {
                        self.preparing.take();
                        return Err(io::Error::other("后端准备任务提前结束，请重试。"));
                    }
                }
            }
        }
        if let Some(session) = &mut self.session {
            if session
                .closing_since
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(750))
            {
                session.shutdown();
            }
            if let Some(status) = session.child.try_wait()? {
                session.shutdown();
                self.last_feedback = session.feedback.lock().unwrap().clone();
                self.session.take();
                return Ok(Some(Update::Closed(status)));
            }
        }
        Ok(None)
    }
    fn start_verified(&self, proof: Proof, provider: Provider) -> io::Result<Session> {
        let stream = self.stream_args.len() == 6 && self.stream_args[3] == "stream";
        let capture = self.stream_args.len() == 2
            && matches!(
                self.stream_args[1].to_str(),
                Some("relay-capture" | "relay-devices")
            );
        if !cfg!(test) && !(stream || capture) {
            return Err(io::Error::other("未选择串流主机；禁止启动后端管理界面。"));
        }
        // The official executable itself uses a shared QSettings namespace. Only the agreed,
        // isolated launcher produced by verification is eligible here, never a fallback to chiaki.exe.
        let exe = ordinary(
            &self.root,
            &["backends", provider.directory(), "relay-profile.exe"],
        )?;
        // The active image has a single pathname shared by this installation. Holding this deny-write,
        // deny-delete handle across verification and CreateProcess prevents a concurrent initializer
        // from switching the image (and its bound Windows settings namespace) between the two.
        let mut image = File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&exe)?;
        let meta = image.metadata()?;
        if !meta.is_file()
            || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || meta.len() > 64 * 1024 * 1024
        {
            return Err(io::Error::other("独立配置程序大小或文件类型异常。"));
        }
        let mut data = Vec::with_capacity(meta.len() as usize);
        image.read_to_end(&mut data)?;
        let mut actual = [0u8; 32];
        // Windows 10+ provides an immutable pseudo-handle for the system SHA-256 provider.
        let result = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, &data, &mut actual) };
        if result.is_err() || actual != proof.0 {
            return Err(io::Error::other(
                "独立配置程序校验不一致；另一个实例可能正在切换配置。请稍后重试。",
            ));
        }
        let mut cmd = Command::new(&exe);
        cmd.args(&self.stream_args)
            .current_dir(exe.parent().expect("fixed executable has a parent"))
            .env("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1")
            // RawInput/XInput expose buttons and sticks but not DualSense motion sensors.
            // Select SDL's native PS5 HID path before the child initializes its controllers.
            .env("SDL_JOYSTICK_HIDAPI", "1")
            .env("SDL_JOYSTICK_HIDAPI_PS5", "1")
            .env("SDL_JOYSTICK_HIDAPI_PS5_RUMBLE", "1")
            .env("SDL_JOYSTICK_RAWINPUT", "0")
            // Keep this local to the stream process; do not change Windows device formats.
            .env("SDL_AUDIODRIVER", "wasapi")
            .env("QT_FORCE_STDERR_LOGGING", "1")
            .env("QT_QUICK_CONTROLS_MATERIAL_ACCENT", "#9aa8ff")
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Developer shells and other Qt apps can export their own plugin trees. Resolve
        // QML and platform plugins from this standalone player's verified runtime.
        for key in [
            "QT_QPA_PLATFORM",
            "QT_DEBUG_PLUGINS",
            "QT_PLUGIN_PATH",
            "QT_QPA_PLATFORM_PLUGIN_PATH",
            "QT_QPA_GENERIC_PLUGINS",
            "QML_IMPORT_PATH",
            "QML2_IMPORT_PATH",
            "RELAY_ROOT",
            "RELAY_DATA",
            "RELAY_HOST_PID",
            "RELAY_PLAYER_ROOT",
            INSTALL_ROOT,
            ARCHIVE,
            DATA_ROOT,
            CANCEL,
            NR,
            MODEL,
            SETTINGS,
        ] {
            cmd.env_remove(key);
        }
        if provider == Provider::RelayRender {
            let (namespace, _) = crate::connections::identity(self.profile.as_deref());
            cmd.env("RELAY_RENDER_NAMESPACE", namespace)
                .env("RELAY_RENDER_PROTOCOL", "1")
                // The private stdin command applies settings once, after init.
                .env_remove("RELAY_RENDER_SETTINGS");
        } else {
            cmd.env_remove("RELAY_RENDER_NAMESPACE")
                .env_remove("RELAY_RENDER_PROTOCOL")
                .env_remove("RELAY_RENDER_SETTINGS");
        }
        cmd.env("CHIAKI_VFX_PATH", exe.parent().unwrap().join("vfx"));
        let job = crate::backend::private_job()?;
        let mut child = cmd.spawn()?;
        if let Err(error) = unsafe {
            AssignProcessToJobObject(HANDLE(job.as_raw_handle()), HANDLE(child.as_raw_handle()))
        } {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(format!("无法管理播放组件进程：{error}")));
        }
        if provider == Provider::RelayRender {
            if let Some(input) = child.stdin.as_mut() {
                writeln!(
                    input,
                    "{}",
                    serde_json::json!({"type":"settings","revision":1,"settings":self.render_settings})
                )?;
                input.flush()?;
            }
        }
        let feedback = Arc::new(Mutex::new(serde_json::json!({})));
        let stats_at = Arc::new(Mutex::new(None));
        let video_received = Arc::new(AtomicBool::new(false));
        let mut drainers = Vec::new();
        if let Some(output) = child.stdout.take() {
            drainers.push(drain_feedback(
                output,
                Arc::clone(&video_received),
                Arc::clone(&feedback),
                Arc::clone(&stats_at),
            ));
        }
        if let Some(output) = child.stderr.take() {
            drainers.push(drain_feedback(
                output,
                Arc::clone(&video_received),
                Arc::clone(&feedback),
                Arc::clone(&stats_at),
            ));
        }
        Ok(Session {
            provider,
            child,
            job: Some(job),
            _image_lock: image,
            video_received,
            feedback,
            stats_at,
            closing_since: None,
            closing_standby: false,
            revision: 1,
            drainers,
        })
    }
}

#[cfg(test)]
#[allow(unused_imports)]
mod feedback_tests {
    use super::*;
    #[test]
    fn session_exit_reason_is_retained_without_claiming_video_was_received() {
        let feedback = Arc::new(Mutex::new(serde_json::json!({})));
        let received = Arc::new(AtomicBool::new(false));
        let event = serde_json::json!({"type":"sessionEnded","error":true,"reasonCode":8,"reason":"Control connection failed"});
        drain_feedback(
            io::Cursor::new(format!("RELAY_RENDER {event}\n").into_bytes()),
            Arc::clone(&received),
            Arc::clone(&feedback),
            Arc::new(Mutex::new(None)),
        )
        .join()
        .unwrap();
        let state = feedback.lock().unwrap();
        assert_eq!(state["sessionEnded"]["reasonCode"], 8);
        assert!(state["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Control connection failed"));
        assert!(!received.load(Ordering::Acquire));
    }
    #[test]
    fn diagnostic_samples_expire_and_are_invalid_while_stopping() {
        assert!(stats_fresh(Some(Instant::now()), false));
        assert!(!stats_fresh(None, false));
        assert!(!stats_fresh(
            Some(Instant::now() - Duration::from_secs(5)),
            false
        ));
        assert!(!stats_fresh(Some(Instant::now()), true));
    }
    #[test]
    fn renderer_reports_reach_host_with_actual_units_and_backend_names() {
        let feedback = Arc::new(Mutex::new(serde_json::json!({})));
        let at = Arc::new(Mutex::new(None));
        let received = Arc::new(AtomicBool::new(false));
        let line = serde_json::json!({"type":"stats","width":1920,"height":1080,
            "bitrateMbps":83.75,"displayFps":117.42,"frameGeneration":"5",
            "frameGenerationMultiplier":2,"pictureSource":"stream",
            "gpu":{"adapterName":"Test GPU","driverVersion":"test-driver",
                "dlssNrReported":true,"dlssNrSupported":false,"nrProbeState":"failed"}});
        let input = io::Cursor::new(format!("RELAY_RENDER {line}\n").into_bytes());
        drain_feedback(
            input,
            Arc::clone(&received),
            Arc::clone(&feedback),
            Arc::clone(&at),
        )
        .join()
        .unwrap();
        let report = feedback.lock().unwrap();
        let stats = &report["stats"];
        assert!(received.load(Ordering::Acquire) && at.lock().unwrap().is_some());
        assert_eq!(stats["bitrateKbps"], 83750.);
        assert_eq!(stats["outputFps"], 117.42);
        assert_eq!(stats["frameGeneration"], "dlssg-dx12");
        assert_eq!(stats["gpu"]["dlssNrSupported"], false);
        assert_eq!(stats["gpu"]["dlssNrReported"], true);
        assert_eq!(stats["pictureSource"], "stream");
        let only_targets =
            normalize_stats(serde_json::json!({"targetBitrateKbps":200000,"targetFps":240}));
        assert!(only_targets["bitrateKbps"].is_null() && only_targets["outputFps"].is_null());
    }
    #[test]
    fn native_pin_requests_are_delivered_to_player_controls() {
        let feedback = Arc::new(Mutex::new(serde_json::json!({})));
        let at = Arc::new(Mutex::new(None));
        let received = Arc::new(AtomicBool::new(false));
        for required in [true, false, true] {
            let line = serde_json::json!({"type":"pinRequired","required":required});
            drain_feedback(
                io::Cursor::new(format!("RELAY_RENDER {line}\n").into_bytes()),
                Arc::clone(&received),
                Arc::clone(&feedback),
                Arc::clone(&at),
            )
            .join()
            .unwrap();
            assert_eq!(
                feedback.lock().unwrap()["pinRequired"]["required"],
                required
            );
            assert!(!received.load(Ordering::Acquire));
        }
    }
    #[test]
    fn only_actual_video_packet_report_enables_playing_state() {
        assert!(video_packet_report(
            "[I] Switched to profile 0, resolution: 1920x1080\r\n"
        ));
        for line in [
            "Connected",
            "Video Profiles:",
            "  0: 1920x1080",
            "NR enabled",
            "Switched to profile 0, resolution: 0x1080",
            "Switched to profile 0, resolution: 1920x1080; injected",
        ] {
            assert!(!video_packet_report(line));
        }
    }
}

struct WindowSearch {
    pid: u32,
    window: Option<HWND>,
}
unsafe extern "system" fn pick(window: HWND, arg: LPARAM) -> BOOL {
    // EnumWindows invokes this synchronously with our live stack value.
    let search = unsafe { &mut *(arg.0 as *mut WindowSearch) };
    if search.window.is_none() {
        let mut owner = 0;
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut owner));
        }
        if owner == search.pid
            && unsafe { IsWindowVisible(window).as_bool() && GetWindowTextLengthW(window) > 0 }
        {
            search.window = Some(window);
        }
    }
    BOOL(1)
}
fn raise_window(pid: u32) -> bool {
    let mut search = WindowSearch { pid, window: None };
    unsafe {
        if EnumWindows(
            Some(pick),
            LPARAM((&mut search as *mut WindowSearch) as isize),
        )
        .is_err()
        {
            return false;
        }
        let Some(window) = search.window else {
            return false;
        };
        if IsIconic(window).as_bool() {
            let _ = ShowWindowAsync(window, SW_RESTORE);
        }
        SetForegroundWindow(window).as_bool()
    }
}
