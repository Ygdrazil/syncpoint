//! Application wiring: plugins, menu, tray, windows and startup.

mod commands;
mod connection;
mod engine;
mod menu;
mod profile;
mod rsync;
mod settings;
mod state;
mod store;
mod transfer;
mod tray;
mod version;

use state::AppState;
use tauri::Manager;

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
                    state::set_status(&handle, &profile.id, "error", Some(err));
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_settings,
            commands::take_settings_section,
            commands::app_info,
            commands::get_settings,
            commands::save_settings,
            commands::export_config,
            commands::import_config,
            commands::list_profiles,
            commands::list_connections,
            commands::list_statuses,
            commands::rsync_info,
            commands::save_connection,
            commands::delete_connection,
            commands::test_connection,
            commands::save_profile,
            commands::delete_profile,
            commands::set_profile_enabled,
            commands::sync_now
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
