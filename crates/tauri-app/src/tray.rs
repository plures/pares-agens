//! System tray setup for Pares Agens.
//!
//! Creates the tray icon with a context menu that lets users:
//! - Show / Hide the main window
//! - Open the Settings dialog
//! - Quit the application

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, Emitter, Manager,
};

/// Register the system tray icon and context menu with the Tauri application.
///
/// Menu items:
/// - **Show / Hide** — toggle main window visibility
/// - **Settings** — emit a `show-settings` event to the frontend
/// - _(separator)_
/// - **Quit** — exit the application
///
/// Left-clicking the icon toggles the window; right-clicking (or
/// left-clicking on macOS) opens the context menu.
pub fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let show_hide = MenuItem::with_id(app, "show_hide", "Show / Hide", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show_hide, &settings, &sep, &quit])?;

    TrayIconBuilder::new()
        .menu(&menu)
        // Keep left-click for the window-toggle handler below.
        // The context menu is still reachable via right-click on all platforms.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show_hide" => toggle_window(app),
            "settings" => open_settings_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click toggles window visibility; right-click opens the menu.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

/// Toggle the main window's visibility.
fn toggle_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            show_and_focus_main_window(app, true);
        }
    }
}

/// Show, restore and focus the main window.
///
/// When `focus_input` is true, also emits `focus-chat-input` so the frontend
/// can place keyboard focus in the chat textbox.
pub fn show_and_focus_main_window(app: &tauri::AppHandle, focus_input: bool) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        let _ = window.show();
        let _ = window.set_focus();
        if focus_input {
            let _ = window.emit("focus-chat-input", ());
        }
    }
}

/// Show the main window and emit `show-settings` so the frontend can open
/// the Settings dialog immediately.
fn open_settings_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        show_and_focus_main_window(app, false);
        let _ = window.emit("show-settings", ());
    }
}
