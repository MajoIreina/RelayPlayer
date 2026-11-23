//! Runs a disposable backend using this test executable. No installed backend,
//! console, network, device or existing user data is required.
#[path = "backend.rs"]
mod backend;
#[allow(dead_code)]
#[path = "chiaki.rs"]
mod chiaki;

use backend::{Backend, BackendEvent};
use serde_json::{json, Value};
use std::{
    env, fs,
    io::{self, BufRead, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
};

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
    assert_eq!(env::args_os().count(), 1);
    assert_eq!(
        env::var("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS").as_deref(),
        Ok("1")
    );
    for name in [
        "RELAY_ROOT",
        "RELAY_DATA",
        "RELAY_HOST_PID",
        "RELAY_PLAYER_ROOT",
    ] {
        assert!(env::var_os(name).is_none());
    }
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(cwd.join("fixture-started"))?;
    writeln!(marker, "{}", std::process::id())?;
    let stop = cwd.join("fixture-exit");
    let until = Instant::now() + Duration::from_secs(10);
    while !stop.is_file() && Instant::now() < until {
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
    // Closing the launcher must not disconnect a live session in the player's own window.
    fs::remove_file(dir.join("fixture-exit"))?;
    fs::remove_file(dir.join("fixture-started"))?;
    player.begin_fixture(PROVIDER_FIXTURE, PROFILE_FIXTURE)?;
    assert!(matches!(wait_for_gui(&mut player)?, chiaki::Update::Opened));
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
    drop(player);
    assert_eq!(
        unsafe { WaitForSingleObject(HANDLE(handle.as_raw_handle()), 50) },
        WAIT_TIMEOUT
    );
    fs::write(dir.join("fixture-exit"), b"stop")?;
    assert_eq!(
        unsafe { WaitForSingleObject(HANDLE(handle.as_raw_handle()), 5000) },
        WAIT_OBJECT_0
    );
    println!("public_gui: PowerShell failed-install stop, graceful cancellation and cleanup, checksum mismatch stop, UTF-16 path arguments, profile isolation handoff, image lock, single child, SDL background events, exit tracking and uninterrupted session after closing host passed");
    Ok(())
}
