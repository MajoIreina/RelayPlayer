use crate::{migration, Prefs};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

const PLAYER_JSON: &[u8] = br#"{
  "volume": 0.37,
  "muted": true,
  "autoHide": false,
  "nativeRemoteWindow": true
}
"#;
const SETTINGS_JSON: &[u8] = br#"{
  "resolution": "720p",
  "fps": 30,
  "bitrateKbps": 87234,
  "upscaler": "rtx-vsr",
  "neuralRendering": true
}
"#;
const VAULT_BYTES: &[u8] = &[0, 1, 2, 0xfe, 0xff, 21, 0, 99];

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "Relay-settings-migration-{}-{nonce}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn nested_current_root(&self) -> PathBuf {
        self.0.join("RelayPlayer-0.1.7-win-x64").join("RelayPlayer")
    }

    fn flat_current_root(&self) -> PathBuf {
        self.0.join("RelayPlayer-0.1.7-win-x64")
    }

    fn release_data(&self, version: &str, win_x64: bool, nested: bool) -> PathBuf {
        let suffix = if win_x64 { "-win-x64" } else { "" };
        let release = self.0.join(format!("RelayPlayer-{version}{suffix}"));
        if nested {
            release.join("RelayPlayer").join("data")
        } else {
            release.join("data")
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_source(data: &Path, player: Option<&[u8]>, settings: Option<&[u8]>, vault: bool) {
    fs::create_dir_all(data).unwrap();
    if let Some(bytes) = player {
        fs::write(data.join("player.json"), bytes).unwrap();
    }
    if let Some(bytes) = settings {
        fs::write(data.join("settings.json"), bytes).unwrap();
    }
    if vault {
        fs::write(data.join("credentials.dpapi"), VAULT_BYTES).unwrap();
    }
}

fn assert_loaded_player_preferences(prefs: &Prefs) {
    assert!((prefs.volume - 0.37).abs() < f32::EPSILON);
    assert!(prefs.muted);
    assert!(!prefs.auto_hide);
    assert!(prefs.native_remote_window);
}

fn assert_default_player_preferences(prefs: &Prefs) {
    assert_eq!(prefs.volume, 1.0);
    assert!(!prefs.muted);
    assert!(prefs.auto_hide);
    assert!(!prefs.native_remote_window);
}

#[test]
fn first_load_migrates_nested_win_x64_settings_byte_for_byte() {
    let f = Fixture::new();
    let root = f.nested_current_root();
    let old = f.release_data("0.1.5", true, true);
    fs::create_dir_all(&root).unwrap();
    write_source(&old, Some(PLAYER_JSON), Some(SETTINGS_JSON), true);

    migration::inherit(&root).unwrap();
    let prefs = Prefs::load(&root);

    assert_loaded_player_preferences(&prefs);
    let data = root.join("data");
    assert_eq!(fs::read(data.join("player.json")).unwrap(), PLAYER_JSON);
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), SETTINGS_JSON);
    assert_eq!(
        fs::read(data.join("credentials.dpapi")).unwrap(),
        VAULT_BYTES
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings["resolution"], "720p");
    assert_eq!(settings["fps"], 30);
    assert_eq!(settings["bitrateKbps"], 87234);
    assert_eq!(settings["upscaler"], "rtx-vsr");
    assert_eq!(settings["neuralRendering"], true);
}

#[test]
fn first_load_migrates_flat_win_x64_settings_byte_for_byte() {
    let f = Fixture::new();
    let root = f.flat_current_root();
    let old = f.release_data("0.1.5", true, false);
    fs::create_dir_all(&root).unwrap();
    write_source(&old, Some(PLAYER_JSON), Some(SETTINGS_JSON), true);

    migration::inherit(&root).unwrap();
    let prefs = Prefs::load(&root);

    assert_loaded_player_preferences(&prefs);
    let data = root.join("data");
    assert_eq!(fs::read(data.join("player.json")).unwrap(), PLAYER_JSON);
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), SETTINGS_JSON);
    assert_eq!(
        fs::read(data.join("credentials.dpapi")).unwrap(),
        VAULT_BYTES
    );
}

#[test]
fn partial_destinations_fill_only_missing_settings_with_or_without_vault() {
    let with_vault = Fixture::new();
    let root = with_vault.nested_current_root();
    let old = with_vault.release_data("0.1.5", true, true);
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("credentials.dpapi"), b"existing-vault").unwrap();
    write_source(&old, Some(PLAYER_JSON), Some(SETTINGS_JSON), true);

    migration::inherit(&root).unwrap();
    assert_loaded_player_preferences(&Prefs::load(&root));
    assert_eq!(
        fs::read(data.join("credentials.dpapi")).unwrap(),
        b"existing-vault"
    );
    assert_eq!(fs::read(data.join("player.json")).unwrap(), PLAYER_JSON);
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), SETTINGS_JSON);

    let with_settings = Fixture::new();
    let root = with_settings.nested_current_root();
    let old = with_settings.release_data("0.1.5", true, true);
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("settings.json"), b"{\"bitrateKbps\":44000}").unwrap();
    write_source(&old, Some(PLAYER_JSON), Some(SETTINGS_JSON), true);

    migration::inherit(&root).unwrap();
    assert_loaded_player_preferences(&Prefs::load(&root));
    assert_eq!(fs::read(data.join("player.json")).unwrap(), PLAYER_JSON);
    assert_eq!(
        fs::read(data.join("settings.json")).unwrap(),
        b"{\"bitrateKbps\":44000}"
    );
    assert_eq!(
        fs::read(data.join("credentials.dpapi")).unwrap(),
        VAULT_BYTES
    );
}

