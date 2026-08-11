use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::connection::Connection;
use crate::profile::Profile;
use crate::settings::Settings;

const CONFIG_FILE: &str = "config.json";
/// Pre-connections layout: a bare array of profiles with SSH fields inline.
const LEGACY_FILE: &str = "profiles.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE)
}

/// The app used to be called LiveSync, and its bundle identifier changed with
/// the rename. `app_config_dir()` derives from that identifier, so without this
/// every existing profile and connection would simply vanish on upgrade.
///
/// Copies rather than moves: if the rename is ever rolled back, the old install
/// still finds its config where it left it.
pub fn adopt_legacy_dir(dir: &Path, legacy_dir: &Path) -> bool {
    let target = config_path(dir);
    let source = config_path(legacy_dir);
    if target.exists() || !source.exists() {
        return false;
    }
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    fs::copy(&source, &target).is_ok()
}

pub fn load(dir: &Path) -> Config {
    let path = config_path(dir);
    if let Ok(raw) = fs::read_to_string(&path) {
        return serde_json::from_str(&raw).unwrap_or_default();
    }

    // No config.json yet — adopt a pre-connections profiles.json if one is
    // there. The old file is left untouched as a fallback.
    let legacy = dir.join(LEGACY_FILE);
    match fs::read_to_string(&legacy) {
        Ok(raw) => {
            let config = migrate_legacy(&raw);
            let _ = save(dir, &config);
            config
        }
        Err(_) => Config::default(),
    }
}

/// Turn each distinct `user@host:port` found in the old profiles into one
/// named connection, and point the profiles at it.
fn migrate_legacy(raw: &str) -> Config {
    let Ok(entries) = serde_json::from_str::<Vec<Value>>(raw) else {
        return Config::default();
    };

    let mut connections: Vec<Connection> = Vec::new();
    let mut by_target: HashMap<String, String> = HashMap::new();
    let mut profiles = Vec::new();

    for mut entry in entries {
        let user = take_string(&mut entry, "sshUser");
        let host = take_string(&mut entry, "sshHost");
        let port = entry
            .get("sshPort")
            .and_then(Value::as_u64)
            .unwrap_or(22)
            .clamp(1, 65535) as u16;

        if !host.is_empty() {
            let key = format!("{user}@{host}:{port}");
            let id = by_target.entry(key).or_insert_with(|| {
                let id = Uuid::new_v4().to_string();
                let name = if port == 22 {
                    host.clone()
                } else {
                    format!("{host}:{port}")
                };
                connections.push(Connection {
                    id: id.clone(),
                    name,
                    user: user.clone(),
                    host: host.clone(),
                    port,
                    identity_file: String::new(),
                });
                id
            });
            if let Some(object) = entry.as_object_mut() {
                object.insert("connectionId".into(), Value::String(id.clone()));
            }
        }

        if let Ok(profile) = serde_json::from_value::<Profile>(entry) {
            profiles.push(profile);
        }
    }

    Config {
        settings: Settings::default(),
        connections,
        profiles,
    }
}

