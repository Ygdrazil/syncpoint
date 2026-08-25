use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

pub const TRAY_ID: &str = "syncpoint";

const SHOW_ID: &str = "tray-show";
const SETTINGS_ID: &str = "tray-settings";
const SYNC_ALL_ID: &str = "tray-sync-all";
const QUIT_ID: &str = "tray-quit";

// Template images: shape lives in the alpha channel and macOS recolours them to
// match the menu bar, so they stay legible in both light and dark.
const ICON_IDLE: &[u8] = include_bytes!("../icons/tray-idle.png");
const ICON_SYNCING: &[u8] = include_bytes!("../icons/tray-syncing.png");

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, SHOW_ID, "Open SyncPoint", true, None::<&str>)?;
    let sync_all = MenuItem::with_id(app, SYNC_ALL_ID, "Sync all now", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, SETTINGS_ID, "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit SyncPoint", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &sync_all,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(ICON_IDLE)?)
        .icon_as_template(true)
        .tooltip("SyncPoint — idle")
        .menu(&menu)
        // Left click raises the window; the menu stays on right click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            SHOW_ID => crate::menu::show_main(app),
            SYNC_ALL_ID => {
                let _ = crate::commands::sync_all(app.clone());
            }
            SETTINGS_ID => {
                let _ = crate::menu::open_settings(app);
            }
            QUIT_ID => crate::menu::request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::menu::show_main(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

/// Reflect how many profiles are mid-sync. The icon only has two states, but
/// the tooltip carries the count.
///
/// Dispatched to the main thread because `tray_by_id` returns an `Rc` clone of
/// the tray icon, and dropping that clone off the main thread calls
/// `TrayIcon::remove` → `NSStatusBar._removeStatusItem:` on the wrong dispatch
/// queue, tripping `assertBarrierOnQueue` (EXC_BREAKPOINT). Every caller — the
/// sync loop, status changes, startup — may be on a tokio worker, so the
/// dispatch belongs here, not at each call site.
pub fn set_syncing(app: &AppHandle, syncing: usize) {
    let app = app.clone();
    // The clone gives run_on_main_thread its &self receiver; the owned `app`
    // moves into the closure and is dropped on the main thread.
    let _ = app.clone().run_on_main_thread(move || {
        let Some(tray) = app.tray_by_id(TRAY_ID) else {
            return;
        };

        let bytes = if syncing > 0 { ICON_SYNCING } else { ICON_IDLE };
        if let Ok(icon) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
            // set_icon resets the template flag on macOS, so re-assert it or the
            // icon turns into a solid black blob in dark menu bars.
            let _ = tray.set_icon_as_template(true);
        }

        let _ = tray.set_tooltip(Some(tooltip(syncing)));
    });
}

fn tooltip(syncing: usize) -> String {
    match syncing {
        0 => "SyncPoint — idle".to_string(),
        1 => "SyncPoint — syncing 1 profile".to_string(),
        n => format!("SyncPoint — syncing {n} profiles"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_reads_naturally_at_every_count() {
        assert_eq!(tooltip(0), "SyncPoint — idle");
        assert_eq!(tooltip(1), "SyncPoint — syncing 1 profile");
        assert_eq!(tooltip(4), "SyncPoint — syncing 4 profiles");
    }

    #[test]
    fn both_icon_states_are_decodable_pngs() {
        // include_bytes! only proves the files exist; this proves Tauri can
        // actually turn them into images at runtime.
        assert!(Image::from_bytes(ICON_IDLE).is_ok());
        assert!(Image::from_bytes(ICON_SYNCING).is_ok());
    }

    #[test]
    fn the_two_states_look_different() {
        assert_ne!(ICON_IDLE, ICON_SYNCING);
    }
}
