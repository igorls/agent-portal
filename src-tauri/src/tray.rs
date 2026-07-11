//! System tray + the quick-peek popup window.
//!
//! The main window hides to the tray instead of quitting. The tray icon:
//!   - left click toggles the peek popup (a small always-on-top glance window),
//!   - right click opens a menu (Open / Quit).

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const PEEK: &str = "peek";
pub const MAIN: &str = "main";

/// Build the hidden peek window and the tray icon. Call from `setup`.
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    build_peek_window(app)?;

    let open = MenuItem::with_id(app, "open", "Open Agent Portal", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let icon = app
        .default_window_icon()
        .cloned()
        .expect("app has a default window icon");

    TrayIconBuilder::with_id("agent-portal-tray")
        .icon(icon)
        .tooltip("Agent Portal")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                toggle_peek(tray.app_handle(), position);
            }
        })
        .build(app)?;

    Ok(())
}

fn build_peek_window(app: &AppHandle) -> tauri::Result<()> {
    let peek = WebviewWindowBuilder::new(app, PEEK, WebviewUrl::App("index.html".into()))
        .title("Agent Portal — Quick Peek")
        .inner_size(360.0, 480.0)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(true)
        .visible(false)
        .build()?;

    // Dismiss the popup when it loses focus, like a native flyout.
    let handle = peek.clone();
    peek.on_window_event(move |event| {
        if let WindowEvent::Focused(false) = event {
            let _ = handle.hide();
        }
    });
    Ok(())
}

/// Restore and focus the main board window; hide the peek popup.
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    if let Some(p) = app.get_webview_window(PEEK) {
        let _ = p.hide();
    }
}

fn toggle_peek(app: &AppHandle, click: PhysicalPosition<f64>) {
    let Some(peek) = app.get_webview_window(PEEK) else {
        return;
    };
    if peek.is_visible().unwrap_or(false) {
        let _ = peek.hide();
        return;
    }
    // Anchor above-and-left of the tray click so the popup sits over the tray.
    if let Ok(size) = peek.outer_size() {
        let x = (click.x - size.width as f64).max(8.0);
        let y = (click.y - size.height as f64 - 12.0).max(8.0);
        let _ = peek.set_position(PhysicalPosition::new(x, y));
    }
    let _ = peek.show();
    let _ = peek.set_focus();
}
