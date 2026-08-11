use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

pub const SETTINGS_WINDOW: &str = "settings";
pub const ABOUT_WINDOW: &str = "about";
pub const SETTINGS_MENU_ID: &str = "settings";
pub const ABOUT_MENU_ID: &str = "about";
pub const QUIT_MENU_ID: &str = "quit";

static QUITTING: AtomicBool = AtomicBool::new(false);

/// Quit for real.
///
/// Closing the main window only hides it, so that watchers keep running and the
/// tray icon stays put. That means the app needs an explicit way out, and the
/// close handler needs to tell a real quit from a window being dismissed —
/// which is what this flag is for. The predefined Quit item cannot be used:
/// it would trip the same close handler and be swallowed.
pub fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    QUITTING.store(true, Ordering::SeqCst);
    app.exit(0);
}

pub fn is_quitting() -> bool {
    QUITTING.load(Ordering::SeqCst)
}

/// Bring the main window back from hidden.
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn build<R: Runtime>(handle: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    // A custom About item rather than PredefinedMenuItem::about, so the window
    // can also report the detected rsync binary and the config file path —
    // the two things worth having in a bug report.
    let about = MenuItem::with_id(handle, ABOUT_MENU_ID, "About SyncPoint", true, None::<&str>)?;
    let settings = MenuItem::with_id(
        handle,
        SETTINGS_MENU_ID,
        "Settings…",
        true,
        Some("CmdOrCtrl+,"),
    )?;
    // Custom rather than PredefinedMenuItem::quit: quitting has to set the
    // flag that lets the close handler distinguish it from hiding the window.
    let quit = MenuItem::with_id(
        handle,
        QUIT_MENU_ID,
        "Quit SyncPoint",
        true,
        Some("CmdOrCtrl+Q"),
    )?;

    // macOS puts About and Settings in the application menu, not File.
    #[cfg(target_os = "macos")]
    let first = Submenu::with_items(
        handle,
        "SyncPoint",
        true,
        &[
            &about,
            &PredefinedMenuItem::separator(handle)?,
            &settings,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::services(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::hide(handle, None)?,
            &PredefinedMenuItem::hide_others(handle, None)?,
            &PredefinedMenuItem::show_all(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &quit,
        ],
    )?;

    #[cfg(not(target_os = "macos"))]
    let first = Submenu::with_items(
        handle,
        "File",
        true,
        &[
            &settings,
            &PredefinedMenuItem::separator(handle)?,
            &about,
            &PredefinedMenuItem::separator(handle)?,
            &quit,
        ],
    )?;

    // Without a native Edit menu, macOS gives the webview no Cmd+C/V/X/A in
    // text fields — the shortcuts simply do nothing.
    let edit = Submenu::with_items(
        handle,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(handle, None)?,
            &PredefinedMenuItem::redo(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::cut(handle, None)?,
            &PredefinedMenuItem::copy(handle, None)?,
            &PredefinedMenuItem::paste(handle, None)?,
            &PredefinedMenuItem::select_all(handle, None)?,
        ],
    )?;

    let window = Submenu::with_items(
        handle,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(handle, None)?,
            &PredefinedMenuItem::close_window(handle, None)?,
        ],
    )?;

    Menu::with_items(handle, &[&first, &edit, &window])
}

pub fn open_settings<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    focus_or_create(app, SETTINGS_WINDOW, "Settings", 760.0, 600.0, true)
}

pub fn open_about<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    focus_or_create(app, ABOUT_WINDOW, "About SyncPoint", 420.0, 460.0, false)
}

/// Focus an auxiliary window, creating it the first time.
///
/// Every window loads the same SPA at `/` and tells itself apart by label,
/// which keeps dev and bundled builds identical — no route that has to survive
/// static export, where only `index.html` is emitted.
fn focus_or_create<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
    title: &str,
    width: f64,
    height: f64,
    resizable: bool,
) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }

    WebviewWindowBuilder::new(app, label, WebviewUrl::App("/".into()))
        .title(title)
        .inner_size(width, height)
        .resizable(resizable)
        .build()?;
    Ok(())
}
