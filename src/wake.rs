//! Wake a paired LAN console and wait for Remote Play before starting its engine.
use relay_protocol::{base64, discovery as d};
use serde_json::Value;
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub const REST_HELP: &str = "请在 PS5「设定 → 系统 → 省电 → 待机模式中可使用的功能」开启「与互联网保持连接」和「允许通过网络启动 PS5」；确认电脑与主机在同一局域网。完全关机的主机无法通过网络唤醒。";

pub enum Event {
    Checking,
    Waking,
    Waiting,
    Ready(IpAddr),
    Failed(String),
}
pub struct WakeTask {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Event>,
}
impl WakeTask {
    #[cfg(test)]
    pub fn ready_fixture() -> Self {
        let (tx, rx) = mpsc::channel();
        tx.send(Event::Ready(Ipv4Addr::LOCALHOST.into())).unwrap();
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
        }
    }
    pub fn start(target: &Value) -> io::Result<Self> {
        let host = Host::parse(target)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("relay-wake".into())
            .spawn(move || {
                if let Err(e) = wait_ready(host, &stop, &tx, Timing::default()) {
                    if !stop.load(Ordering::Acquire) {
                        let _ = tx.send(Event::Failed(e.to_string()));
                    }
                }
            })?;
        Ok(Self { cancel, rx })
    }
    pub fn poll(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Event::Failed("主机唤醒检查已结束，请重试连接。".into()))
            }
        }
    }
}
impl Drop for WakeTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

