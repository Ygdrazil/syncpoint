//! Everything the frontend can invoke, plus the helpers those calls share.

use std::collections::HashMap;

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
use crate::folder::{self, Folder, ItemKind};
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

/// Width of the sidebar column. Must match `grid-template-columns` in
/// `_layout.scss`: in compact mode the window is exactly the sidebar.
const SIDEBAR_WIDTH: f64 = 240.0;

/// Shrink the main window to just its sidebar, and back.
///
/// The window is resized rather than only hiding the pane, which would leave a
/// list stretched across a thousand pixels. Done here rather than from the view
/// so it needs no window-sizing permission in the webview, and so the width to
/// come back to survives a reload.
#[tauri::command]
pub fn set_compact(app: AppHandle, compact: bool) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "The main window is not open.".to_string())?;

    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let size = window.inner_size().map_err(|e| e.to_string())?.to_logical::<f64>(scale);
    let state = app.state::<AppState>();

    let (min, width) = if compact {
        *state.expanded_width.lock().unwrap() = Some(size.width);
        // The configured minimum is wider than the sidebar, so it has to come
        // down first or the resize is clamped straight back.
        (SIDEBAR_WIDTH, SIDEBAR_WIDTH)
    } else {
        let width = state.expanded_width.lock().unwrap().take().unwrap_or(1000.0);
        (720.0, width.max(720.0))
    };

    window
        .set_min_size(Some(tauri::LogicalSize::new(min, 480.0)))
        .map_err(|e| format!("Could not resize the window: {e}"))?;
    window
        .set_size(tauri::LogicalSize::new(width, size.height))
        .map_err(|e| format!("Could not resize the window: {e}"))?;
    Ok(())
}

/// Collected once by the settings window on mount, then cleared.
#[tauri::command]
pub fn take_settings_section(state: State<'_, AppState>) -> Option<String> {
    state.pending_settings_section.lock().unwrap().take()
}

/// Open the About window. When `auto_update` is `Some(true)` the About view
/// starts the download immediately on mount, so a click on an update toast
/// lands the user on a downloading About window without a second click.
#[tauri::command]
pub fn open_about(app: AppHandle, auto_update: Option<bool>) -> Result<(), String> {
    if let Some(true) = auto_update {
        *app.state::<AppState>()
            .pending_auto_update
            .lock()
            .unwrap() = Some(true);
    }
    menu::open_about(&app).map_err(|e| format!("Could not open About: {e}"))
}

/// Collected once by the About window on mount, then cleared.
#[tauri::command]
pub fn take_auto_update(state: State<'_, AppState>) -> Option<bool> {
    state.pending_auto_update.lock().unwrap().take()
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
    include_preferences: bool,
) -> Result<usize, String> {
    let bundle = transfer::export(
        &state.snapshot(),
        include_connections,
        include_preferences,
        state::now_secs(),
    );
    let count = bundle.profiles.len();
    let json = serde_json::to_string_pretty(&bundle)
        .map_err(|e| format!("Could not serialize the export: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("Could not write {path}: {e}"))?;
    Ok(count)
}

/// Merge a bundle into the current config. Imported profiles arrive paused,
/// so nothing starts transferring until it has been reviewed and enabled.
#[tauri::command]
pub fn import_config(app: AppHandle, path: String) -> Result<ImportSummary, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("Could not read {path}: {e}"))?;
    let bundle: Bundle =
        serde_json::from_str(&raw).map_err(|e| format!("{path} is not a SyncPoint export: {e}"))?;

    let (summary, stopped) = {
        let state = app.state::<AppState>();
        let mut config = state.snapshot();
        let summary = transfer::import(&mut config, bundle)?;
        let stopped: Vec<String> = config
            .profiles
            .iter()
            .filter(|p| !p.enabled)
            .map(|p| p.id.clone())
            .collect();
        // replace_collections deliberately leaves settings alone (see its own
        // doc comment) — preferences opted into the bundle are the one case an
        // import does carry, so they're written back separately.
        if summary.preferences_applied {
            *state.settings.lock().unwrap() = config.settings.clone();
        }
        state.replace_collections(config);
        (summary, stopped)
    };

    // The config now says these are off; the watchers still running from before
    // the import have to be told. Without this a profile the import replaced —
    // or one whose connection it repointed — keeps syncing on the old terms.
    for id in stopped {
        engine::stop(&app, &id);
        state::set_status(&app, &id, "disabled", None);
    }

    commit(&app)?;
    Ok(summary)
}

