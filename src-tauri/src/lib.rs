//! Application wiring: plugins, menu, tray, windows and startup.

mod commands;
mod connection;
mod engine;
mod folder;
mod menu;
mod profile;
mod rsync;
mod settings;
mod state;
mod store;
mod template;
mod transfer;
mod tray;
mod version;
mod whats_new;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_deep_link::init())
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
            // syncpoint://new-profile?source=/path — used by the Finder Quick
            // Action. Handled here rather than in the frontend so the window is
            // raised even when the app was sitting in the tray.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        commands::handle_deep_link(&handle, &url);
                    }
                });
            }

            let dir = app.path().app_config_dir()?;
            std::fs::create_dir_all(&dir)?;

            tray::build(app.handle())?;

            let config = store::load(&dir);
            let profiles = config.profiles.clone();
            let paused = config.settings.paused;
            app.manage(AppState::new(dir, config));

            // Profiles left enabled resume watching on launch, unless the app
            // was suspended when it last quit.
            let handle = app.handle().clone();
            let enabled: Vec<_> = profiles.into_iter().filter(|p| p.enabled).collect();
            if paused {
                for profile in &enabled {
                    state::set_status(&handle, &profile.id, "paused", None);
                }
            } else {
                commands::start_all(&handle, &enabled);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_settings,
            commands::set_compact,
            commands::take_settings_section,
            commands::app_info,
            commands::get_settings,
            commands::save_settings,
            commands::export_config,
            commands::import_config,
            commands::list_folders,
            commands::save_folder,
            commands::delete_folder,
            commands::move_item,
            commands::list_profiles,
            commands::list_connections,
            commands::list_statuses,
            commands::rsync_info,
            commands::save_connection,
            commands::delete_connection,
            commands::test_connection,
            commands::whats_new_on_launch,
            commands::dismiss_whats_new,
            commands::render_profile_paths,
            commands::preview_path_templates,
            commands::save_profile,
            commands::delete_profile,
            commands::set_profile_enabled,
            commands::set_paused,
            commands::sync_now,
            commands::sync_all,
            commands::open_about,
            commands::take_auto_update
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
