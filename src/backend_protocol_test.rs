//! Runs a disposable backend using this test executable. No installed backend,
#[allow(dead_code)]
#[path = "playback.rs"]
mod playback;
// console, network, device or existing user data is required.
#[path = "backend.rs"]
mod backend;
#[allow(dead_code)]
#[path = "chiaki.rs"]
mod chiaki;
#[allow(dead_code)]
#[path = "chiaki_ui.rs"]
mod chiaki_ui;
#[allow(dead_code)]
#[path = "connections.rs"]
mod connections;
#[path = "wake.rs"]
mod wake;

use backend::{Backend, BackendEvent};
use serde_json::{json, Value};
use std::{
    env, fs,
    io::{self, BufRead, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    core::w,
    Win32::{
        Foundation::{HANDLE, HWND, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{
            OpenProcess, WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_SYNCHRONIZE,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, IsWindow, PeekMessageW, MSG,
            PM_REMOVE, WS_CHILD, WS_EX_NOPARENTNOTIFY, WS_POPUP,
        },
    },
};

struct TestChild(Child);
impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct TestWindow(HWND);
impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
fn hidden_window(parent: HWND) -> TestWindow {
    TestWindow(unsafe {
        CreateWindowExW(
            WS_EX_NOPARENTNOTIFY,
            w!("STATIC"),
            w!("Relay lifecycle fixture"),
            if parent.is_invalid() {
                WS_POPUP
            } else {
                WS_CHILD
            },
            0,
            0,
            16,
            16,
            parent,
            None,
            None,
            None,
        )
        .expect("hidden test window")
    })
}
fn assert_exited(handle: &OwnedHandle) {
    assert_eq!(
        unsafe { WaitForSingleObject(HANDLE(handle.as_raw_handle()), 5000) },
        WAIT_OBJECT_0
    );
}
fn await_pid(path: &Path) -> u32 {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(pid) = fs::read_to_string(path)
            .ok()
            .and_then(|v| v.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < end, "fixture did not start");
        thread::sleep(Duration::from_millis(10));
    }
}
fn fixture_host(root: PathBuf, profile: PathBuf, archive: PathBuf) -> io::Result<()> {
    let mut player = chiaki::Player::new(root, Some(profile), Some(archive));
    player.begin_stream_fixture(PROVIDER_FIXTURE, PROFILE_FIXTURE)?;
    assert!(matches!(wait_for_gui(&mut player)?, chiaki::Update::Ready));
    player.launch_stream("测试 PS5 '$&", "127.0.0.1")?;
    loop {
        thread::sleep(Duration::from_millis(20));
    }
}

fn fixture() -> io::Result<()> {
    let exe = env::current_exe()?;
    let cwd = env::current_dir()?;
    let install_root = PathBuf::from(env::var_os("RELAY_ROOT").expect("RELAY_ROOT"));
    let data = PathBuf::from(env::var_os("RELAY_DATA").expect("RELAY_DATA"));
    let host_pid = env::var("RELAY_HOST_PID").unwrap().parse::<u32>().unwrap();
    assert_eq!(install_root, cwd);
    assert_eq!(exe, cwd.join("backend/relay-backend.exe"));
    assert_eq!(env::args_os().count(), 1);
    let mut stdout = io::stdout().lock();
    let mut helper = None;
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let (id, name) = (&request["id"], request["name"].as_str().unwrap());
        let reply = match name {
            "init" => {
                assert!(helper.is_none() && request["args"] == json!({}));
                let child = Command::new(&exe)
                    .arg("--fixture-wait")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
                let child_pid = child.id();
                helper = Some(child);
                json!({"type":"state","state":{"ready":true,"status":{"phase":"idle"},"backendPid":std::process::id(),"child":child_pid,"host":host_pid,"data":data}})
            }
            "echo" => json!({"type":"result","id":id,"name":name,"value":request["args"]}),
            "exit" => {
                fs::write(data.join("graceful-exit"), b"exit received")?;
                break;
            }
            _ => json!({"type":"error","id":id,"name":name,"message":"unsupported"}),
        };
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
    }
    Ok(())
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn receive(backend: &Backend) -> BackendEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(event) = backend.try_recv() {
            if let BackendEvent::Exited(reason) = &event {
                panic!("fixture exited prematurely: {reason}");
            }
            return event;
        }
        assert!(Instant::now() < deadline, "timed out waiting for fixture");
        thread::sleep(Duration::from_millis(10));
    }
}
fn process_handle(pid: u32) -> OwnedHandle {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.unwrap();
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn run_contract() -> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = env::temp_dir().join(format!(
        "Relay-public-protocol-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path)?;
    let scratch = Scratch(path);
    let root = scratch.0.join("portable");
    let data = scratch.0.join("独立 profile/data");
    fs::create_dir(&root)?;
    fs::create_dir_all(&data)?;
    let missing = Backend::spawn(&root, &data)
        .err()
        .expect("missing backend must be explained");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert!(
        missing.to_string().contains("缺少兼容播放后端")
            && missing.to_string().contains("backend/relay-backend.exe")
    );
    fs::create_dir(root.join("backend"))?;
    fs::copy(env::current_exe()?, root.join("backend/relay-backend.exe"))?;
    let mut backend = Backend::spawn(&root, &data)?;
    let BackendEvent::State(state) = receive(&backend) else {
        panic!("expected initial state")
    };
    assert_eq!(state["ready"], true);
    assert_eq!(state["host"], std::process::id());
    assert_eq!(state["data"], json!(data));
    let pid = backend.playback_pid();
    assert_ne!(pid, 0);
    assert_eq!(state["backendPid"], pid);
    let observed = [
        process_handle(pid),
        process_handle(u32::try_from(state["child"].as_u64().unwrap())?),
    ];
    let args = json!({"message":"旋钮\n第二行"});
    let sent = backend.send("echo", args.clone())?;
    assert!(
        matches!(receive(&backend), BackendEvent::Result { id, name, value } if id == sent && name == "echo" && value == args)
    );
    let sent = backend.send("not-implemented", json!({}))?;
    assert!(
        matches!(receive(&backend), BackendEvent::Error { id, name, message } if id == sent && name == "not-implemented" && message == "unsupported")
    );
    backend.shutdown();
    assert_eq!(backend.playback_pid(), 0);
    assert_eq!(fs::read(data.join("graceful-exit"))?, b"exit received");
    for handle in observed {
        assert_eq!(
            unsafe { WaitForSingleObject(HANDLE(handle.as_raw_handle()), 5000) },
            WAIT_OBJECT_0
        );
    }
    println!("backend_protocol: missing backend, three environment variables, init, command results and errors, graceful exit and descendant cleanup passed");
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::args().nth(1).as_deref() == Some("--check-import") {
        let profile = PathBuf::from(env::args_os().nth(2).ok_or("missing profile")?);
        let connections = connections::Connections::new(&profile, false);
        assert!(
            connections.startup_error.is_none(),
            "imported vault did not validate"
        );
        let metadata = connections.metadata();
        let settings: Value =
            serde_json::from_slice(&fs::read(profile.join("data/public-playback.json"))?)?;
        assert!(
            settings
                .as_object()
                .is_some_and(|object| object.iter().all(|(k, v)| playback::valid(k, v))),
            "imported settings did not validate"
        );
        println!(
            "{}",
            json!({"vaultValidated":true,"hostCount":metadata["consoles"].as_array().unwrap().len(),"accountLoaded":metadata["account"].is_object(),"settingsValidated":true})
        );
        return Ok(());
    }
    if env::args().nth(1).as_deref() == Some("--fixture-wait") {
        thread::sleep(Duration::from_secs(45));
        return Ok(());
    }
    if env::args().nth(1).as_deref() == Some("--fixture-owned-host") {
        let args: Vec<_> = env::args_os().skip(2).map(PathBuf::from).collect();
        fixture_host(args[0].clone(), args[1].clone(), args[2].clone())?;
        return Ok(());
    }
    if env::args().nth(1).as_deref() == Some("--native-pair-fixture") {
        connections::write_native_fixture(Path::new(
            &env::args_os().nth(2).ok_or("missing fixture profile")?,
        ))?;
        return Ok(());
    }
    if env::current_exe()?.file_name() == Path::new("relay-backend.exe").file_name() {
        if env::args().nth(1).as_deref() == Some("--fixture-wait") {
            thread::sleep(Duration::from_secs(30));
            return Ok(());
        }
        fixture()?;
        return Ok(());
    }
    if env::current_exe()?.file_name() == Path::new("relay-profile.exe").file_name() {
        return gui_fixture();
    }
    run_contract()?;
    run_gui_contract()
}

// Only the two embedded PowerShell programs are replaced in this test. Launch, proof parsing,
// path handling, digest checks and process lifetime use the production implementation.
const PROVIDER_FIXTURE: &str = r#"param([string]$InstallRoot,[string]$ArchivePath)
if (![IO.File]::Exists($ArchivePath)) { throw 'Missing archive fixture' }
[IO.File]::WriteAllText([IO.Path]::Combine($InstallRoot,'archive-used'),$ArchivePath)
[Console]::WriteLine('RELAY verify 1 2')
"#;
const PROFILE_FIXTURE: &str = r#"param([string]$InstallRoot,[string]$ProfileRoot)
if (![IO.Directory]::Exists($ProfileRoot)) {throw 'Missing profile fixture'}
[IO.File]::WriteAllText([IO.Path]::Combine($InstallRoot,'profile-used'),$ProfileRoot)
$image = [IO.Path]::Combine($InstallRoot,'chiaki-ng','relay-profile.exe')
$sha = [Security.Cryptography.SHA256]::Create()
try { $hash = [BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($image))).Replace('-','').ToLowerInvariant() } finally { $sha.Dispose() }
[Console]::WriteLine('RELAY_PROFILE RTEST2 '+$hash)
"#;
const CANCEL_PROVIDER: &str = r#"param([string]$InstallRoot,[string]$ArchivePath)
$ev = [Threading.EventWaitHandle]::OpenExisting($env:RELAY_PREPARE_CANCEL)
try {
 [IO.File]::WriteAllText([IO.Path]::Combine($InstallRoot,'cancel-ready'),'ready')
 if (!$ev.WaitOne(8000)) { throw 'cancel was not signalled' }
} finally { $ev.Dispose(); [IO.File]::WriteAllText([IO.Path]::Combine($InstallRoot,'cancel-cleanup'),'done') }
"#;
const BAD_PROOF: &str = r#"param([string]$InstallRoot,[string]$ProfileRoot)
[Console]::WriteLine('RELAY_PROFILE RTEST2 '+('0' * 64))
"#;
const FAIL_INSTALL: &str = r#"param([string]$InstallRoot,[string]$ArchivePath)
[Console]::Error.WriteLine('test: invalid archive digest'); exit 1
"#;

fn gui_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    assert_eq!(env::current_exe()?, cwd.join("relay-profile.exe"));
    if env::args_os().count() != 1 {
        assert_eq!(
            env::args().skip(1).collect::<Vec<_>>(),
            [
                "--dualsense",
                "--exit-app-on-stream-exit",
                "--",
                "stream",
                "测试 PS5 '$&",
                "127.0.0.1"
            ]
        );
        println!("[I] Switched to profile 0, resolution: 1920x1080");
        println!(
            "RELAY_RENDER {}",
            json!({"type":"stats","width":1920,"height":1080,
            "bitrateMbps":83.75,"displayFps":117.42,"frameGeneration":"5","frameGenerationMultiplier":2,
            "upscalerState":"error","frameGenerationReasonCode":2,
            "pictureSource":"stream","gpu":{"adapterName":"Fixture GPU","driverVersion":"fixture-driver"}})
        );
        io::stdout().flush()?;
    }
    assert_eq!(
        env::var("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS").as_deref(),
        Ok("1")
    );
    for (name, value) in [
        ("SDL_JOYSTICK_HIDAPI", "1"),
        ("SDL_JOYSTICK_HIDAPI_PS5", "1"),
        ("SDL_JOYSTICK_HIDAPI_PS5_RUMBLE", "1"),
        ("SDL_JOYSTICK_RAWINPUT", "0"),
        ("SDL_AUDIODRIVER", "wasapi"),
    ] {
        assert_eq!(env::var(name).as_deref(), Ok(value));
    }
    for name in [
        "RELAY_ROOT",
        "RELAY_DATA",
        "RELAY_HOST_PID",
        "RELAY_PLAYER_ROOT",
    ] {
        assert!(env::var_os(name).is_none());
    }
    // Keep inherited output handles open, even if the renderer exits first.
    // The owner must end the whole private job before joining its pipe readers.
    let helper = Command::new(env::current_exe()?)
        .arg("--fixture-wait")
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    fs::write(cwd.join("fixture-helper"), helper.id().to_string())?;
    let mode = fs::read_to_string(cwd.join("fixture-mode")).unwrap_or_default();
    if mode == "protocol" {
        let stop_marker = cwd.join("fixture-stop");
        thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                if let Ok(line) = line {
                    if let Ok(command) = serde_json::from_str::<Value>(&line) {
                        if command["type"] == "stop" {
                            assert!(command["standby"].is_boolean());
                            fs::write(&stop_marker, command.to_string()).unwrap();
                            break;
                        }
                    }
                }
            }
        });
    }
    let window = if mode == "window" {
        let parent = fs::read_to_string(cwd.join("fixture-parent"))
            .ok()
            .and_then(|v| v.parse::<isize>().ok())
            .unwrap_or(0);
        Some(hidden_window(HWND(parent as _)))
    } else {
        None
    };
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(cwd.join("fixture-started"))?;
    writeln!(marker, "{}", std::process::id())?;
    let stop = cwd.join("fixture-exit");
    let until = Instant::now() + Duration::from_secs(10);
    while !stop.is_file() && Instant::now() < until {
        if mode == "protocol" && cwd.join("fixture-stop").is_file() {
            return Ok(());
        }
        if let Some(window) = &window {
            let mut msg = MSG::default();
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                if !IsWindow(window.0).as_bool() {
                    fs::write(cwd.join("fixture-graceful"), b"WM_CLOSE")?;
                    return Ok(());
                }
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(stop.is_file());
    Ok(())
}
fn wait_for_gui(player: &mut chiaki::Player) -> io::Result<chiaki::Update> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match player.poll()? {
            Some(chiaki::Update::Progress(p)) => assert!(p.done <= p.total),
            Some(done) => return Ok(done),
            None => (),
        }
        assert!(Instant::now() < deadline, "timed out waiting for player");
        thread::sleep(Duration::from_millis(10));
    }
}
fn run_gui_contract() -> Result<(), Box<dyn std::error::Error>> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let scratch = Scratch(env::temp_dir().join(format!(
        "Relay GUI 混合 ' $ [x] & {} {stamp}",
        std::process::id()
    )));
    let root = scratch.0.join("播放器");
    let storage = root.join("backends");
    let dir = storage.join("chiaki-ng");
    let profile = scratch.0.join("测试 profile");
    let zip = scratch.0.join("离线 archive.zip");
    fs::create_dir_all(&dir)?;
    fs::create_dir_all(&profile)?;
    fs::write(&zip, b"fixture only")?;
    // A second, unmodified upstream-like executable is deliberately present; it is never eligible to run.
    fs::write(dir.join("chiaki.exe"), b"not an eligible process")?;
    fs::copy(env::current_exe()?, dir.join("relay-profile.exe"))?;
    let mut player = chiaki::Player::new(root.clone(), Some(profile.clone()), Some(zip.clone()));
    assert!(matches!(
        player.begin_fixture(FAIL_INSTALL, PROFILE_FIXTURE)?,
        chiaki::Open::Preparing
    ));
    assert!(wait_for_gui(&mut player)
        .err()
        .expect("installer must fail")
        .to_string()
        .contains("invalid archive digest"));
    assert!(!storage.join("profile-used").exists() && !dir.join("fixture-started").exists());
    player.begin_fixture(CANCEL_PROVIDER, PROFILE_FIXTURE)?;
    let deadline = Instant::now() + Duration::from_secs(6);
    while !storage.join("cancel-ready").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    player.cancel_preparation();
    assert!(matches!(
        wait_for_gui(&mut player)?,
        chiaki::Update::Cancelled
    ));
    assert!(
        storage.join("cancel-cleanup").is_file()
            && !storage.join("profile-used").exists()
            && !dir.join("fixture-started").exists()
    );
    player.begin_fixture(PROVIDER_FIXTURE, BAD_PROOF)?;
    assert!(wait_for_gui(&mut player)
        .err()
        .expect("bad proof must fail")
        .to_string()
        .contains("校验不一致"));
    assert!(!dir.join("fixture-started").exists() && !player.running());
    player.begin_fixture(PROVIDER_FIXTURE, PROFILE_FIXTURE)?;
    assert!(matches!(player.open()?, chiaki::Open::AlreadyPreparing));
    assert!(matches!(wait_for_gui(&mut player)?, chiaki::Update::Opened));
    assert!(player.running() && !player.preparing());
    assert_eq!(
        Path::new(&fs::read_to_string(storage.join("archive-used"))?),
        zip
    );
    assert_eq!(
        Path::new(&fs::read_to_string(storage.join("profile-used"))?),
        profile
    );
    let until = Instant::now() + Duration::from_secs(5);
    while !dir.join("fixture-started").is_file() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(player.open()?, chiaki::Open::Existing { .. }));
    assert!(fs::OpenOptions::new()
        .write(true)
        .open(dir.join("relay-profile.exe"))
        .is_err());
    fs::write(dir.join("fixture-exit"), b"stop")?;
    assert!(matches!(wait_for_gui(&mut player)?, chiaki::Update::Closed(s) if s.success()));
    assert!(!player.running() && !root.join("data").exists());
    // Closing the host must stop its renderer and helpers, including when no window exists.
    fs::remove_file(dir.join("fixture-exit"))?;
    fs::remove_file(dir.join("fixture-started"))?;
    player.begin_stream_fixture(PROVIDER_FIXTURE, PROFILE_FIXTURE)?;
    assert!(matches!(wait_for_gui(&mut player)?, chiaki::Update::Ready));
    assert!(!player.running() && !dir.join("fixture-started").exists());
    player.launch_stream("测试 PS5 '$&", "127.0.0.1")?;
    let end = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Some(pid) = fs::read_to_string(dir.join("fixture-started"))
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
        {
            break pid;
        }
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(10));
    };
    let handle = process_handle(pid);
    let helper = process_handle(await_pid(&dir.join("fixture-helper")));
    let unrelated = TestChild(
        Command::new(env::current_exe()?)
            .arg("--fixture-wait")
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let until = Instant::now() + Duration::from_secs(3);
    while !player.video_received() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
    let feedback_until = Instant::now() + Duration::from_secs(3);
    while player.render_feedback()["stats"]["gpu"]["adapterName"] != "Fixture GPU" {
        assert!(Instant::now() < feedback_until);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        player.render_feedback()["stats"]["frameGeneration"],
        "dlssg-dx12"
    );
    assert_eq!(player.render_feedback()["stats"]["bitrateKbps"], 83750.);
    let shutdown_at = Instant::now();
    drop(player);
    assert!(shutdown_at.elapsed() < Duration::from_secs(4));
    assert_exited(&handle);
    assert_exited(&helper);
    assert_eq!(
        unsafe { WaitForSingleObject(HANDLE(unrelated.0.as_raw_handle()), 0) },
        WAIT_TIMEOUT
    );
    for embedded in [false, true] {
        for name in ["fixture-started", "fixture-helper", "fixture-graceful"] {
            let _ = fs::remove_file(dir.join(name));
        }
        fs::write(dir.join("fixture-mode"), b"window")?;
        let host = hidden_window(HWND::default());
        fs::write(
            dir.join("fixture-parent"),
            if embedded {
                (host.0 .0 as isize).to_string()
            } else {
                "0".into()
            },
        )?;
        let mut player =
            chiaki::Player::new(root.clone(), Some(profile.clone()), Some(zip.clone()));
        player.begin_stream_fixture(PROVIDER_FIXTURE, PROFILE_FIXTURE)?;
        assert!(matches!(wait_for_gui(&mut player)?, chiaki::Update::Ready));
        player.launch_stream("测试 PS5 '$&", "127.0.0.1")?;
        let renderer = process_handle(await_pid(&dir.join("fixture-started")));
        let helper = process_handle(await_pid(&dir.join("fixture-helper")));
        player.shutdown();
        player.shutdown(); // Explicit exit followed by Drop is harmless.
        assert!(!player.running());
        assert_exited(&renderer);
        assert_exited(&helper);
        assert_eq!(fs::read(dir.join("fixture-graceful"))?, b"WM_CLOSE");
    }
    for name in [
        "fixture-started",
        "fixture-helper",
        "fixture-mode",
        "fixture-parent",
    ] {
        let _ = fs::remove_file(dir.join(name));
    }
    for action in [
        "stop",
        "reconnect",
        "enhancement-retry",
        "shutdown",
        "unresponsive",
        "blocked",
    ] {
        for name in ["fixture-started", "fixture-helper", "fixture-stop"] {
            let _ = fs::remove_file(dir.join(name));
        }
        let protocol = !matches!(action, "unresponsive" | "blocked");
        fs::write(
            dir.join("fixture-mode"),
            if protocol { "protocol" } else { "unresponsive" },
        )?;
        let mut public =
            chiaki_ui::PublicPlayback::new(root.clone(), profile.clone(), true, Some(zip.clone()));
        let player = &mut public.player;
        player.begin_stream_fixture(PROVIDER_FIXTURE, PROFILE_FIXTURE)?;
        assert!(matches!(wait_for_gui(player)?, chiaki::Update::Ready));
        player.launch_stream("测试 PS5 '$&", "127.0.0.1")?;
        let renderer = process_handle(await_pid(&dir.join("fixture-started")));
        let helper = process_handle(await_pid(&dir.join("fixture-helper")));
        player.use_render_protocol_fixture();
        if action == "blocked" {
            let submitted = Instant::now();
            player.send_control(&json!({"type":"devices","padding":"x".repeat(60000)}))?;
            assert!(submitted.elapsed() < Duration::from_millis(100));
            thread::sleep(Duration::from_millis(50));
        }
        let stop_at = Instant::now();
        if action == "shutdown" {
            player.shutdown();
            assert!(!player.running());
        } else {
            if action == "enhancement-retry" {
                public.poll()?;
                assert!(public.settings_need_reconnect(&json!({"frameGenerationBackend":"nvidia"})));
                assert!(public.settings_need_reconnect(&json!({"upscaler":"artcnn"})));
                assert!(!public.settings_need_reconnect(&json!({"gamma":1.2})));
                assert!(!public
                    .settings_need_reconnect(&json!({"upscaler":public.settings["upscaler"]})));
                let before = public.settings.clone();
                public.settings["upscaler"] = json!("rtx-vsr");
                public.snapshot = json!({"stats":{"upscalerState":"error"}});
                assert_eq!(public.feedback()["upscaler"]["canRetry"], true);
                public.retry_enhancements()?;
                assert!(public.retry_enhancements().is_err());
                assert_eq!(public.feedback()["upscaler"]["canRetry"], false);
                assert!(public.snapshot["stats"].is_null());
                assert_eq!(public.settings["hdr"], before["hdr"]);
            } else if action == "reconnect" {
                player.close_for_reconnect()?;
            } else {
                player.close()?;
            }
            let player = &mut public.player;
            assert!(
                player.stopping()
                    && !player.video_received()
                    && player.render_feedback()["stats"].is_null()
            );
            assert!(matches!(wait_for_gui(player)?, chiaki::Update::Closed(_)));
        }
        assert!(stop_at.elapsed() < Duration::from_secs(3));
        assert_exited(&renderer);
        assert_exited(&helper);
        if protocol {
            let command: Value = serde_json::from_slice(&fs::read(dir.join("fixture-stop"))?)?;
            assert_eq!(
                command["standby"],
                !matches!(action, "reconnect" | "enhancement-retry")
            );
        }
        assert!(public.player.render_feedback()["stats"].is_null());
    }
    for name in [
        "fixture-started",
        "fixture-helper",
        "fixture-mode",
        "fixture-stop",
    ] {
        let _ = fs::remove_file(dir.join(name));
    }
    let mut host = TestChild(
        Command::new(env::current_exe()?)
            .arg("--fixture-owned-host")
            .args([&root, &profile, &zip])
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let renderer = process_handle(await_pid(&dir.join("fixture-started")));
    let helper = process_handle(await_pid(&dir.join("fixture-helper")));
    host.0.kill()?; // No Rust destructors or on_exit callback can run here.
    host.0.wait()?;
    assert_exited(&renderer);
    assert_exited(&helper);
    assert_eq!(
        unsafe { WaitForSingleObject(HANDLE(unrelated.0.as_raw_handle()), 0) },
        WAIT_TIMEOUT
    );
    println!("public_gui: install/cancellation, profile isolation, image lock, background SDL, graceful embedded and independent exit, bounded headless exit, descendant cleanup, abrupt host termination and unrelated-process isolation passed");
    Ok(())
}