pub fn credential(record: &Value) -> io::Result<u64> {
    let bytes = base64::decode(
        record["rp_regist_key"]
            .as_str()
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|_| io::Error::other("主机唤醒凭证无效，请重新配对此主机。"))?;
    if bytes.len() != 16 {
        return Err(io::Error::other("主机唤醒凭证无效，请重新配对此主机。"));
    }
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    if end == 0
        || bytes[..end].iter().any(|b| !b.is_ascii_hexdigit())
        || bytes[end..].iter().any(|b| *b != 0)
    {
        return Err(io::Error::other("主机唤醒凭证无效，请重新配对此主机。"));
    }
    u64::from_str_radix(std::str::from_utf8(&bytes[..end]).unwrap(), 16)
        .map_err(|_| io::Error::other("主机唤醒凭证无效，请重新配对此主机。"))
}
struct Host {
    id: String,
    address: String,
    ps5: bool,
    credential: u64,
}
impl Host {
    fn parse(record: &Value) -> io::Result<Self> {
        let id = record["hostId"]
            .as_str()
            .filter(|s| s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| io::Error::other("请选择有效的已配对主机。"))?;
        let address = record["ipAddress"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 253 && !s.chars().any(char::is_control))
            .ok_or_else(|| io::Error::other("请填写主机地址或重新搜索局域网。"))?;
        Ok(Self {
            id: id.to_ascii_lowercase(),
            address: address.into(),
            ps5: record["hostType"] != "PS4",
            credential: credential(record)?,
        })
    }
}
#[derive(Clone, Copy)]
struct Timing {
    timeout: Duration,
    interval: Duration,
    wake_retry: Duration,
    settle: Duration,
    direct_fallback: Duration,
    discovery_port: Option<u16>,
    session_port: u16,
    broadcast: bool,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            interval: Duration::from_millis(500),
            wake_retry: Duration::from_secs(3),
            settle: Duration::from_secs(1),
            direct_fallback: Duration::from_secs(2),
            discovery_port: None,
            session_port: 9295,
            broadcast: true,
        }
    }
}
fn packet(command: d::DiscoveryCmd, ps5: bool, credential: u64) -> Vec<u8> {
    let mut data = d::packet_fmt(&d::DiscoveryPacket {
        cmd: command,
        protocol_version: Some(
            if ps5 {
                d::DISCOVERY_PROTOCOL_VERSION_PS5
            } else {
                d::DISCOVERY_PROTOCOL_VERSION_PS4
            }
            .into(),
        ),
        user_credential: credential,
    })
    .unwrap()
    .into_bytes();
    data.push(0);
    data
}
fn wait_ready(
    host: Host,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<Event>,
    timing: Timing,
) -> io::Result<()> {
    let _ = tx.send(Event::Checking);
    // Resolution stays on the worker; stopping or closing the player never waits
    // for a Windows resolver or accepts a late result from a cancelled attempt.
    let initial = (host.address.as_str(), timing.session_port)
        .to_socket_addrs()
        .map_err(|_| io::Error::other("无法解析主机地址，请重新搜索局域网。"))?
        .find(|a| a.is_ipv6() == host.address.contains(':'))
        .ok_or_else(|| io::Error::other("无法解析主机地址，请重新搜索局域网。"))?;
    let bind_ip: IpAddr = if initial.is_ipv6() {
        "::".parse().unwrap()
    } else {
        Ipv4Addr::UNSPECIFIED.into()
    };
    let socket = (d::DISCOVERY_PORT_LOCAL_MIN..=d::DISCOVERY_PORT_LOCAL_MAX)
        .find_map(|port| UdpSocket::bind(SocketAddr::new(bind_ip, port)).ok())
        .or_else(|| UdpSocket::bind(SocketAddr::new(bind_ip, 0)).ok())
        .ok_or_else(|| io::Error::other("无法创建主机唤醒服务。"))?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    if initial.is_ipv4() {
        socket.set_broadcast(true)?;
    }
    let port = timing.discovery_port.unwrap_or(if host.ps5 {
        d::DISCOVERY_PORT_PS5
    } else {
        d::DISCOVERY_PORT_PS4
    });
    let search = packet(d::DiscoveryCmd::Srch, host.ps5, 0);
    let wake = packet(d::DiscoveryCmd::Wakeup, host.ps5, host.credential);
    let mut endpoint = SocketAddr::new(initial.ip(), port);
    let start = Instant::now();
    let mut next_search = Duration::ZERO;
    let mut next_wake = Duration::ZERO;
    let mut wakes = 0;
    let mut state = d::DiscoveryHostState::Unknown;
    let mut saw_standby = false;
    let mut conflicting_host = false;
    let mut ready_since = None::<Instant>;
    let mut last_service_check = None::<Instant>;
    let mut response = [0u8; 2048];
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let elapsed = start.elapsed();
        if elapsed >= timing.timeout {
            return Err(io::Error::other(format!(
                "{}{}",
                if saw_standby {
                    "PS5 唤醒等待超时，尚未进入可串流状态。"
                } else {
                    "未能找到可连接的主机，或远程游玩服务尚未就绪。"
                },
                REST_HELP
            )));
        }
        if elapsed >= next_search {
            let mut sent = socket.send_to(&search, endpoint).is_ok();
            // Broadcast carries no pairing credential. It also finds the same
            // paired console after DHCP changes its saved IP address.
            if timing.broadcast && endpoint.is_ipv4() {
                sent |= socket
                    .send_to(&search, SocketAddr::new(Ipv4Addr::BROADCAST.into(), port))
                    .is_ok();
            }
            if !sent {
                return Err(io::Error::other("无法发送主机状态请求，请检查网络。"));
            }
            next_search = elapsed + timing.interval;
        }
        match socket.recv_from(&mut response) {
            Ok((len, from)) if from.port() == port => {
                if let Ok(found) = d::srch_response_parse(from, &response[..len]) {
                    if found
                        .host_id
                        .as_ref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(&host.id))
                        && found.is_ps5() == host.ps5
                    {
                        conflicting_host = false;
                        endpoint = SocketAddr::new(from.ip(), port);
                        state = found.state;
                        if state == d::DiscoveryHostState::Standby {
                            if !saw_standby {
                                let _ = tx.send(Event::Waking);
                            }
                            saw_standby = true;
                            ready_since = None;
                        } else if state == d::DiscoveryHostState::Ready {
                            if ready_since.is_none() {
                                ready_since = Some(Instant::now());
                                let _ = tx.send(Event::Waiting);
                            }
                        }
                    } else if from.ip() == endpoint.ip() {
                        conflicting_host = true;
                    }
                }
            }
            Ok(_) => (),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        // Windows reports ICMP port-unreachable as a UDP reset
                        // while the console's discovery service is restarting.
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::Interrupted
                ) =>
            {
                ()
            }
            Err(_) => return Err(io::Error::other("读取主机状态失败，请检查网络。")),
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let unresponsive = state == d::DiscoveryHostState::Unknown
            && elapsed >= timing.direct_fallback
            && !conflicting_host;
        if (state == d::DiscoveryHostState::Standby || unresponsive)
            && wakes < 5
            && elapsed >= next_wake
        {
            if wakes == 0 && !saw_standby {
                let _ = tx.send(Event::Waking);
            }
            socket
                .send_to(&wake, endpoint)
                .map_err(|_| io::Error::other("发送唤醒请求失败，请检查网络。"))?;
            wakes += 1;
            next_wake = elapsed + timing.wake_retry;
        }
        let ready = state == d::DiscoveryHostState::Ready
            && (!saw_standby || ready_since.is_some_and(|at| at.elapsed() >= timing.settle));
        // Preserve direct connections on networks that filter UDP discovery.
        // Once standby is observed this shortcut is forbidden: wait for 200.
        let direct = state == d::DiscoveryHostState::Unknown
            && !saw_standby
            && !conflicting_host
            && elapsed >= timing.direct_fallback;
        if (ready || direct) && last_service_check.is_none_or(|at| at.elapsed() >= timing.interval)
        {
            last_service_check = Some(Instant::now());
            if TcpStream::connect_timeout(
                &SocketAddr::new(endpoint.ip(), timing.session_port),
                Duration::from_millis(200),
            )
            .is_ok()
                && !cancel.load(Ordering::Acquire)
            {
                let _ = tx.send(Event::Ready(endpoint.ip()));
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    const ID: &str = "020000000001";
    fn host() -> Host {
        Host {
            id: ID.into(),
            address: "127.0.0.1".into(),
            ps5: true,
            credential: 0x1234abcd,
        }
    }
    fn fast(port: u16, session: u16) -> Timing {
        Timing {
            timeout: Duration::from_millis(650),
            interval: Duration::from_millis(20),
            wake_retry: Duration::from_millis(30),
            settle: Duration::from_millis(30),
            direct_fallback: Duration::from_millis(80),
            discovery_port: Some(port),
            session_port: session,
            broadcast: false,
        }
    }
    struct Console {
        address: SocketAddr,
        service: TcpListener,
        stop: Arc<AtomicBool>,
        wakes: Arc<AtomicUsize>,
        searches: Arc<AtomicUsize>,
        worker: Option<thread::JoinHandle<()>>,
    }
    impl Console {
        // ready_after=None: forever in standby; 0: already awake; >0: retry N wake packets.
        fn new(ready_after: Option<usize>, id: &'static str) -> Self {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            let address = socket.local_addr().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            let service = TcpListener::bind("127.0.0.1:0").unwrap();
            service.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let halt = Arc::clone(&stop);
            let wakes = Arc::new(AtomicUsize::new(0));
            let wake_count = Arc::clone(&wakes);
            let searches = Arc::new(AtomicUsize::new(0));
            let search_count = Arc::clone(&searches);
            let worker = thread::spawn(move || {
                let mut buf = [0; 2048];
                while !halt.load(Ordering::Acquire) {
                    if let Ok((n, from)) = socket.recv_from(&mut buf) {
                        let text = String::from_utf8_lossy(&buf[..n]);
                        if text.starts_with("WAKEUP") {
                            assert!(text.contains("user-credential:305441741\n"));
                            assert!(text.contains("device-discovery-protocol-version:00030010"));
                            wake_count.fetch_add(1, Ordering::AcqRel);
                        } else if text.starts_with("SRCH") {
                            search_count.fetch_add(1, Ordering::AcqRel);
                            let ready = ready_after
                                .is_some_and(|n| wake_count.load(Ordering::Acquire) >= n);
                            let reply=format!("HTTP/1.1 {}\nhost-id:{id}\ndevice-discovery-protocol-version:00030010\nhost-request-port:9295\n\n",if ready{"200 Ok"}else{"620 Standby"});
                            socket.send_to(reply.as_bytes(), from).unwrap();
                        }
                    }
                }
            });
            Self {
                address,
                service,
                stop,
                wakes,
                searches,
                worker: Some(worker),
            }
        }
        fn timing(&self) -> Timing {
            fast(
                self.address.port(),
                self.service.local_addr().unwrap().port(),
            )
        }
    }
    impl Drop for Console {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.worker.take().unwrap().join().unwrap();
        }
    }
    #[test]
    fn standby_wakes_retries_and_waits_for_ready_before_opening_session() {
        let console = Console::new(Some(3), ID);
        let (tx, rx) = mpsc::channel();
        wait_ready(host(), &AtomicBool::new(false), &tx, console.timing()).unwrap();
        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|e| matches!(e, Event::Waking)));
        assert!(events.iter().any(|e| matches!(e, Event::Waiting)));
        assert!(matches!(events.last(), Some(Event::Ready(_))));
        assert!(console.wakes.load(Ordering::Acquire) >= 3);
        assert!(console.service.accept().is_ok());
    }
    #[test]
    fn ready_console_connects_without_wakeup_or_boot_delay() {
        let console = Console::new(Some(0), ID);
        let (tx, rx) = mpsc::channel();
        let start = Instant::now();
        wait_ready(host(), &AtomicBool::new(false), &tx, console.timing()).unwrap();
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(rx.try_iter().any(|e| matches!(e, Event::Ready(_))));
        assert_eq!(console.wakes.load(Ordering::Acquire), 0);
    }
    #[test]
    fn ready_discovery_still_waits_until_remote_play_tcp_service_is_listening() {
        let console = Console::new(Some(0), ID);
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let mut timing = console.timing();
        timing.session_port = port;
        let server = thread::spawn(move || {
            thread::sleep(Duration::from_millis(180));
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                if listener.accept().is_ok() {
                    return true;
                }
                thread::sleep(Duration::from_millis(5));
            }
            false
        });
        let (tx, rx) = mpsc::channel();
        let start = Instant::now();
        wait_ready(host(), &AtomicBool::new(false), &tx, timing).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(180));
        assert!(rx.try_iter().any(|e| matches!(e, Event::Ready(_))));
        assert!(server.join().unwrap());
    }
    #[test]
    fn standby_never_starts_engine_even_if_tcp_port_is_open() {
        let console = Console::new(None, ID);
        let (tx, rx) = mpsc::channel();
        let result =
            wait_ready(host(), &AtomicBool::new(false), &tx, console.timing()).unwrap_err();
        assert!(result.to_string().contains("待机模式"));
        assert!(!rx.try_iter().any(|e| matches!(e, Event::Ready(_))));
        assert!(
            console.service.accept().is_err(),
            "standby must never reach the session port"
        );
        assert!(console.wakes.load(Ordering::Acquire) <= 5);
    }
    #[test]
    fn rejects_another_consoles_discovery_reply_and_sends_no_credentials() {
        let console = Console::new(Some(0), "020000000099");
        let (tx, rx) = mpsc::channel();
        assert!(wait_ready(host(), &AtomicBool::new(false), &tx, console.timing()).is_err());
        assert_eq!(console.wakes.load(Ordering::Acquire), 0);
        assert!(!rx.try_iter().any(|e| matches!(e, Event::Ready(_))));
        assert!(console.service.accept().is_err());
    }
    #[test]
    fn stopping_wake_worker_returns_promptly_without_late_ready_or_more_packets() {
        let console = Console::new(None, ID);
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let timing = console.timing();
        let worker = thread::spawn(move || wait_ready(host(), &stop, &tx, timing));
        let until = Instant::now() + Duration::from_secs(1);
        while console.wakes.load(Ordering::Acquire) == 0 && Instant::now() < until {
            thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::Release);
        let start = Instant::now();
        worker.join().unwrap().unwrap();
        assert!(start.elapsed() < Duration::from_millis(300));
        assert!(!rx.try_iter().any(|e| matches!(e, Event::Ready(_))));
        let count = console.searches.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(60));
        assert_eq!(console.searches.load(Ordering::Acquire), count);
        assert!(console.service.accept().is_err());
    }
    #[test]
    fn direct_address_still_works_when_discovery_is_filtered() {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let service = TcpListener::bind("127.0.0.1:0").unwrap();
        service.set_nonblocking(true).unwrap();
        let (tx, rx) = mpsc::channel();
        wait_ready(
            host(),
            &AtomicBool::new(false),
            &tx,
            fast(
                udp.local_addr().unwrap().port(),
                service.local_addr().unwrap().port(),
            ),
        )
        .unwrap();
        assert!(rx.try_iter().any(|e| matches!(e, Event::Ready(_))));
        assert!(service.accept().is_ok());
    }
    #[test]
    fn discovery_service_can_restart_without_aborting_wake() {
        let reserved = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let service = TcpListener::bind("127.0.0.1:0").unwrap();
        service.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            thread::sleep(Duration::from_millis(180));
            let socket = UdpSocket::bind(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut buf = [0; 2048];
            loop {
                let (n, from) = socket.recv_from(&mut buf).unwrap();
                if buf[..n].starts_with(b"SRCH") {
                    let reply=format!("HTTP/1.1 200 Ok\nhost-id:{ID}\ndevice-discovery-protocol-version:00030010\n\n");
                    socket.send_to(reply.as_bytes(), from).unwrap();
                    return;
                }
            }
        });
        let (tx, rx) = mpsc::channel();
        let mut timing = fast(address.port(), service.local_addr().unwrap().port());
        timing.direct_fallback = Duration::from_secs(2);
        let start = Instant::now();
        wait_ready(host(), &AtomicBool::new(false), &tx, timing).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(180));
        assert!(rx.try_iter().any(|e| matches!(e, Event::Ready(_))));
        assert!(service.accept().is_ok());
        server.join().unwrap();
    }
    #[test]
    fn credential_is_nul_terminated_ascii_hex_not_raw_key_or_decimal() {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(b"1234abcd");
        assert_eq!(
            credential(&serde_json::json!({"rp_regist_key":base64::encode(&bytes)})).unwrap(),
            0x1234abcd
        );
        for bad in [&[0u8; 16][..], b"not-a-regist-key!", b"1234\0bad-padding"] {
            assert!(credential(&serde_json::json!({"rp_regist_key":base64::encode(bad)})).is_err());
        }
        let ps4 = packet(d::DiscoveryCmd::Wakeup, false, 123);
        assert!(
            String::from_utf8_lossy(&ps4).contains("device-discovery-protocol-version:00020020")
        );
    }
}
