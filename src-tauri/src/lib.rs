mod commands;
mod naming;
mod state;
mod tray;

use std::sync::Arc;

use state::AppState;
use tauri::{Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("agent-portal"));
            let state = Arc::new(AppState::new(data_dir));
            app.manage(state.clone());
            naming::start(state, app.handle().clone());
            tray::init(app.handle())?;

            // Closing the main window hides it to the tray instead of quitting;
            // only the tray's Quit exits.
            if let Some(main) = app.get_webview_window(tray::MAIN) {
                let handle = main.clone();
                main.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = handle.hide();
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::health,
            commands::detect_agents,
            commands::get_cached_board,
            commands::refresh_board,
            commands::get_session_preview,
            commands::check_ollama,
            commands::get_settings,
            commands::save_settings,
            commands::plan_migration,
            commands::execute_migration,
            commands::undo_migration,
            commands::launch_session,
            commands::launch_command,
            commands::list_activity,
            commands::naming_status,
            commands::show_main_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agent Portal");
}