#[tauri::command]
pub fn list_folders(state: State<'_, AppState>) -> Vec<Folder> {
    state.folders.lock().unwrap().clone()
}

/// Create or rename a folder, or move it under a different parent.
#[tauri::command]
pub fn save_folder(app: AppHandle, mut incoming: Folder) -> Result<Folder, String> {
    incoming.validate()?;
    if incoming.id.trim().is_empty() {
        incoming.id = Uuid::new_v4().to_string();
    }

    {
        let state = app.state::<AppState>();
        let mut config = state.snapshot();
        // Refused rather than normalised away, so the user is told instead of
        // watching their folder silently jump to the top level.
        folder::check_move(&config.folders, &incoming.id, incoming.parent_id.as_deref())?;
        match config.folders.iter_mut().find(|f| f.id == incoming.id) {
            Some(existing) => *existing = incoming.clone(),
            None => {
                // A new folder goes after everything already in its level.
                incoming.position = u32::MAX;
                config.folders.push(incoming.clone());
            }
        }
        folder::renumber(&mut config);
        // Report the position it actually got, not the sentinel.
        if let Some(stored) = config.folders.iter().find(|f| f.id == incoming.id) {
            incoming = stored.clone();
        }
        state.replace_collections(config);
    }
    commit(&app)?;
    Ok(incoming)
}

/// Delete a folder, lifting everything inside it up one level.
///
/// Returns how many items moved. Profiles are never deleted with a folder —
/// only the grouping goes.
#[tauri::command]
pub fn delete_folder(app: AppHandle, id: String) -> Result<FolderDeletion, String> {
    let removed = {
        let state = app.state::<AppState>();
        let mut config = state.snapshot();
        let removed = folder::remove(&mut config, &id);
        *state.folders.lock().unwrap() = config.folders;
        *state.profiles.lock().unwrap() = config.profiles;
        removed
    };

    // Dropping the records is not enough: a watcher left running would keep
    // syncing a profile that no longer exists anywhere on screen.
    for profile_id in &removed.profile_ids {
        engine::stop(&app, profile_id);
        app.state::<AppState>().statuses.lock().unwrap().remove(profile_id);
    }

    commit(&app)?;
    Ok(FolderDeletion {
        profiles: removed.profile_ids.len(),
        folders: removed.folders,
    })
}

/// What deleting a folder took with it, for the message afterwards.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderDeletion {
    profiles: usize,
    /// Nested folders, not counting the one that was deleted.
    folders: usize,
}

/// Drop a folder or profile into `folder_id` (the top level with `None`),
/// immediately before `before` — the single operation a drag ends in.
///
/// Applied to a copy so a refused move leaves the stored config untouched
/// rather than half-applied.
#[tauri::command]
pub fn move_item(
    app: AppHandle,
    kind: ItemKind,
    id: String,
    folder_id: Option<String>,
    before: Option<String>,
) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        let mut config = state.snapshot();
        folder::move_item(&mut config, kind, &id, folder_id.as_deref(), before.as_deref())?;
        state.replace_collections(config);
    }
    commit(&app)
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

