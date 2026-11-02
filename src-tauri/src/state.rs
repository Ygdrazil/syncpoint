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
    /// How many syncs may share one SSH master at a time, one semaphore per
    /// master. See [`AppState::session_limit`].
    pub session_limits: Mutex<HashMap<String, Arc<Semaphore>>>,
    /// How many times each profile has been stopped. A one-off run has no channel
    /// to close, so it remembers this count from when it was asked for, and any
    /// change withdraws it. See [`crate::engine::run_detached`].
    pub stop_counts: Mutex<HashMap<String, u64>>,
}

/// Concurrent rsync runs allowed on one SSH master.
///
/// Each run opens a session on the master, and sshd's `MaxSessions` defaults to
/// 10 per network connection. Past it the server answers
/// `Session open refused by peer` and the sync fails outright — so syncing
/// thirty profiles at once failed most of them.
///
/// Set well under the limit rather than at it. Thirty rsyncs to one host
/// saturate the link and the remote disk long before they saturate the session
/// count, so the queue costs nothing real, and it leaves room for the sessions
/// a retry or a manual Sync now opens alongside.
const MAX_SESSIONS_PER_MASTER: usize = 6;

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
            stop_counts: Mutex::new(HashMap::new()),
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

    /// The gate for one SSH master, created on first use. Shared, so every sync
    /// through that master queues on the same one.
    ///
    /// Keyed on the socket rather than the connection's id: two records with the
    /// same login share one master, and each counting up to the limit on its own
    /// would add up past `MaxSessions`.
    pub fn session_limit(&self, connection: &Connection) -> Arc<Semaphore> {
        self.session_limits
            .lock()
            .unwrap()
            .entry(connection.control_path())
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_SESSIONS_PER_MASTER)))
            .clone()
    }

    pub fn note_stopped(&self, profile_id: &str) {
        *self
            .stop_counts
            .lock()
            .unwrap()
            .entry(profile_id.to_string())
            .or_default() += 1;
    }

    pub fn times_stopped(&self, profile_id: &str) -> u64 {
        self.stop_counts
            .lock()
            .unwrap()
            .get(profile_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn profile(&self, id: &str) -> Option<Profile> {
        self.profiles
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// Write a status and return it for publishing, unless `stopped` says the
    /// writer no longer speaks for this profile.
    ///
    /// Checked under the lock the status is written with. Whoever stops a run
    /// closes its channel before writing, so a run that lost that race cannot
    /// land on top of what they wrote, or bring back a deleted profile's status.
    fn record_status(
        &self,
        profile_id: &str,
        state: &str,
        message: Option<String>,
        synced: bool,
        stopped: impl Fn() -> bool,
    ) -> Option<SyncStatus> {
        let mut statuses = self.statuses.lock().unwrap();
        if stopped() {
            // When a transfer last completed is still true, even if the profile
            // has since been pointed somewhere else.
            if let (true, Some(entry)) = (synced, statuses.get_mut(profile_id)) {
                entry.last_sync = Some(now_secs());
            }
            return None;
        }
        let entry = statuses
            .entry(profile_id.to_string())
            .or_insert_with(|| SyncStatus::new(profile_id, state));
        entry.state = state.to_string();
        entry.message = message;
        if synced {
            entry.last_sync = Some(now_secs());
        }
        Some(entry.clone())
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
/// just synced. Use [`mark_synced_unless`] for the success path.
pub fn set_status(app: &AppHandle, profile_id: &str, state: &str, message: Option<String>) {
    update_status(app, profile_id, state, message, false, || false);
}

/// [`set_status`] on behalf of a run, which may have been stopped since it last
/// looked. Returns whether it was written.
pub fn set_status_unless(
    stopped: impl Fn() -> bool,
    app: &AppHandle,
    profile_id: &str,
    state: &str,
    message: Option<String>,
) -> bool {
    update_status(app, profile_id, state, message, false, stopped)
}

/// A sync completed. Stamps `last_sync`, which is what the UI reports.
pub fn mark_synced_unless(
    stopped: impl Fn() -> bool,
    app: &AppHandle,
    profile_id: &str,
    summary: String,
) {
    update_status(app, profile_id, "idle", Some(summary), true, stopped);
}

fn update_status(
    app: &AppHandle,
    profile_id: &str,
    state: &str,
    message: Option<String>,
    synced: bool,
    stopped: impl Fn() -> bool,
) -> bool {
    let app_state = app.state::<AppState>();
    let recorded = app_state.record_status(profile_id, state, message, synced, stopped);
    let Some(status) = recorded else {
        // Refused, but this may be a run ending, and the screen may be out of step
        // with it: it went on reporting progress after the stop, which only a
        // status event clears, and the tray may still be counting it.
        let current = app_state.statuses.lock().unwrap().get(profile_id).cloned();
        if let Some(current) = current {
            let _ = app.emit("sync-status", current);
        }
        refresh_tray(app);
        return false;
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
    true
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

    fn connection(id: &str) -> Connection {
        Connection {
            id: id.into(),
            name: id.into(),
            user: "deploy".into(),
            host: "example.com".into(),
            port: 22,
            identity_file: String::new(),
        }
    }

    #[test]
    fn every_sync_on_a_master_queues_on_the_same_gate() {
        // The gate only limits anything if the profiles sharing a master share
        // it. One per profile would let all thirty through at once, which is
        // the failure it exists to prevent.
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());

        let first = state.session_limit(&connection("c1"));
        let again = state.session_limit(&connection("c1"));
        let mut elsewhere = connection("c2");
        elsewhere.host = "example.org".into();
        let other = state.session_limit(&elsewhere);

        assert!(Arc::ptr_eq(&first, &again), "same master, same gate");
        assert!(!Arc::ptr_eq(&first, &other), "a slow host must not stall another");
        assert_eq!(first.available_permits(), MAX_SESSIONS_PER_MASTER);
        assert!(
            MAX_SESSIONS_PER_MASTER < 10,
            "sshd MaxSessions defaults to 10; at or above it the server refuses sessions"
        );
    }

    #[test]
    fn two_records_for_one_login_cannot_add_up_past_the_limit() {
        // They share one master, so MaxSessions counts them together. With a
        // gate each they could hold twice the limit, which is over sshd's 10.
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());
        let prod = state.session_limit(&connection("c1"));
        let staging = state.session_limit(&connection("c2"));

        let _held: Vec<_> = (0..MAX_SESSIONS_PER_MASTER)
            .map(|i| if i % 2 == 0 { &prod } else { &staging })
            .map(|gate| gate.try_acquire().unwrap())
            .collect();

        assert!(prod.try_acquire().is_err());
        assert!(staging.try_acquire().is_err());
    }

    #[test]
    fn a_stopped_run_cannot_write_over_what_stopped_it() {
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());
        state.record_status("p1", "paused", None, false, || false);

        let written = state.record_status("p1", "error", Some("boom".into()), false, || true);

        assert!(written.is_none());
        let statuses = state.statuses.lock().unwrap();
        assert_eq!(statuses["p1"].state, "paused");
        assert_eq!(statuses["p1"].message, None);
        assert_eq!(statuses["p1"].last_sync, None);
    }

    #[test]
    fn a_transfer_that_finished_after_a_stop_still_counts_as_the_last_sync() {
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());
        state.record_status("p1", "paused", None, false, || false);

        state.record_status("p1", "idle", Some("done".into()), true, || true);

        let statuses = state.statuses.lock().unwrap();
        assert_eq!(statuses["p1"].state, "paused");
        assert!(statuses["p1"].last_sync.is_some());
    }

    #[test]
    fn stops_are_counted_per_profile() {
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());
        assert_eq!(state.times_stopped("p1"), 0);

        state.note_stopped("p1");
        state.note_stopped("p1");

        assert_eq!(state.times_stopped("p1"), 2);
        assert_eq!(state.times_stopped("p2"), 0);
    }

    #[test]
    fn a_stopped_run_cannot_bring_back_a_deleted_profiles_status() {
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());

        state.record_status("gone", "idle", None, true, || true);

        assert!(state.statuses.lock().unwrap().get("gone").is_none());
    }

    #[test]
    fn a_run_still_reports_while_it_is_wanted() {
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());

        let written = state.record_status("p1", "idle", Some("done".into()), true, || false);

        let written = written.unwrap();
        assert_eq!(written.state, "idle");
        assert!(written.last_sync.is_some());
    }

    #[test]
    fn whether_a_run_was_stopped_is_decided_under_the_status_lock() {
        // Checked before taking the lock, a run could pass the check, lose the
        // race to whoever stopped it, and then write over them.
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());

        state.record_status("p1", "idle", None, false, || {
            assert!(state.statuses.try_lock().is_err());
            false
        });
    }

    #[test]
    fn a_second_key_has_its_own_master_and_its_own_gate() {
        // A different key is a separate network connection with its own
        // MaxSessions, so queueing it behind the first would only slow it down.
        let state = AppState::new(PathBuf::from("/tmp"), Config::default());
        let mut other_key = connection("c2");
        other_key.identity_file = "~/.ssh/id_work".into();

        let first = state.session_limit(&connection("c1"));
        let second = state.session_limit(&other_key);

        assert!(!Arc::ptr_eq(&first, &second));
    }
}
