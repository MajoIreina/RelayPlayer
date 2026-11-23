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
        Arc,
    },
    thread::{self, JoinHandle},
};
use windows::core::HSTRING;
use windows::Win32::{
    Foundation::{GetLastError, BOOL, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM},
    Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    },
    System::{
        SystemInformation::GetSystemDirectoryW,
        Threading::{CreateEventW, SetEvent, CREATE_NO_WINDOW},
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SetForegroundWindow, ShowWindowAsync, SW_RESTORE,
    },
};

const INSTALL_SCRIPT: &str = include_str!("../tools/ensure-chiaki.ps1");
const PROFILE_SCRIPT: &str = include_str!("../tools/Initialize-ChiakiProfile.ps1");
const INSTALL_ROOT: &str = "RELAY_PREPARE_DIR";
const ARCHIVE: &str = "RELAY_PREPARE_ARCHIVE";
const DATA_ROOT: &str = "RELAY_PREPARE_PROFILE";
const CANCEL: &str = "RELAY_PREPARE_CANCEL";
// Only these constant instructions are parsed from the command line. The reviewed installer arrives
// over stdin from our executable. Paths stay OS strings in the child environment, never shell text.
const RUN_SCRIPT: &str = r#"$ErrorActionPreference='Stop';[Console]::InputEncoding=[Text.UTF8Encoding]::new($false);[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);try{$s=[ScriptBlock]::Create([Console]::In.ReadToEnd().TrimStart([char]0xfeff));$parameters=@{InstallRoot=$env:RELAY_PREPARE_DIR};if($env:RELAY_PREPARE_ARCHIVE){$parameters.ArchivePath=$env:RELAY_PREPARE_ARCHIVE};if($env:RELAY_PREPARE_PROFILE){$parameters.ProfileRoot=$env:RELAY_PREPARE_PROFILE};& $s @parameters}catch{[Console]::Error.WriteLine('Could not run the embedded backend installer.');exit 1}"#;

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

#[derive(PartialEq, Eq, Debug)]
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
struct Preparation {
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
        .env(INSTALL_ROOT, root)
        .env(CANCEL, &stop.name)
        .env_remove(ARCHIVE)
        .env_remove(DATA_ROOT)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = archive {
        cmd.env(ARCHIVE, path);
    }
    if let Some(path) = profile {
        cmd.env(DATA_ROOT, path);
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
    child: Child,
    _image_lock: File,
}
pub struct Player {
    root: PathBuf,
    profile: Option<PathBuf>,
    archive: Option<PathBuf>,
    preparing: Option<Preparation>,
    session: Option<Session>,
}
pub enum Open {
    Preparing,
    AlreadyPreparing,
    Existing { raised: bool },
}
pub enum Update {
    Progress(Progress),
    Opened,
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
        }
    }
    pub fn running(&self) -> bool {
        self.session.is_some()
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
    pub fn open(&mut self) -> io::Result<Open> {
        self.begin(INSTALL_SCRIPT, PROFILE_SCRIPT)
    }
    fn begin(&mut self, installer: &'static str, initializer: &'static str) -> io::Result<Open> {
        if self.preparing.is_some() {
            return Ok(Open::AlreadyPreparing);
        }
        if let Some(session) = &mut self.session {
            if session.child.try_wait()?.is_none() {
                return Ok(Open::Existing {
                    raised: raise_window(session.child.id()),
                });
            }
            self.session.take();
        }
        let dest = self.root.join("backends");
        let archive = self.archive.clone();
        let profile = self.profile.clone();
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(Stop::new()?);
        let signal = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("relay-provider".into())
            .spawn(move || {
                let result = run_script(&dest, archive.as_deref(), None, installer, &tx, &signal)
                    .and_then(|_| {
                        let _ = tx.send(InstallEvent::Progress(Progress {
                            stage: Stage::Profile,
                            done: 0,
                            total: 1,
                        }));
                        run_script(&dest, None, profile.as_deref(), initializer, &tx, &signal)?
                            .ok_or_else(|| {
                                io::Error::other("未获得独立配置程序的校验值；连接窗口未启动。")
                            })
                    });
                let _ = tx.send(InstallEvent::Finish(result));
            })?;
        self.preparing = Some(Preparation {
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
        self.begin(installer, initializer)
    }
    pub fn poll(&mut self) -> io::Result<Option<Update>> {
        if let Some(prep) = &self.preparing {
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
                        self.session = Some(self.start_verified(result?)?);
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
            if let Some(status) = session.child.try_wait()? {
                self.session.take();
                return Ok(Some(Update::Closed(status)));
            }
        }
        Ok(None)
    }
    fn start_verified(&self, proof: Proof) -> io::Result<Session> {
        // The official executable itself uses a shared QSettings namespace. Only the agreed,
        // isolated launcher produced by verification is eligible here, never a fallback to chiaki.exe.
        let exe = ordinary(&self.root, &["backends", "chiaki-ng", "relay-profile.exe"])?;
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
        cmd.current_dir(exe.parent().expect("fixed executable has a parent"))
            .env("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1")
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Developer shells and other Qt apps can export their own plugin trees. Resolve
        // QML and platform plugins from this standalone player's verified runtime.
        for key in [
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
        ] {
            cmd.env_remove(key);
        }
        let child = cmd.spawn()?;
        Ok(Session {
            child,
            _image_lock: image,
        })
    }
    // Closing Relay only closes the tracking handles; a running player's session continues.
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
