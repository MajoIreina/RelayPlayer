//! Bounded native control mailbox. Only the worker can block on the pipe.
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

const CAPACITY: usize = 32;
struct Message {
    bytes: Vec<u8>,
    settings: bool,
}
#[derive(Default)]
struct State {
    queue: VecDeque<Message>,
    stopping: bool,
    finish: bool,
    error: Option<io::ErrorKind>,
}
type Shared = Arc<(Mutex<State>, Condvar)>;
pub struct Control {
    shared: Shared,
    worker: Option<JoinHandle<()>>,
}
impl Control {
    pub fn start(mut output: impl Write + Send + 'static) -> io::Result<Self> {
        let shared: Shared = Arc::default();
        let state = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("render-control".into())
            .spawn(move || loop {
                let message = {
                    let (mutex, ready) = &*state;
                    let mut s = mutex.lock().unwrap();
                    while s.queue.is_empty() && !s.finish {
                        s = ready.wait(s).unwrap();
                    }
                    match s.queue.pop_front() {
                        Some(m) => m,
                        None => break,
                    }
                };
                if let Err(e) = output
                    .write_all(&message.bytes)
                    .and_then(|_| output.flush())
                {
                    let mut s = state.0.lock().unwrap();
                    s.error = Some(e.kind());
                    s.queue.clear();
                    break;
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    pub fn send(&self, value: &Value) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        if bytes.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "渲染命令过大。",
            ));
        }
        bytes.push(b'\n');
        let settings = value["type"] == "settings";
        let stop = value["type"] == "stop";
        let mut s = self.shared.0.lock().unwrap();
        if let Some(kind) = s.error {
            return Err(io::Error::new(kind, "渲染控制通道写入失败。"));
        }
        if s.finish || (s.stopping && !stop) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "渲染控制通道正在关闭。",
            ));
        }
        let message = Message { bytes, settings };
        if stop {
            // Stop supersedes queued edits; a later standby request can upgrade it.
            s.stopping = true;
            s.queue.clear();
            s.queue.push_front(message);
        } else if settings && s.queue.back().is_some_and(|m| m.settings) {
            *s.queue.back_mut().unwrap() = message;
        } else if s.queue.len() < CAPACITY {
            s.queue.push_back(message);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "渲染控制队列已满，请稍后重试。",
            ));
        }
        self.shared.1.notify_one();
        Ok(())
    }
    pub fn error(&self) -> Option<io::Error> {
        self.shared
            .0
            .lock()
            .unwrap()
            .error
            .map(|kind| io::Error::new(kind, "渲染控制通道写入失败。"))
    }
    // Flush queued stop, then close stdin. Session kills its job before join if stalled.
    pub fn finish(&self) {
        self.shared.0.lock().unwrap().finish = true;
        self.shared.1.notify_one();
    }
    pub fn join(&mut self) {
        self.finish();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    struct Blocked {
        began: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        bytes: Arc<Mutex<Vec<u8>>>,
        first: bool,
    }
    impl Write for Blocked {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.first {
                self.first = false;
                self.began.send(()).unwrap();
                self.release.recv().unwrap();
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn blocked_pipe_is_bounded_coalesces_edits_and_prioritizes_stop() {
        let (began, start) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut c = Control::start(Blocked {
            began,
            release: wait,
            bytes: bytes.clone(),
            first: true,
        })
        .unwrap();
        c.send(&json!({"type":"devices"})).unwrap();
        start.recv_timeout(Duration::from_secs(2)).unwrap();
        let at = Instant::now();
        for revision in 1..1000 {
            c.send(&json!({"type":"settings","revision":revision}))
                .unwrap();
        }
        assert_eq!(c.shared.0.lock().unwrap().queue.len(), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&c.shared.0.lock().unwrap().queue[0].bytes).unwrap()
                ["revision"],
            999
        );
        for _ in 1..CAPACITY {
            c.send(&json!({"type":"devices"})).unwrap();
        }
        assert_eq!(
            c.send(&json!({"type":"devices"})).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        c.send(&json!({"type":"stop","standby":false})).unwrap();
        c.send(&json!({"type":"stop","standby":true})).unwrap();
        assert!(c.send(&json!({"type":"settings"})).is_err());
        assert!(at.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        c.join();
        let output = bytes.lock().unwrap();
        let lines: Vec<Value> = output
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        assert_eq!(
            lines,
            vec![
                json!({"type":"devices"}),
                json!({"type":"stop","standby":true})
            ]
        );
    }
}
