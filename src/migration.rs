//! Inherit portable settings and a DPAPI blob without modifying the old release.
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use windows::Win32::{
    Foundation::{BOOL, HANDLE},
    Security::{
        EqualSid, GetKernelObjectSecurity, GetSecurityDescriptorOwner, GetTokenInformation,
        TokenUser, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

const VAULT: &str = "credentials.dpapi";
const SETTINGS: [&str; 2] = ["player.json", "settings.json"];
const REPARSE: u32 = 0x400;
const MAX_VAULT: u64 = 16 * 1024 * 1024;
const MAX_SETTINGS: u64 = 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

fn real_dir(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(m.is_dir() && m.file_attributes() & REPARSE == 0),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn known_wrapper(name: Option<&OsStr>) -> bool {
    let Some(s) = name.and_then(OsStr::to_str) else {
        return false;
    };
    let lower = s.to_ascii_lowercase();
    let Some(v) = lower.strip_prefix("relayplayer-") else {
        return false;
    };
    let v = v.strip_suffix("-win-x64").unwrap_or(v);
    let mut parts = v.split('.');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|x| x.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn MoveFileW(existing: *const u16, new: *const u16) -> i32;
}

// A same-directory Windows rename which fails if another player created the
// destination. Unlike std::fs::rename it never replaces the user's new vault;
// unlike hard_link it also works on FAT/exFAT portable drives.
fn publish_if_absent(tmp: &Path, dest: &Path) -> io::Result<bool> {
    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain([0]).collect()
    }
    let (a, b) = (wide(tmp), wide(dest));
    if unsafe { MoveFileW(a.as_ptr(), b.as_ptr()) } != 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::AlreadyExists {
        return Ok(false);
    }
    Err(e)
}

struct Staged(PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

// Check ownership on the source handle, without decrypting the DPAPI blob or
// opening other accounts' profile directories. An unidentifiable owner (e.g.
// on a FAT drive) is not a candidate for *automatic* inheritance.
fn belongs_to_current_user(file: &File) -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = OwnedHandle::from_raw_handle(token.0);
        let token_handle = HANDLE(token.as_raw_handle());
        let mut size = 0;
        let _ = GetTokenInformation(token_handle, TokenUser, None, 0, &mut size);
        if size == 0 {
            return false;
        }
        // Both structures contain pointers; keep their backing buffers aligned.
        let mut user = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        if GetTokenInformation(
            token_handle,
            TokenUser,
            Some(user.as_mut_ptr().cast()),
            size,
            &mut size,
        )
        .is_err()
        {
            return false;
        }
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        let f = HANDLE(file.as_raw_handle());
        let mut need = 0;
        let _ = GetKernelObjectSecurity(
            f,
            OWNER_SECURITY_INFORMATION.0,
            PSECURITY_DESCRIPTOR::default(),
            0,
            &mut need,
        );
        if need == 0 {
            return false;
        }
        let mut descriptor = vec![0usize; (need as usize).div_ceil(size_of::<usize>())];
        let sd = PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast());
        if GetKernelObjectSecurity(f, OWNER_SECURITY_INFORMATION.0, sd, need, &mut need).is_err() {
            return false;
        }
        let mut owner = PSID::default();
        GetSecurityDescriptorOwner(sd, &mut owner, &mut BOOL::default()).is_ok()
            && !owner.0.is_null()
            && EqualSid(user.User.Sid, owner).is_ok()
    }
}

fn publish_if_absent_bytes(dst_data: &Path, name: &str, bytes: &[u8]) -> io::Result<bool> {
    fs::create_dir_all(dst_data)?;
    if !real_dir(dst_data)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "新版数据目录不是普通目录",
        ));
    }
    let (staged, mut out) = loop {
        let n = SERIAL.fetch_add(1, Ordering::Relaxed);
        let path = dst_data.join(format!(".relay-migrate-{}-{n}.tmp", std::process::id()));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => break (Staged(path), file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    out.write_all(bytes)?;
    out.sync_all()?;
    drop(out);
    publish_if_absent(&staged.0, &dst_data.join(name))
}

fn copy_vault(src: impl Read, len: u64, dst_data: &Path) -> io::Result<bool> {
    if !(1..=MAX_VAULT).contains(&len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "旧凭据文件大小异常",
        ));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    if src.take(len + 1).read_to_end(&mut bytes)? as u64 != len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "旧凭据在迁移中变化，请重试",
        ));
    }
    publish_if_absent_bytes(dst_data, VAULT, &bytes)
}

