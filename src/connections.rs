//! Relay's own discovery, browser authorization and pairing. No upstream GUI is opened.
use relay_protocol::{base64, discovery as d, regist as r, Target};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::Duration,
};
use windows::{
    core::w,
    Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::Cryptography::*,
    },
};
use winreg::{enums::*, RegKey, RegValue};

const BINDING: &str = "RelayPlayerProfileBindingV1";
const AUTHORIZE: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize";
const TOKEN: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token";
const REDIRECT: &str = "https://remoteplay.dl.playstation.net/remoteplay/redirect";
const CLIENT: &str = "ba495a24-818c-472b-b12d-ff231c1b5745";
// Public Remote Play application identifier, as published by chiaki-ng; not a user secret.
const APP_SECRET: &str = "mvaiZkRsAsI1IBkY";
const SCOPE: &str = "psn:clientapp referenceDataService:countryConfig.read pushNotification:webSocket.desktop.connect sessionManager:remotePlaySession.system.update";

pub struct Connections {
    store: Arc<Mutex<Value>>,
    vault: PathBuf,
    namespace: String,
    binding: String,
    rx: mpsc::Receiver<(String, Result<Value, String>)>,
    tx: mpsc::Sender<(String, Result<Value, String>)>,
    pub operation: String,
    pub startup_error: Option<String>,
}
fn error(text: &str) -> io::Error {
    io::Error::other(text.to_owned())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn identity(profile: Option<&Path>) -> (String, String) {
    let identity = profile.map_or_else(
        || "default".into(),
        |p| {
            format!(
                "root:{}",
                p.to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .trim_end_matches(['\\', '/'])
                    .replace('/', "\\")
                    .to_uppercase()
            )
        },
    );
    let hash = Sha256::digest(format!("RelayPlayer.Chiaki.Windows.profile.v1:{identity}"));
    let packed = u32::from_be_bytes(hash[..4].try_into().unwrap());
    let mut slug = String::from("R");
    for shift in [27, 22, 17, 12, 7] {
        slug.push(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"[((packed >> shift) & 31) as usize] as char);
    }
    (slug, hex(&hash))
}
fn protect(input: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    if input.len() > 4 * 1024 * 1024 {
        return Err(error("授权存储大小异常。"));
    }
    let blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        let result = if encrypt {
            CryptProtectData(
                &blob,
                w!("RelayPlayer connections"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &blob,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        result.map_err(|_| error("无法读取此 Windows 用户的授权存储；原文件已保留。"))?;
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out.pbData.cast()));
        Ok(bytes)
    }
}
fn save(path: &Path, store: &Value) -> io::Result<()> {
    validate_store(store)?;
    let bytes = protect(store.to_string().as_bytes(), true)?;
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}
fn valid_text(value: &Value, max: usize, empty: bool) -> bool {
    value.as_str().is_some_and(|s| {
        (empty || !s.is_empty()) && s.len() <= max && !s.chars().any(char::is_control)
    })
}
fn validate_store(store: &Value) -> io::Result<()> {
    let invalid = || error("授权存储格式无效；原文件已保留。");
    let object = store.as_object().ok_or_else(invalid)?;
    let hosts = object
        .get("hosts")
        .and_then(Value::as_array)
        .filter(|h| h.len() <= 256)
        .ok_or_else(invalid)?;
    let mut ids = std::collections::HashSet::new();
    for h in hosts {
        let mac = base64::decode(h["server_mac"].as_str().unwrap_or_default().as_bytes())
            .map_err(|_| invalid())?;
        if mac.len() != 6
            || h["hostId"].as_str() != Some(hex(&mac).as_str())
            || !ids.insert(hex(&mac))
            || !valid_text(&h["hostName"], 256, false)
            || !valid_text(&h["ipAddress"], 253, true)
            || !matches!(h["hostType"].as_str(), Some("PS5" | "PS4"))
            || h["target"].as_u64().is_none_or(|v| v > u32::MAX as u64)
            || h["rp_key_type"]
                .as_u64()
                .is_none_or(|v| v > u32::MAX as u64)
            || decode16(h, "rp_key").is_err()
            || decode16(h, "rp_regist_key").is_err()
        {
            return Err(invalid());
        }
    }
    if let Some(account) = object.get("account").filter(|v| !v.is_null()) {
        let user = account["userId"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(invalid)?;
        if account["accountIdBase64"].as_str() != Some(base64::encode(&user.to_le_bytes()).as_str())
            || !valid_text(&account["onlineId"], 256, true)
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn owned_org(namespace: &str, binding: &str, write: bool) -> io::Result<RegKey> {
    let org = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(
        format!("Software\\{namespace}"),
        KEY_READ | KEY_WOW64_64KEY | if write { KEY_WRITE } else { 0 },
    )?;
    if org.get_value::<String, _>(BINDING).ok().as_deref() != Some(binding) {
        return Err(error("后端配置归属不匹配；已有数据未修改。"));
    }
    Ok(org)
}
fn apps(org: &RegKey, ns: &str) -> io::Result<Vec<String>> {
    let mut result = vec![ns.to_owned()];
    if let Ok(s) = org.open_subkey(format!("{ns}\\settings")) {
        if let Ok(profile) = s.get_value::<String, _>("current_profile") {
            if !profile.is_empty() {
                if profile.len() > 240 || profile.contains(['\\', '/']) {
                    return Err(error("后端配置名称无效。"));
                }
                result.push(format!("{ns}-{profile}"));
            }
        }
    }
    Ok(result)
}
// Qt serializes QByteArray as @ByteArray(...), including embedded NULs, in
// a UTF-16 registry value. REG_BINARY is UTF-16 too when QString contains NUL.
fn qt_bytes(key: &RegKey, name: &str) -> io::Result<Vec<u8>> {
    let value = key.get_raw_value(name)?;
    if !matches!(value.vtype, REG_SZ | REG_BINARY) || value.bytes.len() % 2 != 0 {
        return Err(error("配对字段格式无效。"));
    }
    let mut chars: Vec<u16> = value
        .bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    if chars.last() == Some(&0) {
        chars.pop();
    }
    let prefix: Vec<u16> = "@ByteArray(".encode_utf16().collect();
    if !chars.starts_with(&prefix) || chars.last() != Some(&(b')' as u16)) {
        return Err(error("配对字段格式无效。"));
    }
    chars[prefix.len()..chars.len() - 1]
        .iter()
        .map(|c| u8::try_from(*c).map_err(|_| error("配对字段格式无效。")))
        .collect()
}
fn set_qt_bytes(key: &RegKey, name: &str, bytes: &[u8]) -> io::Result<()> {
    let binary = bytes.contains(&0);
    let chars: Vec<u16> = "@ByteArray("
        .encode_utf16()
        .chain(bytes.iter().map(|b| *b as u16))
        .chain([b')' as u16])
        .chain((!binary).then_some(0))
        .collect();
    let value = RegValue {
        vtype: if bytes.contains(&0) {
            REG_BINARY
        } else {
            REG_SZ
        },
        bytes: chars.iter().flat_map(|c| c.to_le_bytes()).collect(),
    };
    key.set_raw_value(name, &value)?;
    if qt_bytes(key, name)? != bytes {
        return Err(error("配对数据写入校验失败。"));
    }
    Ok(())
}
fn decode16(record: &Value, name: &str) -> io::Result<Vec<u8>> {
    let bytes = base64::decode(record[name].as_str().unwrap_or_default().as_bytes())
        .map_err(|_| error("配对数据无效。"))?;
    if bytes.len() != 16 {
        return Err(error("配对数据长度无效。"));
    }
    Ok(bytes)
}
fn authorization_code(callback: &str, state: &str) -> io::Result<String> {
    let callback =
        url::Url::parse(callback).map_err(|_| error("请粘贴完整的 Sony 授权回跳网址。"))?;
    if callback.scheme() != "https"
        || callback.host_str() != Some("remoteplay.dl.playstation.net")
        || callback.path() != "/remoteplay/redirect"
        || callback.port().is_some()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || callback.fragment().is_some()
    {
        return Err(error("不是 Sony Remote Play 授权回跳网址。"));
    }
    let codes = callback
        .query_pairs()
        .filter(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .collect::<Vec<_>>();
    let states = callback
        .query_pairs()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .collect::<Vec<_>>();
    if states.len() != 1 || states[0] != state || state.is_empty() {
        return Err(error("授权会话不匹配，请从本播放器重新打开 Sony 登录。"));
    }
    if codes.len() != 1
        || codes[0].is_empty()
        || codes[0].len() > 2048
        || codes[0].chars().any(char::is_control)
    {
        return Err(error("授权网址中没有有效 code。"));
    }
    Ok(codes.into_iter().next().unwrap())
}
impl Connections {
    pub fn new(profile: &Path, isolated: bool) -> Self {
        let vault = profile.join("data/public-connections.bin");
        let (namespace, binding) = identity(isolated.then_some(profile));
        let mut startup_error = None;
        let mut store = match fs::read(&vault) {
            Ok(bytes) => protect(&bytes, false)
                .and_then(|b| {
                    serde_json::from_slice(&b)
                        .map_err(|_| error("授权存储格式无效；原文件已保留。"))
                })
                .and_then(|v| validate_store(&v).map(|()| v))
                .unwrap_or_else(|e| {
                    startup_error = Some(e.to_string());
                    json!({"hosts":[],"account":null})
                }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => json!({"hosts":[],"account":null}),
            Err(_) => {
                startup_error = Some("无法读取授权存储；原文件已保留。".into());
                json!({"hosts":[],"account":null})
            }
        };
        // Read only our full-hash-bound namespace. Never scan other applications.
        if let Ok(org) = owned_org(&namespace, &binding, false) {
            if let Ok(apps) = apps(&org, &namespace) {
                for app in apps {
                    if let Ok(root) = org.open_subkey(format!("{app}\\registered_hosts")) {
                        let count = root.get_value::<u32, _>("size").unwrap_or(0).min(256);
                        for i in 1..=count {
                            let Ok(host) = root.open_subkey(i.to_string()) else {
                                continue;
                            };
                            let (Ok(mac), Ok(key), Ok(regist)) = (
                                qt_bytes(&host, "server_mac"),
                                qt_bytes(&host, "rp_key"),
                                qt_bytes(&host, "rp_regist_key"),
                            ) else {
                                continue;
                            };
                            let name = host
                                .get_value::<String, _>("server_nickname")
                                .unwrap_or_default();
                            if mac.len() != 6
                                || key.len() != 16
                                || regist.len() != 16
                                || name.is_empty()
                            {
                                continue;
                            }
                            let id = hex(&mac);
                            if store["hosts"]
                                .as_array()
                                .is_some_and(|hs| hs.iter().any(|h| h["hostId"] == id))
                            {
                                continue;
                            }
                            let target = host
                                .get_value::<u32, _>("target")
                                .unwrap_or(Target::Ps5_1 as u32);
                            let record = json!({"hostId":id,"hostName":name,"ipAddress":"","hostType":if target>=Target::Ps5Unknown as u32 {"PS5"}else{"PS4"},"target":target,"rp_key":base64::encode(&key),"rp_regist_key":base64::encode(&regist),"server_mac":base64::encode(&mac),"rp_key_type":host.get_value::<u32,_>("rp_key_type").unwrap_or(0)});
                            if let Some(hs) = store["hosts"].as_array_mut() {
                                hs.push(record);
                            }
                        }
                    }
                }
            }
        }
        let (tx, rx) = mpsc::channel();
        Self {
            store: Arc::new(Mutex::new(store)),
            vault,
            namespace,
            binding,
            tx,
            rx,
            operation: String::new(),
            startup_error,
        }
    }
    pub fn metadata(&self) -> Value {
        let store = self.store.lock().unwrap();
        let consoles=store["hosts"].as_array().map(|hs|hs.iter().map(|h|json!({"hostId":h["hostId"],"hostName":h["hostName"],"ipAddress":h["ipAddress"],"hostType":h["hostType"]})).collect::<Vec<_>>()).unwrap_or_default();
        let account = if store["account"]["accountIdBase64"].is_string() {
            json!({"accountIdBase64":store["account"]["accountIdBase64"],"userId":store["account"]["userId"],"onlineId":store["account"]["onlineId"],"remoteAvailable":false,"libraryAvailable":false})
        } else {
            Value::Null
        };
        json!({"consoles":consoles,"account":account})
    }
    pub fn playback_settings(&self) -> Value {
        let mut result = json!({});
        if let Ok(org) = owned_org(&self.namespace, &self.binding, false) {
            if let Ok(apps) = apps(&org, &self.namespace) {
                for app in apps {
                    if let Ok(settings) = org.open_subkey(format!("{app}\\settings")) {
                        for (key, native) in [
                            ("resolution", "resolution_local_ps5"),
                            ("codec", "codec_local_ps5"),
                        ] {
                            if let Ok(v) = settings.get_value::<String, _>(native) {
                                result[key] =
                                    json!(if v == "h265" { "hevc".to_owned() } else { v });
                            }
                        }
                        for (key, native) in [
                            ("fps", "fps_local_ps5"),
                            ("bitrateKbps", "bitrate_local_ps5"),
                        ] {
                            if let Some(v) =
                                settings.get_value::<u32, _>(native).ok().or_else(|| {
                                    settings.get_value::<String, _>(native).ok()?.parse().ok()
                                })
                            {
                                result[key] = json!(v);
                            }
                        }
                        if let Ok(v) = settings.get_value::<String, _>("neural_render") {
                            if matches!(v.as_str(), "true" | "false") {
                                result["neuralRendering"] = json!(v == "true");
                            }
                        }
                    }
                }
            }
        }
        result
    }
    pub fn runtime_nickname(record: &Value) -> String {
        format!("Relay_{}", record["hostId"].as_str().unwrap_or_default())
    }
    pub fn authorization_url(&self) -> io::Result<String> {
        if self.startup_error.is_some() {
            return Err(error("请先解决授权存储读取错误，原文件未修改。"));
        }
        let mut nonce = [0; 32];
        relay_protocol::random::random_bytes(&mut nonce);
        let mut store = self.store.lock().unwrap();
        store["oauthState"] = json!(hex(&nonce));
        let mut url = url::Url::parse(AUTHORIZE).unwrap();
        url.query_pairs_mut().extend_pairs([
            ("service_entity", "urn:service-entity:psn"),
            ("response_type", "code"),
            ("client_id", CLIENT),
            ("redirect_uri", REDIRECT),
            ("scope", SCOPE),
            ("request_locale", "en_US"),
            ("ui", "pr"),
            ("service_logo", "ps"),
            ("layout_type", "popup"),
            ("prompt", "always"),
            ("state", store["oauthState"].as_str().unwrap()),
        ]);
        Ok(url.to_string())
    }
    pub fn request(&mut self, name: &str, args: Value) -> io::Result<()> {
        if !self.operation.is_empty() {
            return Err(error("上一项连接操作尚未完成。"));
        }
        if self.startup_error.is_some() {
            return Err(error("请先解决授权存储读取错误，原文件未修改。"));
        }
        if !matches!(
            name,
            "discover" | "pair" | "sign-in" | "sign-out" | "wake" | "remove"
        ) {
            return Err(error("此连接操作尚未由公开后端实现。"));
        }
        let store = Arc::clone(&self.store);
        let tx = self.tx.clone();
        let path = self.vault.clone();
        let action = name.to_owned();
        let ns = self.namespace.clone();
        let binding = self.binding.clone();
        let name = action.clone();
        thread::Builder::new()
            .name("relay-connect".into())
            .spawn(move || {
                let result =
                    run(&action, args, &store, &path, &ns, &binding).map_err(|e| e.to_string());
                let _ = tx.send((action, result));
            })?;
        self.operation = name;
        Ok(())
    }
    pub fn poll(&mut self) -> Option<(String, Result<Value, String>)> {
        let event = self.rx.try_recv().ok()?;
        self.operation.clear();
        Some(event)
    }
    pub fn target(&self, args: &Value) -> io::Result<Value> {
        if self.startup_error.is_some() {
            return Err(error("请先解决授权存储读取错误，原文件未修改。"));
        }
        let store = self.store.lock().unwrap();
        let mut record = store["hosts"]
            .as_array()
            .and_then(|hs| hs.iter().find(|h| h["hostId"] == args["hostId"]))
            .cloned()
            .ok_or_else(|| error("请选择已配对主机。"))?;
        if let Some(ip) = args["ipAddress"].as_str().filter(|s| !s.is_empty()) {
            record["ipAddress"] = json!(ip);
        }
        if !valid_text(&record["ipAddress"], 253, false) {
            return Err(error("请先搜索局域网主机，或填写此主机的有效连接地址。"));
        }
        Ok(record)
    }
    /// Only returns a selection; pairing keys stay inside the connection store.
    pub fn recent_target(&self) -> Option<Value> {
        if self.startup_error.is_some() {
            return None;
        }
        let store = self.store.lock().unwrap();
        let hosts = store["hosts"].as_array()?;
        let host = if let Some(id) = store["lastConnectedHostId"].as_str() {
            hosts.iter().find(|h| h["hostId"] == id)?
        } else {
            // Older releases did not record connection history. A sole paired
            // console is unambiguous; multiple consoles require a first selection.
            if hosts.len() != 1 {
                return None;
            }
            &hosts[0]
        };
        valid_text(&host["ipAddress"], 253, false).then(|| json!({"hostId":host["hostId"]}))
    }
    pub fn remember_connected(&self, target: &Value) -> io::Result<()> {
        if self.startup_error.is_some() {
            return Err(error("授权存储读取失败，无法保存最近连接主机。"));
        }
        let mut store = self.store.lock().unwrap();
        let mut next = store.clone();
        let host = next["hosts"]
            .as_array_mut()
            .and_then(|hs| hs.iter_mut().find(|h| h["hostId"] == target["hostId"]))
            .ok_or_else(|| error("最近连接的主机已被移除。"))?;
        if !valid_text(&target["ipAddress"], 253, false) {
            return Err(error("最近连接主机地址无效。"));
        }
        host["ipAddress"] = target["ipAddress"].clone();
        next["lastConnectedHostId"] = target["hostId"].clone();
        save(&self.vault, &next)?;
        *store = next;
        Ok(())
    }
    pub fn write_runtime_host(&self, record: &Value) -> io::Result<()> {
        if self.startup_error.is_some() {
            return Err(error("授权存储读取失败，原文件未修改。"));
        }
        let org = owned_org(&self.namespace, &self.binding, true)?;
        let mac = base64::decode(record["server_mac"].as_str().unwrap_or_default().as_bytes())
            .map_err(|_| error("主机标识无效。"))?;
        if mac.len() != 6 {
            return Err(error("主机标识无效。"));
        }
        let key = decode16(record, "rp_key")?;
        let regist = decode16(record, "rp_regist_key")?;
        // Preserve imported display names and complete credentials before native aliases
        // are introduced. The stream CLI resolves nicknames, which need not be unique on PS5s.
        save(&self.vault, &self.store.lock().unwrap())?;
        for app in apps(&org, &self.namespace)? {
            let (root, _) = org.create_subkey(format!("{app}\\registered_hosts"))?;
            let count = root.get_value::<u32, _>("size").unwrap_or(0);
            if count > 256 {
                return Err(error("配对记录数量异常。"));
            }
            let index = (1..=count)
                .find(|i| {
                    root.open_subkey(i.to_string())
                        .ok()
                        .and_then(|k| qt_bytes(&k, "server_mac").ok())
                        .as_deref()
                        == Some(&mac)
                })
                .unwrap_or(count + 1);
            let (host, _) = root.create_subkey(index.to_string())?;
            host.set_value("server_nickname", &Self::runtime_nickname(record))?;
            host.set_value(
                "target",
                &(record["target"].as_u64().unwrap_or(Target::Ps5_1 as u64) as u32),
            )?;
            host.set_value(
                "rp_key_type",
                &(record["rp_key_type"].as_u64().unwrap_or(0) as u32),
            )?;
            set_qt_bytes(&host, "server_mac", &mac)?;
            set_qt_bytes(&host, "rp_key", &key)?;
            set_qt_bytes(&host, "rp_regist_key", &regist)?;
            if index > count {
                root.set_value("size", &index)?;
            }
        }
        Ok(())
    }
}

fn run(
    name: &str,
    args: Value,
    store: &Mutex<Value>,
    path: &Path,
    namespace: &str,
    binding: &str,
) -> io::Result<Value> {
    match name {
        "discover" => {
            let discovery =
                Arc::new(d::Discovery::new(false).map_err(|_| error("无法创建主机发现服务。"))?);
            let found = Arc::new(Mutex::new(Vec::<d::DiscoveryHost>::new()));
            let sink = Arc::clone(&found);
            let receiver = d::DiscoveryThread::start(
                Arc::clone(&discovery),
                Arc::new(move |host| {
                    let mut hs = sink.lock().unwrap();
                    if !hs.iter().any(|h| h.host_addr == host.host_addr) {
                        hs.push(host);
                    }
                }),
            )
            .map_err(|_| error("无法启动主机发现。"))?;
            let mut sent = false;
            for (port, version) in [
                (d::DISCOVERY_PORT_PS5, d::DISCOVERY_PROTOCOL_VERSION_PS5),
                (d::DISCOVERY_PORT_PS4, d::DISCOVERY_PROTOCOL_VERSION_PS4),
            ] {
                let packet = d::DiscoveryPacket {
                    cmd: d::DiscoveryCmd::Srch,
                    protocol_version: Some(version.into()),
                    user_credential: 0,
                };
                sent |= discovery
                    .send(&packet, ([255, 255, 255, 255], port).into())
                    .is_ok();
            }
            if sent {
                thread::sleep(Duration::from_millis(2200));
            }
            let _ = receiver.stop();
            if !sent {
                return Err(error("无法发送局域网发现请求。"));
            }
            let hosts = found.lock().unwrap();
            let mut data = store.lock().unwrap();
            let mut next = data.clone();
            let public=hosts.iter().map(|h|{
                let id=h.host_id.clone().unwrap_or_default().to_lowercase();
                if let Some(hs)=next["hosts"].as_array_mut(){for saved in hs{if saved["hostId"].as_str()==Some(&id){saved["ipAddress"]=json!(h.host_addr);}}}
                json!({"hostId":id,"hostName":h.host_name,"ipAddress":h.host_addr,"hostType":if h.is_ps5(){"PS5"}else{"PS4"},"state":d::discovery_host_state_string(h.state)})
            }).collect::<Vec<_>>();
            save(path, &next)?;
            *data = next;
            Ok(json!({"consoles":public}))
        }
        "pair" => {
            let ip = args["ipAddress"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 253 && !s.chars().any(char::is_control))
                .ok_or_else(|| error("请输入有效的主机地址。"))?;
            let pin = args["pin"]
                .as_str()
                .filter(|s| s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| error("请输入 8 位配对码。"))?
                .parse::<u32>()
                .map_err(|_| error("配对码无效。"))?;
            let data = store.lock().unwrap();
            let identity = args["accountId"]
                .as_str()
                .or(data["account"]["accountIdBase64"].as_str())
                .ok_or_else(|| error("请先完成 Sony 浏览器授权。"))?;
            let account = if identity.bytes().all(|b| b.is_ascii_digit()) {
                identity
                    .parse::<u64>()
                    .map(|v| v.to_le_bytes().to_vec())
                    .map_err(|_| error("账号 ID 无效。"))?
            } else {
                base64::decode(identity.as_bytes()).map_err(|_| error("账号 ID 无效。"))?
            };
            let account: [u8; 8] = account
                .try_into()
                .map_err(|_| error("账号 ID 长度无效。"))?;
            drop(data);
            let (tx, rx) = mpsc::channel();
            let registration = r::Regist::start(
                r::RegistInfo {
                    target: if args["hostType"] == "PS4" {
                        Target::Ps4_10
                    } else {
                        Target::Ps5_1
                    },
                    host: ip.into(),
                    broadcast: false,
                    psn_online_id: None,
                    psn_account_id: account,
                    pin,
                    console_pin: 0,
                },
                Arc::new(move |event| {
                    let _ = tx.send(event);
                }),
            )
            .map_err(|_| error("无法启动配对。"))?;
            let result = rx.recv_timeout(Duration::from_secs(15));
            if result.is_err() {
                registration.stop();
            }
            registration.fini();
            let Ok(r::RegistEvent::FinishedSuccess(host)) = result else {
                return Err(error(
                    "配对失败，请确认 PS5 正显示当前配对码、使用同一账号并检查网络。",
                ));
            };
            let record = json!({"hostId":hex(&host.server_mac),"hostName":host.server_nickname,"ipAddress":ip,"hostType":if args["hostType"]=="PS4"{"PS4"}else{"PS5"},"target":host.target as u32,"rp_key":base64::encode(&host.rp_key),"rp_regist_key":base64::encode(&host.rp_regist_key),"server_mac":base64::encode(&host.server_mac),"rp_key_type":host.rp_key_type});
            let mut data = store.lock().unwrap();
            let mut next = data.clone();
            let hs = next["hosts"]
                .as_array_mut()
                .ok_or_else(|| error("配对存储格式无效。"))?;
            hs.retain(|h| h["hostId"] != record["hostId"]);
            hs.push(record);
            save(path, &next)?;
            *data = next;
            Ok(json!({"paired":true,"hostId":hex(&host.server_mac)}))
        }
        "sign-in" => {
            let data = store.lock().unwrap();
            let state = data["oauthState"]
                .as_str()
                .ok_or_else(|| error("请先点击“在浏览器登录 Sony”。"))?;
            let code = authorization_code(args["credential"].as_str().unwrap_or_default(), state)?;
            drop(data);
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(15))
                .redirects(0)
                .build();
            let auth = format!(
                "Basic {}",
                base64::encode(format!("{CLIENT}:{APP_SECRET}").as_bytes())
            );
            let response = agent
                .post(TOKEN)
                .set("Authorization", &auth)
                .send_form(&[
                    ("grant_type", "authorization_code"),
                    ("code", code.as_ref()),
                    ("scope", SCOPE),
                    ("redirect_uri", REDIRECT),
                ])
                .map_err(|_| error("Sony 授权交换失败，请重新登录后重试。"))?;
            let token: Value = response
                .into_json()
                .map_err(|_| error("Sony 授权响应无效。"))?;
            let access = token["access_token"]
                .as_str()
                .filter(|s| s.len() < 4096 && !s.chars().any(char::is_control))
                .ok_or_else(|| error("Sony 没有返回有效令牌。"))?;
            let mut token_url = url::Url::parse(TOKEN).unwrap();
            token_url.path_segments_mut().unwrap().push(access);
            let account: Value = agent
                .get(token_url.as_str())
                .set("Authorization", &auth)
                .call()
                .map_err(|_| error("无法读取 Sony 账号 ID。"))?
                .into_json()
                .map_err(|_| error("Sony 账号响应无效。"))?;
            let user = account["user_id"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| account["user_id"].as_u64())
                .ok_or_else(|| error("Sony 没有返回有效账号 ID。"))?;
            let mut data = store.lock().unwrap();
            let mut next = data.clone();
            next["account"] = json!({"userId":user.to_string(),"accountIdBase64":base64::encode(&user.to_le_bytes()),"onlineId":account["online_id"].as_str().unwrap_or_default()});
            next.as_object_mut().unwrap().remove("oauthState");
            save(path, &next)?;
            *data = next;
            Ok(json!({"authorized":true}))
        }
        "sign-out" => {
            let mut data = store.lock().unwrap();
            let mut next = data.clone();
            next["account"] = Value::Null;
            next.as_object_mut().unwrap().remove("oauthState");
            save(path, &next)?;
            *data = next;
            Ok(json!({}))
        }
        "wake" => {
            let data = store.lock().unwrap();
            let h = data["hosts"]
                .as_array()
                .and_then(|hs| hs.iter().find(|h| h["hostId"] == args["hostId"]))
                .ok_or_else(|| error("请选择已配对主机。"))?;
            let key = decode16(h, "rp_regist_key")?;
            let text = String::from_utf8_lossy(&key);
            let credential = u64::from_str_radix(text.trim_matches('\0'), 16)
                .map_err(|_| error("唤醒凭证无效。"))?;
            d::wakeup(
                None,
                h["ipAddress"].as_str().unwrap_or_default(),
                credential,
                h["hostType"] != "PS4",
            )
            .map_err(|_| error("无法发送唤醒请求。"))?;
            Ok(json!({}))
        }
        "remove" => {
            let id = args["hostId"]
                .as_str()
                .filter(|s| s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| error("请选择有效主机。"))?;
            if let Ok(org) = owned_org(namespace, binding, true) {
                for app in apps(&org, namespace)? {
                    if let Ok(root) = org.open_subkey_with_flags(
                        format!("{app}\\registered_hosts"),
                        KEY_READ | KEY_WRITE,
                    ) {
                        let count = root.get_value::<u32, _>("size").unwrap_or(0).min(256);
                        for i in 1..=count {
                            if root
                                .open_subkey(i.to_string())
                                .ok()
                                .and_then(|k| qt_bytes(&k, "server_mac").ok())
                                .is_some_and(|m| hex(&m) == id)
                            {
                                root.delete_subkey_all(i.to_string())?;
                            }
                        }
                    }
                }
            }
            let mut data = store.lock().unwrap();
            let mut next = data.clone();
            if let Some(hs) = next["hosts"].as_array_mut() {
                hs.retain(|h| h["hostId"] != id);
            }
            if next["lastConnectedHostId"] == id {
                next.as_object_mut().unwrap().remove("lastConnectedHostId");
            }
            save(path, &next)?;
            *data = next;
            Ok(json!({}))
        }
        _ => Err(error("不支持的连接操作。")),
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub fn write_native_fixture(profile: &Path) -> io::Result<()> {
    let c = Connections::new(profile, true);
    let org = owned_org(&c.namespace, &c.binding, true)?;
    for app in apps(&org, &c.namespace)? {
        if let Ok(root) = org.open_subkey(format!("{app}\\registered_hosts")) {
            if root.get_value::<u32, _>("size").unwrap_or(0) > 0 {
                return Err(error("Fixture requires an empty isolated profile."));
            }
        }
    }
    let record = json!({"hostId":"020000000001","hostName":"Relay-QA Connection","ipAddress":"127.0.0.1","hostType":"PS5","target":Target::Ps5_1 as u32,"rp_key_type":0,"server_mac":base64::encode(&[2,0,0,0,0,1]),"rp_key":base64::encode(&[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,255]),"rp_regist_key":base64::encode(&[0;16])});
    c.store.lock().unwrap()["hosts"] = json!([record.clone()]);
    c.write_runtime_host(&record)?;
    let imported = Connections::new(profile, true);
    if imported.metadata()["consoles"][0]["hostName"] != "Relay-QA Connection" {
        return Err(error("Fixture native readback failed."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_host() -> Value {
        json!({"hostId":"020000000001","hostName":"Relay-QA Connection","ipAddress":"127.0.0.1","hostType":"PS5","target":Target::Ps5_1 as u32,"rp_key_type":0,"server_mac":base64::encode(&[2,0,0,0,0,1]),"rp_key":base64::encode(&[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,255]),"rp_regist_key":base64::encode(&[0;16])})
    }
    struct Fixture {
        root: PathBuf,
        ns: String,
        binding: String,
    }
    impl Fixture {
        fn new() -> Self {
            loop {
                let mut nonce = [0; 16];
                relay_protocol::random::random_bytes(&mut nonce);
                let root =
                    std::env::temp_dir().join(format!("Relay-Connection-QA-{}", hex(&nonce)));
                let (ns, binding) = identity(Some(&root));
                let hk = RegKey::predef(HKEY_CURRENT_USER);
                let path = format!("Software\\{ns}");
                if hk.open_subkey(&path).is_ok() {
                    continue;
                }
                let (org, disposition) = hk
                    .create_subkey_with_flags(&path, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)
                    .unwrap();
                assert_eq!(disposition, REG_CREATED_NEW_KEY);
                org.set_value(BINDING, &binding).unwrap();
                return Self { root, ns, binding };
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let hk = RegKey::predef(HKEY_CURRENT_USER);
            if let Ok(org) = owned_org(&self.ns, &self.binding, true) {
                drop(org);
                hk.delete_subkey_all(format!("Software\\{}", self.ns))
                    .unwrap();
            }
            if self.root.exists() {
                assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
                fs::remove_dir_all(&self.root).unwrap();
            }
        }
    }
    #[test]
    fn vault_is_encrypted_and_identity_is_stable() {
        let data = b"synthetic QA authorization";
        let enc = protect(data, true).unwrap();
        assert!(!enc.windows(data.len()).any(|b| b == data));
        assert_eq!(protect(&enc, false).unwrap(), data);
        assert_eq!(identity(None), identity(None));
        assert_ne!(identity(None), identity(Some(Path::new(r"L:\QA\isolated"))));
    }
    #[test]
    fn metadata_never_exposes_pairing_keys() {
        let root =
            std::env::temp_dir().join(format!("relay-connection-test-{}", std::process::id()));
        let c = Connections::new(&root, true);
        c.store.lock().unwrap()["hosts"] = json!([{"hostId":"fixture","hostName":"QA","rp_key":"never-return","rp_regist_key":"never-return"}]);
        assert!(!c.metadata().to_string().contains("never-return"));
    }
    #[test]
    fn recent_host_survives_restart_and_removed_hosts_are_not_reconnected() {
        let f = Fixture::new();
        let c = Connections::new(&f.root, true);
        assert!(c.recent_target().is_none());
        let first = fixture_host();
        let mut second = first.clone();
        second["hostId"] = json!("020000000002");
        second["server_mac"] = json!(base64::encode(&[2, 0, 0, 0, 0, 2]));
        c.store.lock().unwrap()["hosts"] = json!([first.clone(), second.clone()]);
        assert!(
            c.recent_target().is_none(),
            "older multi-console profiles need an explicit selection"
        );
        let mut connected = second.clone();
        connected["ipAddress"] = json!("192.0.2.41");
        c.remember_connected(&connected).unwrap();
        let recent = c.recent_target().unwrap();
        assert_eq!(recent, json!({"hostId":second["hostId"]}));
        assert!(recent.get("rp_key").is_none());
        let loaded = Connections::new(&f.root, true);
        assert_eq!(loaded.recent_target(), Some(recent.clone()));
        assert_eq!(loaded.target(&recent).unwrap()["ipAddress"], "192.0.2.41");
        // A failed attempt never invokes remember_connected and leaves history intact.
        assert!(loaded.target(&json!({"hostId":"missing"})).is_err());
        assert_eq!(loaded.recent_target(), Some(recent));
        run(
            "remove",
            json!({"hostId":second["hostId"]}),
            &loaded.store,
            &loaded.vault,
            &f.ns,
            &f.binding,
        )
        .unwrap();
        assert_eq!(
            loaded.recent_target(),
            Some(json!({"hostId":first["hostId"]}))
        );
        loaded.store.lock().unwrap()["hosts"] = json!([]);
        assert!(loaded.recent_target().is_none());
    }
    #[test]
    fn corrupt_vault_blocks_mutations_without_overwriting_original() {
        let f = Fixture::new();
        let vault = f.root.join("data/public-connections.bin");
        fs::create_dir_all(vault.parent().unwrap()).unwrap();
        for data in [
            b"corrupt".to_vec(),
            protect(b"[]", true).unwrap(),
            protect(br#"{"hosts":{},"account":null}"#, true).unwrap(),
        ] {
            fs::write(&vault, &data).unwrap();
            let mut c = Connections::new(&f.root, true);
            assert!(c.startup_error.is_some());
            assert!(c.authorization_url().is_err());
            assert!(c.request("discover", json!({})).is_err());
            assert!(c.target(&json!({})).is_err());
            assert_eq!(fs::read(&vault).unwrap(), data);
        }
    }
    #[test]
    fn sony_callback_rejects_wrong_origin_state_and_ambiguous_values() {
        let callback = format!("{REDIRECT}?code=qa%2Bcode&state=qa");
        assert_eq!(authorization_code(&callback, "qa").unwrap(), "qa+code");
        for url in [
            callback.replace("https:", "http:"),
            callback.replace("remoteplay.dl.playstation.net", "example.com"),
            callback.replace("state=qa", "state=other"),
            format!("{callback}&code=duplicate"),
            format!("{callback}&state=qa"),
            format!("{callback}#fragment"),
            format!("{REDIRECT}?code=synthetic"),
        ] {
            assert!(authorization_code(&url, "qa").is_err());
        }
    }
    #[test]
    fn ps5_pairing_uses_own_network_flow_and_saves_encrypted_host() {
        use relay_protocol::rpcrypt::{aeropause, Rpcrypt};
        use std::{
            io::{Read, Write},
            net::{TcpListener, UdpSocket},
        };
        let f = Fixture::new();
        let mut c = Connections::new(&f.root, true);
        let udp = UdpSocket::bind("127.0.0.1:9295").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:9295").unwrap();
        tcp.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut buf = [0; 16];
            let (n, addr) = udp.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"SRC3\0");
            udp.send_to(b"RES3\0", addr).unwrap();
            let end = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                if let Ok((s, _)) = tcp.accept() {
                    break s;
                }
                assert!(std::time::Instant::now() < end);
                thread::sleep(Duration::from_millis(5));
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                header.push(b[0]);
                assert!(header.len() < 4096);
            }
            let header = String::from_utf8(header).unwrap();
            let len = header
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            assert!((0x1e0..0x400).contains(&len));
            let mut payload = vec![0; len];
            stream.read_exact(&mut payload).unwrap();
            let mut aer = [0; 16];
            aer[..8].copy_from_slice(&payload[0x191..0x199]);
            aer[8..].copy_from_slice(&payload[0xc7..0xcf]);
            let zero = aeropause(Target::Ps5_1, (payload[0] >> 3) as usize, &[0; 16]).unwrap();
            let mut ambassador = [0; 16];
            for i in 0..16 {
                let k = zero[i].wrapping_sub(0xd3).wrapping_sub(i as u8);
                ambassador[i] = aer[i].wrapping_sub(0xd3).wrapping_sub(i as u8) ^ k;
            }
            let crypt = Rpcrypt::new_regist(
                Target::Ps5_1,
                &ambassador,
                (payload[0x18d] & 31) as usize,
                12345678,
            )
            .unwrap();
            crypt.decrypt(0, &mut payload[0x1e0..]).unwrap();
            assert!(
                String::from_utf8_lossy(&payload[0x1e0..]).contains("Np-AccountId: AQAAAAAAAAA=")
            );
            let mut response=b"PS5-Mac: 020000000001\r\nPS5-RegistKey: 31323334000000000000000000000000\r\nRP-KeyType: 0\r\nRP-Key: 000102030405060708090a0b0c0d0eff\r\nPS5-Nickname: Relay-QA Connection\r\n".to_vec();
            crypt.encrypt(0, &mut response).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(&response).unwrap();
        });
        c.request(
            "pair",
            json!({"ipAddress":"127.0.0.1","pin":"12345678","hostType":"PS5","accountId":"1"}),
        )
        .unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(10);
        let result = loop {
            if let Some((action, result)) = c.poll() {
                assert_eq!(action, "pair");
                break result.unwrap();
            }
            assert!(std::time::Instant::now() < end);
            thread::sleep(Duration::from_millis(10));
        };
        server.join().unwrap();
        assert_eq!(result["hostId"], "020000000001");
        let reopened = Connections::new(&f.root, true);
        let record = reopened.target(&json!({"hostId":"020000000001"})).unwrap();
        assert_eq!(
            decode16(&record, "rp_key").unwrap(),
            decode16(&fixture_host(), "rp_key").unwrap()
        );
        let bytes = fs::read(&c.vault).unwrap();
        assert!(!bytes.windows(16).any(|s| s == b"Relay-QA Connect"));
    }
    #[test]
    fn own_registry_pairing_roundtrips_qt_bytes_and_preserves_other_profiles() {
        let f = Fixture::new();
        let mut c = Connections::new(&f.root, true);
        let org = owned_org(&f.ns, &f.binding, true).unwrap();
        let (settings, _) = org.create_subkey(format!("{}\\settings", f.ns)).unwrap();
        settings
            .set_value("current_profile", &"Synthetic-QA")
            .unwrap();
        let (foreign, _) = org.create_subkey("unrelated-app").unwrap();
        foreign.set_value("sentinel", &"unchanged").unwrap();
        let host = fixture_host();
        c.store.lock().unwrap()["hosts"] = json!([host.clone()]);
        save(&c.vault, &c.store.lock().unwrap()).unwrap();
        c.write_runtime_host(&host).unwrap();
        for app in apps(&org, &f.ns).unwrap() {
            let key = org
                .open_subkey(format!("{app}\\registered_hosts\\1"))
                .unwrap();
            let expected = decode16(&host, "rp_key").unwrap();
            assert_eq!(qt_bytes(&key, "rp_key").unwrap(), expected);
            let raw = key.get_raw_value("rp_key").unwrap();
            assert_eq!(raw.vtype, REG_BINARY);
            assert_eq!(raw.bytes.len(), (11 + 16 + 1) * 2);
            set_qt_bytes(&key, "synthetic_nonzero", &[255, 128, 41, 65]).unwrap_err(); // read-only key
            let (write, _) = org
                .create_subkey(format!("{app}\\registered_hosts\\1"))
                .unwrap();
            set_qt_bytes(&write, "synthetic_nonzero", &[255, 128, 41, 65]).unwrap();
            assert_eq!(
                write.get_raw_value("synthetic_nonzero").unwrap().vtype,
                REG_SZ
            );
            assert_eq!(
                qt_bytes(&write, "synthetic_nonzero").unwrap(),
                [255, 128, 41, 65]
            );
        }
        assert_eq!(
            foreign.get_value::<String, _>("sentinel").unwrap(),
            "unchanged"
        );
        let imported = Connections::new(&f.root, true);
        assert_eq!(
            imported.metadata()["consoles"][0]["hostName"],
            host["hostName"]
        );
        assert_eq!(
            imported.target(&json!({"hostId":"020000000001"})).unwrap(),
            host
        );
        assert!(imported
            .target(&json!({"hostId":"020000000001","ipAddress":"bad\naddress"}))
            .is_err());
        // An incorrect full binding must reject a write even with the same short namespace.
        c.binding = "0".repeat(64);
        assert!(c.write_runtime_host(&host).is_err());
    }
}
