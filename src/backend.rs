//! UTF-8 JSON lines over stdin/stdout of the local playback backend; no socket.
//! An implementation waits for `init` before starting helpers or playback. The
//! host sends `init` only after attaching the child to its kill-on-close job.
use serde_json::{json, Value};
use std::{
    cell::Cell,
    io::{self, BufRead, BufReader, Write},
    os::windows::{
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, WAIT_TIMEOUT},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{WaitForSingleObject, CREATE_NO_WINDOW},
        },
    },
};

#[derive(Debug)]
pub enum BackendEvent {
    State(Value),
    Result {
        id: u64,
        name: String,
        value: Value,
    },
    Error {
        id: u64,
        name: String,
        message: String,
    },
    Exited(String),
}

fn event(mut frame: Value) -> Option<BackendEvent> {
    match frame.get("type")?.as_str()? {
        "state" if frame.get("state")?.is_object() => {
            Some(BackendEvent::State(frame["state"].take()))
        }
        "result" => Some(BackendEvent::Result {
            id: frame["id"].as_u64()?,
            name: frame["name"].as_str()?.into(),
            value: frame["value"].take(),
        }),
        "error" => Some(BackendEvent::Error {
            id: frame["id"].as_u64()?,
            name: frame["name"].as_str()?.into(),
            message: frame["message"].as_str()?.into(),
        }),
        "fatal" => Some(BackendEvent::Exited(frame["message"].as_str()?.into())),
        _ => None,
    }
}

fn private_job() -> io::Result<OwnedHandle> {
    unsafe {
        let raw = CreateJobObjectW(None, PCWSTR::null()).map_err(io::Error::other)?;
        let job = OwnedHandle::from_raw_handle(raw.0);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as _,
            std::mem::size_of_val(&limits) as u32,
        )
        .map_err(io::Error::other)?;
        Ok(job)
    }
}

pub struct Backend {
    input: Option<SyncSender<Value>>,
    events: Receiver<BackendEvent>,
    next_id: Cell<u64>,
    worker: Option<Child>,
    job: Option<OwnedHandle>,
}
impl Backend {
    pub fn spawn(root: &Path, data: &Path) -> io::Result<Self> {
        // Both paths are absolute; only the data directory can be overridden by the user.
        let dir = root.join("backend");
        let exe = dir.join("relay-backend.exe");
        const MISSING: &str = "缺少兼容播放后端。请将后端放在播放器目录的 backend/relay-backend.exe，然后点击“重试播放后端”。";
        for (path, directory) in [(&dir, true), (&exe, false)] {
            let attrs = std::fs::symlink_metadata(path).map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    io::Error::new(e.kind(), MISSING)
                } else {
                    e
                }
            })?;
            if (directory && !attrs.is_dir()) || (!directory && !attrs.is_file()) {
                return Err(io::Error::new(io::ErrorKind::NotFound, MISSING));
            }
            if attrs.file_attributes() & 0x400 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "播放后端必须是 backend 目录中的本地普通文件。",
                ));
            }
        }
        let job = private_job()?;
        let mut worker = Command::new(exe)
            .current_dir(root)
            .creation_flags(CREATE_NO_WINDOW.0)
            .env("RELAY_ROOT", root)
            .env("RELAY_DATA", data)
            .env("RELAY_HOST_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if let Err(e) = unsafe {
            AssignProcessToJobObject(HANDLE(job.as_raw_handle()), HANDLE(worker.as_raw_handle()))
        } {
            let _ = worker.kill();
            let _ = worker.wait();
            return Err(io::Error::other(format!("无法管理播放器子进程：{e}")));
        }
        let mut stdin = worker.stdin.take().expect("piped stdin");
        let stdout = worker.stdout.take().expect("piped stdout");
        let (request_tx, request_rx) = mpsc::sync_channel::<Value>(32);
        let (event_tx, events) = mpsc::channel();
        thread::Builder::new()
            .name("relay-control".into())
            .spawn(move || {
                for frame in request_rx {
                    if serde_json::to_writer(&mut stdin, &frame).is_err()
                        || stdin.write_all(b"\n").and_then(|_| stdin.flush()).is_err()
                    {
                        break;
                    }
                }
                // Closing stdin also tears down a worker that is busy preparing a component.
            })?;
        thread::Builder::new()
            .name("relay-events".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let Some(e) = serde_json::from_str(&line).ok().and_then(event) else {
                        continue;
                    };
                    let fatal = matches!(e, BackendEvent::Exited(_));
                    if event_tx.send(e).is_err() || fatal {
                        return;
                    }
                }
                let _ = event_tx.send(BackendEvent::Exited("播放组件已退出。".into()));
            })?;
        let backend = Self {
            input: Some(request_tx),
            events,
            next_id: Cell::new(0),
            worker: Some(worker),
            job: Some(job),
        };
        backend.send("init", json!({}))?;
        Ok(backend)
    }
    pub fn send(&self, name: &str, args: Value) -> io::Result<u64> {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1));
        let pipe = self
            .input
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "播放组件已关闭"))?;
        pipe.try_send(json!({"id":id,"name":name,"args":args}))
            .map_err(|e| match e {
                TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "操作较多，请稍后再试")
                }
                TrySendError::Disconnected(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "播放组件已关闭")
                }
            })?;
        Ok(id)
    }
    /// IPC state cannot select an HWND or audio session belonging to another process.
    pub fn playback_pid(&self) -> u32 {
        self.worker
            .as_ref()
            .filter(|child| unsafe {
                WaitForSingleObject(HANDLE(child.as_raw_handle()), 0) == WAIT_TIMEOUT
            })
            .map_or(0, Child::id)
    }
    pub fn try_recv(&self) -> Option<BackendEvent> {
        self.events.try_recv().ok()
    }
    pub fn shutdown(&mut self) {
        let Some(mut child) = self.worker.take() else {
            return;
        };
        let _ = self.send("exit", json!({}));
        drop(self.input.take());
        let deadline = Instant::now() + Duration::from_millis(2400);
        while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
            thread::sleep(Duration::from_millis(15));
        }
        drop(self.job.take());
        let _ = child.wait();
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}
