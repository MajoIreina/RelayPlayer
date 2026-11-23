//! Runs a disposable backend using this test executable. No installed backend,
//! console, network, device or existing user data is required.
#[path = "backend.rs"]
mod backend;

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
    Foundation::{HANDLE, WAIT_OBJECT_0},
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
    run_contract()
}