fn take_string(entry: &mut Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub fn save(dir: &Path, config: &Config) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("Could not create config directory: {e}"))?;
    let json =
        serde_json::to_string_pretty(config).map_err(|e| format!("Could not serialize config: {e}"))?;

    // Write to a sibling temp file and rename, so a crash mid-write cannot
    // leave a truncated config.json behind.
    let path = config_path(dir);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("Could not write config: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("Could not save config: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r#"[
      {
        "id": "a",
        "name": "Alpha",
        "source": "/tmp/a",
        "sshUser": "deploy",
        "sshHost": "example.com",
        "sshPort": 22,
        "remotePath": "/srv/a",
        "enabled": true,
        "delete": true,
        "excludes": [".git"],
        "debounceMs": 500
      },
      {
        "id": "b",
        "name": "Beta",
        "source": "/tmp/b",
        "sshUser": "deploy",
        "sshHost": "example.com",
        "sshPort": 22,
        "remotePath": "/srv/b",
        "enabled": false,
        "delete": false,
        "excludes": [],
        "debounceMs": 800
      },
      {
        "id": "c",
        "name": "Gamma",
        "source": "/tmp/c",
        "sshUser": "root",
        "sshHost": "other.example.com",
        "sshPort": 2222,
        "remotePath": "/srv/c",
        "enabled": false,
        "delete": true,
        "excludes": [],
        "debounceMs": 500
      }
    ]"#;

    #[test]
    fn migration_folds_identical_targets_into_one_connection() {
        let config = migrate_legacy(LEGACY);
        assert_eq!(config.profiles.len(), 3);
        // Alpha and Beta share deploy@example.com:22; Gamma is its own target.
        assert_eq!(config.connections.len(), 2);

        let alpha = &config.profiles[0];
        let beta = &config.profiles[1];
        let gamma = &config.profiles[2];
        assert_eq!(alpha.connection_id, beta.connection_id);
        assert_ne!(alpha.connection_id, gamma.connection_id);
    }

    #[test]
    fn migration_preserves_profile_settings() {
        let config = migrate_legacy(LEGACY);
        let beta = &config.profiles[1];
        assert_eq!(beta.name, "Beta");
        assert_eq!(beta.remote_path, "/srv/b");
        assert_eq!(beta.debounce_ms, 800);
        assert!(!beta.delete);
        assert!(!beta.enabled);
        assert!(config.profiles[0].enabled);
        assert_eq!(config.profiles[0].excludes, vec![".git".to_string()]);
    }

    #[test]
    fn migration_carries_over_user_host_and_port() {
        let config = migrate_legacy(LEGACY);
        let gamma = &config.profiles[2];
        let connection = config
            .connections
            .iter()
            .find(|c| c.id == gamma.connection_id)
            .expect("gamma's connection exists");

        assert_eq!(connection.user, "root");
        assert_eq!(connection.host, "other.example.com");
        assert_eq!(connection.port, 2222);
        // Non-default ports are kept visible in the generated name.
        assert_eq!(connection.name, "other.example.com:2222");
    }

    #[test]
    fn migration_survives_junk_input() {
        assert!(migrate_legacy("not json").connections.is_empty());
        assert!(migrate_legacy("[]").profiles.is_empty());
    }

    #[test]
    fn config_is_adopted_from_the_pre_rename_directory() {
        let root = std::env::temp_dir().join(format!("syncpoint-rename-{}", Uuid::new_v4()));
        let legacy = root.join("com.example.livesync-next");
        let current = root.join("com.example.syncpoint");

        let mut original = Config::default();
        original.settings.default_remote_prefix = "/srv/apps".into();
        save(&legacy, &original).unwrap();

        assert!(adopt_legacy_dir(&current, &legacy));
        assert_eq!(load(&current).settings.default_remote_prefix, "/srv/apps");
        // The old copy is left in place rather than moved.
        assert!(config_path(&legacy).exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn adoption_never_overwrites_an_existing_config() {
        let root = std::env::temp_dir().join(format!("syncpoint-rename-{}", Uuid::new_v4()));
        let legacy = root.join("legacy");
        let current = root.join("current");

        let mut old = Config::default();
        old.settings.default_remote_prefix = "/old".into();
        save(&legacy, &old).unwrap();

        let mut new = Config::default();
        new.settings.default_remote_prefix = "/new".into();
        save(&current, &new).unwrap();

        assert!(!adopt_legacy_dir(&current, &legacy), "must not run twice");
        assert_eq!(load(&current).settings.default_remote_prefix, "/new");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn adoption_is_a_no_op_without_a_legacy_config() {
        let root = std::env::temp_dir().join(format!("syncpoint-rename-{}", Uuid::new_v4()));
        assert!(!adopt_legacy_dir(&root.join("current"), &root.join("missing")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_survive_a_disk_round_trip() {
        let dir = std::env::temp_dir().join(format!("syncpoint-test-{}", Uuid::new_v4()));
        let mut config = Config::default();
        config.settings.theme = crate::settings::Theme::Dark;
        config.settings.open_at_startup = true;
        config.settings.default_excludes = vec!["target".into(), "*.tmp".into()];
        save(&dir, &config).expect("save works");

        let loaded = load(&dir);
        assert_eq!(loaded.settings.theme, crate::settings::Theme::Dark);
        assert!(loaded.settings.open_at_startup);
        // A customised list must come back verbatim rather than reverting to
        // the seeded defaults.
        assert_eq!(loaded.settings.default_excludes, vec!["target", "*.tmp"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_written_before_settings_existed_still_loads() {
        let dir = std::env::temp_dir().join(format!("syncpoint-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            config_path(&dir),
            r#"{"connections":[],"profiles":[{"id":"a","name":"A","source":"/tmp",
               "connectionId":"c","remotePath":"/srv","enabled":false,"delete":true,
               "excludes":[],"debounceMs":500}]}"#,
        )
        .unwrap();

        let loaded = load(&dir);
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.settings, crate::settings::Settings::default());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("syncpoint-test-{}", Uuid::new_v4()));
        let config = migrate_legacy(LEGACY);
        save(&dir, &config).expect("save works");

        let loaded = load(&dir);
        assert_eq!(loaded.profiles.len(), 3);
        assert_eq!(loaded.connections.len(), 2);
        assert_eq!(loaded.profiles[2].connection_id, config.profiles[2].connection_id);

        let _ = fs::remove_dir_all(&dir);
    }
}
