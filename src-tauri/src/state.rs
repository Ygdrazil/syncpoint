//! Shared application state, and the status every window renders from.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::Semaphore;
use tauri::{AppHandle, Emitter, Manager};

use crate::connection::Connection;
use crate::folder::Folder;
use crate::engine::ProfileRuntime;
use crate::profile::Profile;
use crate::rsync::{self, RsyncInfo};
use crate::settings::Settings;
use crate::store::{self, Config};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub profile_id: String,
    /// disabled | idle | pending | syncing | error | paused
    /// `disabled` is a profile paused on its own; `paused` is Pause all.
    /// Both stop the watcher — the UI calls the first one "Paused" too, and
    /// the second "All paused".
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
    pub folders: Mutex<Vec<Folder>>,
    pub profiles: Mutex<Vec<Profile>>,
    pub statuses: Mutex<HashMap<String, SyncStatus>>,
    pub runtimes: Mutex<HashMap<String, ProfileRuntime>>,
    pub rsync_bin: Mutex<Option<RsyncInfo>>,
    /// How many profiles the tray icon currently reflects as syncing.
    pub tray_syncing: Mutex<usize>,
    /// Category the settings window should land on when it next opens.
    pub pending_settings_section: Mutex<Option<String>>,
    /// Set when About is opened from an update toast, so it auto-starts the
    /// download on mount. Same stash-and-take pattern as the settings section.
    pub pending_auto_update: Mutex<Option<bool>>,
    /// Width to restore when the main window leaves compact mode. Kept here
    /// rather than in the view so a reload cannot strand the window at sidebar
    /// width with no way back.
    pub expanded_width: Mutex<Option<f64>>,
    /// How many syncs may share one connection's SSH master at a time, one
    /// semaphore per connection. See [`AppState::session_limit`].
    pub session_limits: Mutex<HashMap<String, Arc<Semaphore>>>,
}

/// Concurrent rsync runs allowed per connection.
///
/// Each run opens a session on that connection's SSH master, and sshd's
/// `MaxSessions` defaults to 10 per connection. Past it the server answers
/// `Session open refused by peer` and the sync fails outright — so syncing
/// thirty profiles at once failed most of them.
///
/// Set well under the limit rather than at it. Thirty rsyncs to one host
/// saturate the link and the remote disk long before they saturate the session
/// count, so the queue costs nothing real, and it leaves room for the sessions
/// a retry or a manual Sync now opens alongside.
const MAX_SESSIONS_PER_CONNECTION: usize = 6;

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
            folders: Mutex::new(config.folders),
            profiles: Mutex::new(config.profiles),
            statuses: Mutex::new(statuses),
            runtimes: Mutex::new(HashMap::new()),
            rsync_bin: Mutex::new(rsync::detect()),
            tray_syncing: Mutex::new(0),
            pending_settings_section: Mutex::new(None),
            pending_auto_update: Mutex::new(None),
            expanded_width: Mutex::new(None),
            session_limits: Mutex::new(HashMap::new()),
        }
    }

    /// The live state as a plain [`Config`], for saving or exporting.
    pub fn snapshot(&self) -> Config {
        Config {
            settings: self.settings.lock().unwrap().clone(),
            connections: self.connections.lock().unwrap().clone(),
            folders: self.folders.lock().unwrap().clone(),
            profiles: self.profiles.lock().unwrap().clone(),
        }
    }

    /// Replace the stored collections. Settings are left alone: an import
    /// carries profiles and connections, not this machine's preferences.
    pub fn replace_collections(&self, config: Config) {
        *self.connections.lock().unwrap() = config.connections;
        *self.folders.lock().unwrap() = config.folders;
        *self.profiles.lock().unwrap() = config.profiles;
    }

    pub fn persist(&self) -> Result<(), String> {
        store::save(&self.config_dir, &self.snapshot())
    }

    /// The gate for one connection, created on first use. Shared, so every sync
    /// through that connection queues on the same one.
    pub fn session_limit(&self, connection_id: &str) -> Arc<Semaphore> {
        self.session_limits
            .lock()
            .unwrap()
            .entry(connection_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_SESSIONS_PER_CONNECTION)))
            .clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sync_on_a_connection_queues_on_the_same_gate() {
        // The gate only limits anything if the profiles sharing a connection
        // share it. One per profile would let all thirty through at once,
        // which is the failure it exists to prevent.
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());

        let first = state.session_limit("c1");
        let again = state.session_limit("c1");
        let other = state.session_limit("c2");

        assert!(Arc::ptr_eq(&first, &again), "same connection, same gate");
        assert!(!Arc::ptr_eq(&first, &other), "a slow host must not stall another");
        assert_eq!(first.available_permits(), MAX_SESSIONS_PER_CONNECTION);
        assert!(
            MAX_SESSIONS_PER_CONNECTION < 10,
            "sshd MaxSessions defaults to 10; at or above it the server refuses sessions"
        );
    }
}