/// Delete a connection and pause every profile that referenced it. Those
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
        let mut config = state.snapshot();
        match config.profiles.iter_mut().find(|p| p.id == incoming.id) {
            Some(existing) => *existing = incoming.clone(),
            None => {
                // A new profile goes after everything already in its level.
                incoming.position = u32::MAX;
                config.profiles.push(incoming.clone());
            }
        }
        folder::renumber(&mut config);
        if let Some(stored) = config.profiles.iter().find(|p| p.id == incoming.id) {
            incoming = stored.clone();
        }
        state.replace_collections(config);
    }
    app.state::<AppState>().persist()?;

    engine::stop(&app, &incoming.id);
    if incoming.enabled {
        start_unless_paused(&app, &incoming)?;
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

/// Act on a `syncpoint://` URL.
///
/// Only `new-profile?source=<path>` is understood: it raises the window and
/// asks the frontend to open the editor prefilled with that folder. Nothing is
/// saved and no transfer starts, so a link from outside the app can never sync
/// anything on its own.
pub fn handle_deep_link(app: &AppHandle, url: &url::Url) {
    if url.host_str() != Some("new-profile") && url.path().trim_matches('/') != "new-profile" {
        return;
    }
    let Some(source) = url
        .query_pairs()
        .find(|(k, _)| k == "source")
        .map(|(_, v)| v.into_owned())
    else {
        return;
    };

    menu::show_main(app);
    let _ = app.emit("new-profile-from", source);
}

/// Start a profile unless the app is globally paused.
///
/// Every path that would begin watching goes through here, so a paused app
/// cannot be tricked into syncing by saving or enabling a profile.
fn start_unless_paused(app: &AppHandle, profile: &Profile) -> Result<(), String> {
    if app.state::<AppState>().settings.lock().unwrap().paused {
        state::set_status(app, &profile.id, "paused", None);
        return Ok(());
    }
    engine::start(app, profile)
}

/// Suspend or resume every profile at once.
///
/// Profiles keep their own `enabled` flag untouched, so resuming starts exactly
/// what was running before and nothing that was deliberately switched off.
#[tauri::command]
pub fn set_paused(app: AppHandle, paused: bool) -> Result<(), String> {
    app.state::<AppState>().settings.lock().unwrap().paused = paused;
    commit(&app)?;

    let profiles = app.state::<AppState>().profiles.lock().unwrap().clone();
    for profile in profiles.iter().filter(|p| p.enabled) {
        if paused {
            engine::stop(&app, &profile.id);
            state::set_status(&app, &profile.id, "paused", None);
        } else if let Err(err) = engine::start(&app, profile) {
            state::set_status(&app, &profile.id, "error", Some(err));
        }
    }
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
        start_unless_paused(&app, &profile)
    } else {
        engine::stop(&app, &id);
        state::set_status(&app, &id, "disabled", None);
        Ok(())
    }
}

/// Queue an immediate sync. A running profile gets it on its existing loop; a
/// paused one gets a detached one-off pass so "Sync now" still works.
#[tauri::command]
pub fn sync_now(app: AppHandle, id: String) -> Result<(), String> {
    let profile = {
        let state = app.state::<AppState>();
        let profiles = state.profiles.lock().unwrap();
        profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| "Profile not found.".to_string())?
    };
    queue_sync(&app, &profile)
}

/// Queue one sync for a profile.
///
/// A watched profile gets it on its existing loop, which keeps the debounce and
/// coalescing behaviour. One that is not running — paused either way — gets a
/// detached one-off pass instead, so an explicit request always does something.
fn queue_sync(app: &AppHandle, profile: &Profile) -> Result<(), String> {
    if engine::trigger(app, &profile.id, Trigger::Manual) {
        return Ok(());
    }
    profile.validate()?;

    let app = app.clone();
    let profile = profile.clone();
    tauri::async_runtime::spawn(async move {
        engine::run_once(&app, &profile).await;
    });
    Ok(())
}

/// Sync every enabled profile at once.
///
/// Paused profiles are left alone: they were switched off deliberately, and
/// pushing their files is exactly what the user asked not to happen. A profile
/// that cannot start reports on itself and does not stop the others.
///
/// This works while paused — pausing stops *automatic* syncing, and an explicit
/// request is not that.
#[tauri::command]
pub fn sync_all(app: AppHandle) -> Result<usize, String> {
    let state = app.state::<AppState>();
    let profiles: Vec<Profile> = state
        .profiles
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.enabled)
        .cloned()
        .collect();
    let queued = profiles.len();

    // Grouped by connection, and each group opens its own SSH master before its
    // profiles start.
    //
    // Every rsync runs its own ssh with `ControlMaster=auto`. Started together,
    // they all find no socket, all try to become the master, and every loser
    // reports `ControlSocket ... already exists, disabling multiplexing` and
    // dials its own connection — one handshake per profile, which is the cost
    // the socket exists to avoid.
    //
    // A group per connection rather than one queue for all of them, so a host
    // that is down holds up only its own profiles for the connect timeout.
    let mut by_connection: HashMap<String, Vec<Profile>> = HashMap::new();
    for profile in profiles {
        by_connection
            .entry(profile.connection_id.clone())
            .or_default()
            .push(profile);
    }

    for (id, group) in by_connection {
        let connection = state.connection(&id);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(connection) = connection {
                // A failed warm-up is not worth reporting: the sync behind it
                // makes the same connection and reports the real reason.
                let _ = connection::test(&connection).await;
            }
            for profile in &group {
                if let Err(err) = queue_sync(&app, profile) {
                    state::set_status(&app, &profile.id, "error", Some(err));
                }
            }
        });
    }

    Ok(queued)
}
