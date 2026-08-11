//! Shared application state, and the status every window renders from.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::connection::Connection;
use crate::engine::ProfileRuntime;
use crate::profile::Profile;
use crate::rsync::{self, RsyncInfo};
use crate::settings::Settings;
use crate::store::{self, Config};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub profile_id: String,
    /// disabled | idle | pending | syncing | error
    pub state: String,
    pub message: Option<String>,
    /// Unix seconds of the last successful sync.
    pub last_sync: Option<u64>,
}

impl SyncStatus {
    fn new(profile_id: &str, state: &str) -> Self {
        Self {
            profile_id: profile_id.to_string(),
            state: state.to_string(),
            message: None,
            last_sync: None,
        }
    }
}


pub struct AppState {
    pub config_dir: PathBuf,
    pub settings: Mutex<Settings>,
    pub connections: Mutex<Vec<Connection>>,
    pub profiles: Mutex<Vec<Profile>>,
    pub statuses: Mutex<HashMap<String, SyncStatus>>,
    pub runtimes: Mutex<HashMap<String, ProfileRuntime>>,
    pub rsync_bin: Mutex<Option<RsyncInfo>>,
    /// How many profiles the tray icon currently reflects as syncing.
    pub tray_syncing: Mutex<usize>,
    /// Category the settings window should land on when it next opens.
    pub pending_settings_section: Mutex<Option<String>>,
}

impl AppState {
    pub fn new(config_dir: PathBuf, config: Config) -> Self {
        let statuses = config
            .profiles
            .iter()
            .map(|p| {
                let state = if p.enabled { "idle" } else { "disabled" };
                (p.id.clone(), SyncStatus::new(&p.id, state))
            })
            .collect();
        Self {
            config_dir,
            settings: Mutex::new(config.settings),
            connections: Mutex::new(config.connections),
            profiles: Mutex::new(config.profiles),
            statuses: Mutex::new(statuses),
            runtimes: Mutex::new(HashMap::new()),
            rsync_bin: Mutex::new(rsync::detect()),
            tray_syncing: Mutex::new(0),
            pending_settings_section: Mutex::new(None),
        }
    }

    /// The live state as a plain [`Config`], for saving or exporting.
    pub fn snapshot(&self) -> Config {
        Config {
            settings: self.settings.lock().unwrap().clone(),
            connections: self.connections.lock().unwrap().clone(),
            profiles: self.profiles.lock().unwrap().clone(),
        }
    }

    /// Replace the stored collections. Settings are left alone: an import
    /// carries profiles and connections, not this machine's preferences.
    pub fn replace_collections(&self, config: Config) {
        *self.connections.lock().unwrap() = config.connections;
        *self.profiles.lock().unwrap() = config.profiles;
    }

    pub fn persist(&self) -> Result<(), String> {
        store::save(&self.config_dir, &self.snapshot())
    }

    pub fn connection(&self, id: &str) -> Option<Connection> {
        self.connections
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.id == id)
            .cloned()
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Update a profile's status and push it to the frontend.
///
/// Deliberately does not touch `last_sync`: only a transfer that actually
/// finished may stamp that, or merely enabling a profile would claim it had
/// just synced. Use [`mark_synced`] for the success path.
pub fn set_status(app: &AppHandle, profile_id: &str, state: &str, message: Option<String>) {
    update_status(app, profile_id, state, message, false)
}

/// A sync completed. Stamps `last_sync`, which is what the UI reports.
pub fn mark_synced(app: &AppHandle, profile_id: &str, summary: String) {
    update_status(app, profile_id, "idle", Some(summary), true)
}

fn update_status(
    app: &AppHandle,
    profile_id: &str,
    state: &str,
    message: Option<String>,
    synced: bool,
) {
    let status = {
        let app_state = app.state::<AppState>();
        let mut statuses = app_state.statuses.lock().unwrap();
        let entry = statuses
            .entry(profile_id.to_string())
            .or_insert_with(|| SyncStatus::new(profile_id, state));
        entry.state = state.to_string();
        entry.message = message;
        if synced {
            entry.last_sync = Some(now_secs());
        }
        entry.clone()
    };
    // Watcher and validation failures never reach rsync's own logging, so
    // status changes get their own stderr line to keep transcripts complete.
    eprintln!(
        "[syncpoint {} status] {}{}",
        status.profile_id,
        status.state,
        status
            .message
            .as_deref()
            .map(|m| format!(" — {m}"))
            .unwrap_or_default()
    );
    let _ = app.emit("sync-status", status);
    refresh_tray(app);
}

/// Keep the tray icon in step with whether *any* profile is mid-sync. Called
/// from `set_status` because that is the one place every state change passes
/// through. Redundant updates are skipped so a burst of profiles finishing
/// together does not thrash the icon.
fn refresh_tray(app: &AppHandle) {
    let state = app.state::<AppState>();
    let syncing = state
        .statuses
        .lock()
        .unwrap()
        .values()
        .filter(|s| s.state == "syncing")
        .count();

    {
        let mut last = state.tray_syncing.lock().unwrap();
        if *last == syncing {
            return;
        }
        *last = syncing;
    }
    crate::tray::set_syncing(app, syncing);
}
