//! Everything the frontend can invoke, plus the helpers those calls share.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use uuid::Uuid;

use crate::connection::{self, Connection};
use crate::engine::{self, Trigger};
use crate::state::{self, AppState, SyncStatus};
use crate::menu;
use crate::profile::Profile;
use crate::rsync::RsyncInfo;
use crate::settings::Settings;
use crate::store;
use crate::transfer::{self, Bundle, ImportSummary};
use crate::version;

/// Tell every window the stored config changed. Connections are edited in the
/// settings window but shown in the main one, so neither can rely on only its
/// own actions to stay current.
fn broadcast_change(app: &AppHandle) {
    let _ = app.emit("config-changed", ());
}

/// Save the current state and tell every window about it. Paired in every
/// mutating command, so they travel together rather than being remembered.
fn commit(app: &AppHandle) -> Result<(), String> {
    app.state::<AppState>().persist()?;
    broadcast_change(app);
    Ok(())
}

/// Open the settings window, optionally on a specific category.
///
/// A window being created has no listener yet, so the section is stashed for it
/// to collect on mount. A window that already exists is told by event instead —
/// stashing for that case would leave a stale section behind for the next open.
#[tauri::command]
pub fn open_settings(app: AppHandle, section: Option<String>) -> Result<(), String> {
    let already_open = app.get_webview_window(menu::SETTINGS_WINDOW).is_some();
    if let Some(section) = section.clone() {
        if already_open {
            let _ = app.emit("settings-navigate", section);
        } else {
            *app.state::<AppState>()
                .pending_settings_section
                .lock()
                .unwrap() = Some(section);
        }
    }
    menu::open_settings(&app).map_err(|e| format!("Could not open settings: {e}"))
}

/// Collected once by the settings window on mount, then cleared.
#[tauri::command]
pub fn take_settings_section(state: State<'_, AppState>) -> Option<String> {
    state.pending_settings_section.lock().unwrap().take()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    name: String,
    version: String,
    is_dev: bool,
    /// Surfaced so a bug report can name the exact file to look at.
    config_path: String,
    rsync: Option<RsyncInfo>,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        name: "SyncPoint".to_string(),
        version: version::describe(),
        is_dev: tauri::is_dev(),
        config_path: store::config_path(&state.config_dir).display().to_string(),
        rsync: state.rsync_bin.lock().unwrap().clone(),
    }
}

/// The OS launch-at-login registration is the source of truth for
/// `open_at_startup` — the user can revoke it in System Settings without the
/// app knowing, so reporting the stored value would be a lie.
#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    let mut current = app.state::<AppState>().settings.lock().unwrap().clone();
    if let Ok(registered) = app.autolaunch().is_enabled() {
        current.open_at_startup = registered;
    }
    current
}

#[tauri::command]
pub fn save_settings(app: AppHandle, mut incoming: Settings) -> Result<Settings, String> {
    incoming.validate()?;
    incoming.normalize();
    apply_autostart(&app, incoming.open_at_startup)?;
    *app.state::<AppState>().settings.lock().unwrap() = incoming.clone();
    commit(&app)?;
    Ok(incoming)
}

fn apply_autostart(app: &AppHandle, wanted: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    let registered = manager.is_enabled().unwrap_or(false);
    if wanted == registered {
        return Ok(());
    }
    let result = if wanted {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|e| format!("Could not change the launch-at-login setting: {e}"))
}

#[tauri::command]
pub fn export_config(
    state: State<'_, AppState>,
    path: String,
    include_connections: bool,
) -> Result<usize, String> {
    let bundle = transfer::export(&state.snapshot(), include_connections, state::now_secs());
    let count = bundle.profiles.len();
    let json = serde_json::to_string_pretty(&bundle)
        .map_err(|e| format!("Could not serialize the export: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("Could not write {path}: {e}"))?;
    Ok(count)
}

/// Merge a bundle into the current config. Imported profiles arrive disabled,
/// so nothing starts transferring until it has been reviewed and enabled.
#[tauri::command]
pub fn import_config(app: AppHandle, path: String) -> Result<ImportSummary, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("Could not read {path}: {e}"))?;
    let bundle: Bundle =
        serde_json::from_str(&raw).map_err(|e| format!("{path} is not a SyncPoint export: {e}"))?;

    let summary = {
        let state = app.state::<AppState>();
        let mut config = state.snapshot();
        let summary = transfer::import(&mut config, bundle)?;
        state.replace_collections(config);
        summary
    };

    commit(&app)?;
    Ok(summary)
}

#[tauri::command]
pub fn list_profiles(state: State<'_, AppState>) -> Vec<Profile> {
    state.profiles.lock().unwrap().clone()
}

#[tauri::command]
pub fn list_connections(state: State<'_, AppState>) -> Vec<Connection> {
    state.connections.lock().unwrap().clone()
}

#[tauri::command]
pub fn list_statuses(state: State<'_, AppState>) -> Vec<SyncStatus> {
    state.statuses.lock().unwrap().values().cloned().collect()
}