fn copy_setting_file(source: &Path, dst_data: &Path, name: &str) -> io::Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(0x00200000)
        .open(source)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.file_attributes() & REPARSE != 0
        || !(1..=MAX_SETTINGS).contains(&metadata.len())
    {
        return Ok(false);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file.take(MAX_SETTINGS + 1).read_to_end(&mut bytes)? as u64 != metadata.len() {
        return Ok(false);
    }
    if !serde_json::from_slice::<serde_json::Value>(&bytes).is_ok_and(|value| value.is_object()) {
        return Ok(false);
    }
    publish_if_absent_bytes(dst_data, name, &bytes)
}

fn old_data_dirs(base: &Path, version: &str) -> [PathBuf; 4] {
    let plain = base.join(format!("RelayPlayer-{version}"));
    let suffixed = base.join(format!("RelayPlayer-{version}-win-x64"));
    [
        plain.join("RelayPlayer").join("data"),
        plain.join("data"),
        suffixed.join("RelayPlayer").join("data"),
        suffixed.join("data"),
    ]
}

fn inherit_settings(root: &Path, base: &Path) -> io::Result<bool> {
    let data = root.join("data");
    let mut changed = false;
    let mut first_error = None;
    for name in SETTINGS {
        if exists(&data.join(name))? {
            continue;
        }
        for old_data in old_data_dirs(base, "0.1.5") {
            if !real_dir(&old_data)? {
                continue;
            }
            match copy_setting_file(&old_data.join(name), &data, name) {
                Ok(true) => {
                    changed = true;
                    break;
                }
                Ok(false) => {}
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
    }
    if let Some(e) = first_error {
        return Err(e);
    }
    Ok(changed)
}

/// Recognise only known, adjacent release layouts; never recurse through the
/// user's machine looking for credentials. Explicit/dev data roots do not migrate.
/// Existing destination credentials always belong to the user, even if unreadable.
pub(super) fn inherit(root: &Path) -> io::Result<bool> {
    let data = root.join("data");
    if !real_dir(root)? {
        return Ok(false);
    }
    let Some(mut base) = root.parent() else {
        return Ok(false);
    };
    if root
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("RelayPlayer"))
        && known_wrapper(base.file_name())
    {
        let Some(parent) = base.parent() else {
            return Ok(false);
        };
        if !real_dir(base)? {
            return Ok(false);
        }
        base = parent;
    }
    // Settings come only from 0.1.5. Each destination file is preserved independently.
    let (settings_changed, settings_error) = match inherit_settings(root, &base) {
        Ok(changed) => (changed, None),
        Err(e) => (false, Some(e)),
    };
    if exists(&data.join(VAULT))? {
        return match settings_error {
            Some(e) => Err(e),
            None => Ok(settings_changed),
        };
    }
    // Prefer the user's current 0.1.5 registration, then retain older stores as fallbacks.
    for v in ["0.1.5", "0.1.4", "0.1.3"] {
        for suffix in ["", "-win-x64"] {
            let wrapper = base.join(format!("RelayPlayer-{v}{suffix}"));
            if !real_dir(&wrapper)? {
                continue;
            }
            for candidate in [wrapper.join("RelayPlayer"), wrapper.clone()] {
                if !real_dir(&candidate)? || !real_dir(&candidate.join("data"))? {
                    continue;
                }
                let source = candidate.join("data").join(VAULT);
                // Open a reparse point itself rather than following it. The normal
                // player writes snapshots by atomically replacing this file.
                let file = match OpenOptions::new()
                    .read(true)
                    .custom_flags(0x00200000)
                    .open(&source)
                {
                    Ok(f) => f,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                let m = file.metadata()?;
                if m.is_file()
                    && m.file_attributes() & REPARSE == 0
                    && m.len() > 0
                    && belongs_to_current_user(&file)
                {
                    let copied = copy_vault(file, m.len(), &data)?;
                    return match settings_error {
                        Some(e) => Err(e),
                        None => Ok(copied || settings_changed),
                    };
                }
            }
        }
    }
    match settings_error {
        Some(e) => Err(e),
        None => Ok(settings_changed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let n = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!(
                "Relay-migration-fixture-{}-{n}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn root(&self) -> PathBuf {
            self.0.join("RelayPlayer-0.1.6").join("RelayPlayer")
        }
        fn old_data(&self, version: &str) -> PathBuf {
            self.0
                .join(format!("RelayPlayer-{version}"))
                .join("RelayPlayer")
                .join("data")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prefers_015_keeps_older_stores_and_never_overwrites() {
        let f = Fixture::new();
        let root = f.root();
        let old = f.old_data("0.1.3");
        let previous = f.old_data("0.1.4");
        let latest = f.old_data("0.1.5");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&previous).unwrap();
        fs::create_dir_all(&latest).unwrap();
        fs::write(old.join(VAULT), b"older").unwrap();
        fs::write(previous.join(VAULT), b"previous").unwrap();
        let opaque = [0, 1, 2, 0xfe, 0xff, 21, 0, 99];
        fs::write(latest.join(VAULT), opaque).unwrap();

        assert!(inherit(&root).unwrap());
        assert_eq!(fs::read(root.join("data").join(VAULT)).unwrap(), opaque);
        assert_eq!(fs::read(old.join(VAULT)).unwrap(), b"older");
        assert_eq!(fs::read(previous.join(VAULT)).unwrap(), b"previous");
        assert_eq!(fs::read(latest.join(VAULT)).unwrap(), opaque);

        fs::write(root.join("data").join(VAULT), b"newer-user-data").unwrap();
        assert!(!inherit(&root).unwrap());
        assert_eq!(
            fs::read(root.join("data").join(VAULT)).unwrap(),
            b"newer-user-data"
        );
        assert!(fs::read_dir(root.join("data")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }

    #[test]
    fn falls_back_to_014_then_013_and_does_not_search_unrelated_development_roots() {
        let f = Fixture::new();
        let root = f.root();
        fs::create_dir_all(&root).unwrap();
        let previous = f.old_data("0.1.4");
        let old = f.old_data("0.1.3");
        fs::create_dir_all(&previous).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::write(previous.join(VAULT), b"previous-blob").unwrap();
        fs::write(old.join(VAULT), b"test-blob").unwrap();
        assert!(inherit(&root).unwrap());
        assert_eq!(
            fs::read(root.join("data").join(VAULT)).unwrap(),
            b"previous-blob"
        );

        let oldest = Fixture::new();
        let oldest_root = oldest.root();
        fs::create_dir_all(&oldest_root).unwrap();
        let old = oldest.old_data("0.1.3");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join(VAULT), b"test-blob").unwrap();
        assert!(inherit(&oldest_root).unwrap());
        assert_eq!(
            fs::read(oldest_root.join("data").join(VAULT)).unwrap(),
            b"test-blob"
        );

        let isolated = Fixture::new();
        let isolated_root = isolated.root();
        fs::create_dir_all(&isolated_root).unwrap();
        assert!(!inherit(&isolated_root).unwrap());
        assert!(!isolated_root.join("data").exists());
    }
    #[test]
    fn failed_partial_copy_and_competing_publication_keep_only_the_winner() {
        let f = Fixture::new();
        let data = f.0.join("data");
        assert!(copy_vault(&b"short"[..], 10, &data).is_err());
        assert!(!data.exists());
        // A second instance may publish during our copy; MoveFileW must not replace it.
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join(VAULT), b"winner").unwrap();
        assert!(!copy_vault(&b"later"[..], 5, &data).unwrap());
        assert_eq!(fs::read(data.join(VAULT)).unwrap(), b"winner");
        assert_eq!(fs::read_dir(&data).unwrap().count(), 1);
    }
}
