mod commands;
mod state;

use std::sync::Arc;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("agent-portal"));
            app.manage(Arc::new(AppState::new(data_dir)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::health,
            commands::detect_agents,
            commands::get_board,
            commands::get_session_preview,
            commands::check_ollama,
            commands::plan_migration,
            commands::execute_migration,
            commands::undo_migration,
            commands::launch_session,
            commands::launch_command,
            commands::list_activity
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agent Portal");
}
