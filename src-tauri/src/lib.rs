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
                // `tauri dev` runs a bare executable on macOS and does not
                // consistently apply the platform override that creates the
                // native title-bar controls. Reinforce it at runtime so the
                // live development window matches the packaged app.
                #[cfg(target_os = "macos")]
                {
                    main.set_decorations(true)?;
                    main.set_title_bar_style(tauri::TitleBarStyle::Overlay)?;
                    #[cfg(debug_assertions)]
                    main.set_title("")?;
                }

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
            commands::pull_ollama_model,
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
