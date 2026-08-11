mod connection;
mod engine;
mod menu;
mod profile;
mod rsync;
mod settings;
mod store;
mod transfer;
mod tray;
mod version;

use connection::Connection;
use engine::{AppState, SyncStatus, Trigger};
use profile::Profile;
use rsync::RsyncInfo;
use serde::Serialize;
use settings::Settings;
use transfer::{Bundle, ImportSummary};
use tauri_plugin_autostart::ManagerExt;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

/// Tell every window the stored config changed. Connections are edited in the
/// settings window but shown in the main one, so neither can rely on only its
/// own actions to stay current.
fn broadcast_change(app: &AppHandle) {
    let _ = app.emit("config-changed", ());
}

/// Open the settings window, optionally on a specific category.
///
/// A window being created has no listener yet, so the section is stashed for it
/// to collect on mount. A window that already exists is told by event instead —
/// stashing for that case would leave a stale section behind for the next open.
#[tauri::command]
fn open_settings(app: AppHandle, section: Option<String>) -> Result<(), String> {
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
fn take_settings_section(state: State<'_, AppState>) -> Option<String> {
    state.pending_settings_section.lock().unwrap().take()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    name: String,
    version: String,
    is_dev: bool,
    /// Surfaced so a bug report can name the exact file to look at.
    config_path: String,
    rsync: Option<RsyncInfo>,
}

#[tauri::command]
fn app_info(state: State<'_, AppState>) -> AppInfo {
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
fn get_settings(app: AppHandle) -> Settings {
    let mut current = app.state::<AppState>().settings.lock().unwrap().clone();
    if let Ok(registered) = app.autolaunch().is_enabled() {
        current.open_at_startup = registered;
    }
    current
}

#[tauri::command]
fn save_settings(app: AppHandle, mut incoming: Settings) -> Result<Settings, String> {
    incoming.validate()?;
    incoming.normalize();
    apply_autostart(&app, incoming.open_at_startup)?;
    *app.state::<AppState>().settings.lock().unwrap() = incoming.clone();
    app.state::<AppState>().persist()?;
    broadcast_change(&app);
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

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tauri::command]
fn export_config(
    state: State<'_, AppState>,
    path: String,
    include_connections: bool,
) -> Result<usize, String> {
    let config = store::Config {
        settings: state.settings.lock().unwrap().clone(),
        connections: state.connections.lock().unwrap().clone(),
        profiles: state.profiles.lock().unwrap().clone(),
    };
    let bundle = transfer::export(&config, include_connections, now_secs());
    let count = bundle.profiles.len();
    let json = serde_json::to_string_pretty(&bundle)
        .map_err(|e| format!("Could not serialize the export: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("Could not write {path}: {e}"))?;
    Ok(count)
}

/// Merge a bundle into the current config. Imported profiles arrive disabled,
/// so nothing starts transferring until it has been reviewed and enabled.
#[tauri::command]
fn import_config(app: AppHandle, path: String) -> Result<ImportSummary, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("Could not read {path}: {e}"))?;
    let bundle: Bundle =
        serde_json::from_str(&raw).map_err(|e| format!("{path} is not a SyncPoint export: {e}"))?;

    let summary = {
        let state = app.state::<AppState>();
        let mut config = store::Config {
            settings: state.settings.lock().unwrap().clone(),
            connections: state.connections.lock().unwrap().clone(),
            profiles: state.profiles.lock().unwrap().clone(),
        };
        let summary = transfer::import(&mut config, bundle)?;
        *state.connections.lock().unwrap() = config.connections;
        *state.profiles.lock().unwrap() = config.profiles;
        summary
    };

    app.state::<AppState>().persist()?;
    broadcast_change(&app);
    Ok(summary)
}

#[tauri::command]
fn list_profiles(state: State<'_, AppState>) -> Vec<Profile> {
    state.profiles.lock().unwrap().clone()
}

#[tauri::command]
fn list_connections(state: State<'_, AppState>) -> Vec<Connection> {
    state.connections.lock().unwrap().clone()
}

#[tauri::command]
fn list_statuses(state: State<'_, AppState>) -> Vec<SyncStatus> {
    state.statuses.lock().unwrap().values().cloned().collect()
}

#[tauri::command]
fn rsync_info(state: State<'_, AppState>) -> Option<RsyncInfo> {
    state.rsync_bin.lock().unwrap().clone()
}

/// Create or update a connection. Profiles resolve connections per sync, so an
/// edit here reaches every profile using it without restarting their watchers.
#[tauri::command]
fn save_connection(app: AppHandle, mut incoming: Connection) -> Result<Connection, String> {
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
    app.state::<AppState>().persist()?;
    broadcast_change(&app);
    Ok(incoming)
}

/// Delete a connection and disable every profile that referenced it. Those
/// profiles keep their settings and show an error until a new connection is
/// picked. Returns their names so the UI can say what stopped.
#[tauri::command]
fn delete_connection(app: AppHandle, id: String) -> Result<Vec<String>, String> {
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
        engine::set_status(
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
async fn test_connection(candidate: Connection) -> Result<String, String> {
    connection::test(&candidate).await
}

/// Create or update a profile. The watcher is restarted from the saved values
/// so an edit to source, excludes or debounce takes effect immediately.
#[tauri::command]
fn save_profile(app: AppHandle, mut incoming: Profile) -> Result<Profile, String> {
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
        engine::set_status(&app, &incoming.id, "disabled", None);
    }
    broadcast_change(&app);
    Ok(incoming)
}

#[tauri::command]
fn delete_profile(app: AppHandle, id: String) -> Result<(), String> {
    engine::stop(&app, &id);
    {
        let state = app.state::<AppState>();
        state.profiles.lock().unwrap().retain(|p| p.id != id);
        state.statuses.lock().unwrap().remove(&id);
    }
    app.state::<AppState>().persist()?;
    broadcast_change(&app);
    Ok(())
}

#[tauri::command]
fn set_profile_enabled(app: AppHandle, id: String, enabled: bool) -> Result<(), String> {
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
    app.state::<AppState>().persist()?;
    broadcast_change(&app);

    if enabled {
        engine::start(&app, &profile)
    } else {
        engine::stop(&app, &id);
        engine::set_status(&app, &id, "disabled", None);
        Ok(())
    }
}

/// Queue an immediate sync. A running profile gets it on its existing loop; a
/// disabled one gets a detached one-off pass so "Sync now" still works.
#[tauri::command]
fn sync_now(app: AppHandle, id: String) -> Result<(), String> {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .menu(menu::build)
        .on_menu_event(|app, event| {
            if event.id() == menu::SETTINGS_MENU_ID {
                let _ = menu::open_settings(app);
            } else if event.id() == menu::ABOUT_MENU_ID {
                let _ = menu::open_about(app);
            } else if event.id() == menu::QUIT_MENU_ID {
                menu::request_quit(app);
            }
        })
        .on_window_event(|window, event| {
            // Closing the main window hides it instead of quitting: the point
            // of the tray icon is that watchers keep running with no window
            // open. Quit explicitly from the tray or the app menu.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" && !menu::is_quitting() {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            std::fs::create_dir_all(&dir)?;

            tray::build(app.handle())?;

            // Carried over from before the rename to SyncPoint, which changed
            // the bundle identifier this directory is named after. Must run
            // before load(), or the app starts on an empty config and the old
            // profiles simply never appear.
            if let Some(parent) = dir.parent() {
                let legacy = parent.join("com.volodiafernandez.livesync-next");
                if store::adopt_legacy_dir(&dir, &legacy) {
                    eprintln!("[syncpoint] adopted configuration from {}", legacy.display());
                }
            }

            let config = store::load(&dir);
            let profiles = config.profiles.clone();
            app.manage(AppState::new(dir, config));

            // Profiles left enabled resume watching on launch.
            let handle = app.handle().clone();
            for profile in profiles.iter().filter(|p| p.enabled) {
                if let Err(err) = engine::start(&handle, profile) {
                    engine::set_status(&handle, &profile.id, "error", Some(err));
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_settings,
            take_settings_section,
            app_info,
            get_settings,
            save_settings,
            export_config,
            import_config,
            list_profiles,
            list_connections,
            list_statuses,
            rsync_info,
            save_connection,
            delete_connection,
            test_connection,
            save_profile,
            delete_profile,
            set_profile_enabled,
            sync_now
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {
            // Clicking the Dock icon with the window hidden should bring it
            // back, which is the only way in besides the tray.
            #[cfg(target_os = "macos")]
            if matches!(_event, tauri::RunEvent::Reopen { .. }) {
                menu::show_main(_app);
            }
        });
}