#[tauri::command]
pub fn rsync_info(state: State<'_, AppState>) -> Option<RsyncInfo> {
    state.rsync_bin.lock().unwrap().clone()
}

/// Create or update a connection. Profiles resolve connections per sync, so an
/// edit here reaches every profile using it without restarting their watchers.
#[tauri::command]
pub fn save_connection(app: AppHandle, mut incoming: Connection) -> Result<Connection, String> {
    incoming.validate()?;
    if incoming.id.trim().is_empty() {
        incoming.id = Uuid::new_v4().to_string();
    }

    {
        let state = app.state::<AppState>();
        let mut connections = state.connections.lock().unwrap();
        match connections.iter_mut().find(|c| c.id == incoming.id) {
            Some(existing) => *existing = incoming.clone(),
            None => connections.push(incoming.clone()),
        }
    }
    commit(&app)?;
    Ok(incoming)
}

/// Delete a connection and disable every profile that referenced it. Those
/// profiles keep their settings and show an error until a new connection is
/// picked. Returns their names so the UI can say what stopped.
#[tauri::command]
pub fn delete_connection(app: AppHandle, id: String) -> Result<Vec<String>, String> {
    let orphaned: Vec<Profile> = {
        let state = app.state::<AppState>();
        let mut profiles = state.profiles.lock().unwrap();
        let mut orphaned = Vec::new();
        for profile in profiles.iter_mut().filter(|p| p.connection_id == id) {
            profile.enabled = false;
            orphaned.push(profile.clone());
        }
        state.connections.lock().unwrap().retain(|c| c.id != id);
        orphaned
    };

    app.state::<AppState>().persist()?;

    for profile in &orphaned {
        engine::stop(&app, &profile.id);
        state::set_status(
            &app,
            &profile.id,
            "error",
            Some("The connection this profile used was deleted. Pick a new one.".into()),
        );
    }
    broadcast_change(&app);
    Ok(orphaned.into_iter().map(|p| p.name).collect())
}

/// Open a real SSH session to verify the connection works. Also primes the
/// ControlMaster socket, so the next sync skips the handshake.
#[tauri::command]
pub async fn test_connection(candidate: Connection) -> Result<String, String> {
    connection::test(&candidate).await
}

/// Create or update a profile. The watcher is restarted from the saved values
/// so an edit to source, excludes or debounce takes effect immediately.
#[tauri::command]
pub fn save_profile(app: AppHandle, mut incoming: Profile) -> Result<Profile, String> {
    incoming.validate()?;
    if app
        .state::<AppState>()
        .connection(&incoming.connection_id)
        .is_none()
    {
        return Err("Pick a connection for this profile.".into());
    }
    if incoming.id.trim().is_empty() {
        incoming.id = Uuid::new_v4().to_string();
    }

    {
        let state = app.state::<AppState>();
        let mut profiles = state.profiles.lock().unwrap();
        match profiles.iter_mut().find(|p| p.id == incoming.id) {
            Some(existing) => *existing = incoming.clone(),
            None => profiles.push(incoming.clone()),
        }
    }
    app.state::<AppState>().persist()?;

    engine::stop(&app, &incoming.id);
    if incoming.enabled {
        engine::start(&app, &incoming)?;
    } else {
        state::set_status(&app, &incoming.id, "disabled", None);
    }
    broadcast_change(&app);
    Ok(incoming)
}

#[tauri::command]
pub fn delete_profile(app: AppHandle, id: String) -> Result<(), String> {
    engine::stop(&app, &id);
    {
        let state = app.state::<AppState>();
        state.profiles.lock().unwrap().retain(|p| p.id != id);
        state.statuses.lock().unwrap().remove(&id);
    }
    commit(&app)?;
    Ok(())
}

#[tauri::command]
pub fn set_profile_enabled(app: AppHandle, id: String, enabled: bool) -> Result<(), String> {
    let profile = {
        let state = app.state::<AppState>();
        let mut profiles = state.profiles.lock().unwrap();
        let profile = profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| "Profile not found.".to_string())?;
        profile.enabled = enabled;
        profile.clone()
    };
    commit(&app)?;

    if enabled {
        engine::start(&app, &profile)
    } else {
        engine::stop(&app, &id);
        state::set_status(&app, &id, "disabled", None);
        Ok(())
    }
}

/// Queue an immediate sync. A running profile gets it on its existing loop; a
/// disabled one gets a detached one-off pass so "Sync now" still works.
#[tauri::command]
pub fn sync_now(app: AppHandle, id: String) -> Result<(), String> {
    if engine::trigger(&app, &id, Trigger::Manual) {
        return Ok(());
    }
    let profile = {
        let state = app.state::<AppState>();
        let profiles = state.profiles.lock().unwrap();
        profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| "Profile not found.".to_string())?
    };
    profile.validate()?;
    tauri::async_runtime::spawn(async move {
        engine::run_once(&app, &profile).await;
    });
    Ok(())
}
