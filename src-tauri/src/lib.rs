mod commands;
pub mod mcp;
mod naming;
pub mod search_index;
pub mod semantic_index;
pub mod state;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;

use state::AppState;
use tauri::{Manager, WindowEvent};

/// Dirs where the shipped WormDB binary may live (never PATH).
fn wormdb_search_dirs(app: &tauri::App) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // Packaged / tauri-dev resource root (externalBin lands next to exe or in Resources).
    if let Ok(resource) = app.path().resource_dir() {
        dirs.push(resource);
    }
    // Vendored binaries for `cargo run` / `tauri dev` (triple-suffixed + plain).
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries"));
    dirs
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be first so a second launch exits before setup runs workers / WormDB / MCP.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Focus the existing instance instead of starting another.
            tray::show_main(app);
        }))
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("agent-portal"));
            let state = Arc::new(AppState::with_wormdb_search_dirs(
                data_dir,
                &wormdb_search_dirs(app),
            ));
            app.manage(state.clone());
            naming::start(state.clone(), app.handle().clone());
            search_index::start(state.clone());
            semantic_index::start(state.clone());
            // In-process HTTP MCP (loopback) — primary surface for agents.
            state.apply_mcp_http();
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
            commands::launch_agent_on_project,
            commands::launch_command,
            commands::list_activity,
            commands::naming_status,
            commands::search_sessions,
            commands::search_index_status,
            commands::ensure_wormdb_sidecar,
            commands::mcp_http_status,
            commands::reset_semantic_index,
            commands::set_semantic_indexing_paused,
            commands::show_main_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agent Portal");
}