#[test]
fn migration_is_idempotent_and_does_not_fall_back_to_014_settings() {
    let f = Fixture::new();
    let root = f.nested_current_root();
    let old_015 = f.release_data("0.1.5", true, true);
    let old_014 = f.release_data("0.1.4", true, true);
    fs::create_dir_all(&root).unwrap();
    write_source(&old_015, None, None, true);
    write_source(&old_014, Some(PLAYER_JSON), Some(SETTINGS_JSON), false);

    migration::inherit(&root).unwrap();
    let prefs = Prefs::load(&root);
    assert_default_player_preferences(&prefs);
    assert!(!root.join("data/player.json").exists());
    assert!(!root.join("data/settings.json").exists());
    assert_eq!(
        fs::read(root.join("data/credentials.dpapi")).unwrap(),
        VAULT_BYTES
    );

    let changed_player = b"{\"volume\":0.81,\"muted\":false}";
    let changed_settings = b"{\"bitrateKbps\":99000}";
    fs::write(root.join("data/player.json"), changed_player).unwrap();
    fs::write(root.join("data/settings.json"), changed_settings).unwrap();
    migration::inherit(&root).unwrap();
    assert_eq!(
        fs::read(root.join("data/player.json")).unwrap(),
        changed_player
    );
    assert_eq!(
        fs::read(root.join("data/settings.json")).unwrap(),
        changed_settings
    );
}

#[test]
fn invalid_json_is_skipped_without_blocking_other_files() {
    for invalid_file in ["player.json", "settings.json"] {
        let f = Fixture::new();
        let root = f.nested_current_root();
        let old = f.release_data("0.1.5", true, true);
        fs::create_dir_all(&root).unwrap();
        let player = if invalid_file == "player.json" {
            b"{invalid".as_slice()
        } else {
            PLAYER_JSON
        };
        let settings = if invalid_file == "settings.json" {
            b"{invalid".as_slice()
        } else {
            SETTINGS_JSON
        };
        write_source(&old, Some(player), Some(settings), true);

        migration::inherit(&root).unwrap();
        let prefs = Prefs::load(&root);
        let data = root.join("data");

        if invalid_file == "player.json" {
            assert_default_player_preferences(&prefs);
            assert!(!data.join("player.json").exists());
            assert_eq!(fs::read(data.join("settings.json")).unwrap(), SETTINGS_JSON);
        } else {
            assert_loaded_player_preferences(&prefs);
            assert_eq!(fs::read(data.join("player.json")).unwrap(), PLAYER_JSON);
            assert!(!data.join("settings.json").exists());
        }
        assert_eq!(
            fs::read(data.join("credentials.dpapi")).unwrap(),
            VAULT_BYTES
        );
    }
}

#[test]
fn development_root_does_not_search_unrelated_release_directory() {
    let f = Fixture::new();
    let root = f.0.join("workspace/build-output");
    let old = f.release_data("0.1.5", true, true);
    fs::create_dir_all(&root).unwrap();
    write_source(&old, Some(PLAYER_JSON), Some(SETTINGS_JSON), true);

    assert!(!migration::inherit(&root).unwrap());
    assert_default_player_preferences(&Prefs::load(&root));
    assert!(!root.join("data").exists());
}

#[test]
fn public_startup_imports_only_with_the_explicit_flag_and_default_profile() {
    use crate::Startup;
    let f = Fixture::new();
    let root = f.nested_current_root();
    let profile = f.0.join("isolated-profile");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir(&profile).unwrap();
    let old = f.release_data("0.1.5", true, true);
    write_source(&old, Some(PLAYER_JSON), Some(SETTINGS_JSON), true);

    for (custom, flags) in [
        (None, vec![]),
        (None, vec!["--other"]),
        (Some(profile.clone()), vec!["--import-legacy"]),
        (Some(root.clone()), vec!["--import-legacy"]),
    ] {
        let startup = Startup::from_options(
            root.clone(),
            custom.clone(),
            flags.into_iter().map(Into::into),
        )
        .unwrap();
        assert!(!startup.import_if_requested().unwrap());
        assert_eq!(startup.install_root, root);
        assert_eq!(startup.profile_root, custom.as_deref().unwrap_or(&root));
        assert!(!root.join("data").exists() && !profile.join("data").exists());
    }
    let explicit = Startup::from_options(root.clone(), None, ["--import-legacy".into()]).unwrap();
    assert!(explicit.import_if_requested().unwrap());
    assert_loaded_player_preferences(&Prefs::load(&root));
    assert_eq!(
        fs::read(root.join("data/credentials.dpapi")).unwrap(),
        VAULT_BYTES
    );
    assert!(!profile.join("data").exists());
}
