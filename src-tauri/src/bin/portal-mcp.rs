//! Headless Agent Portal MCP server (stdio).
//!
//! ```text
//! portal-mcp.exe
//! # or
//! cargo run -p portal-tauri --bin portal-mcp
//! ```
//!
//! Configure in Claude/Cursor/Grok MCP settings:
//! ```json
//! {
//!   "mcpServers": {
//!     "agent-portal": {
//!       "command": "P:/agent-portal/target/debug/portal-mcp.exe"
//!     }
//!   }
//! }
//! ```
//!
//! Logs go to stderr. Stdout is reserved for MCP framing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use portal_tauri_lib::mcp;
use portal_tauri_lib::search_index;
use portal_tauri_lib::state::AppState;

fn main() {
    // Never use the GUI windows_subsystem path — MCP needs a console for stdio.
    let data_dir = resolve_data_dir();
    eprintln!(
        "agent-portal mcp {} — data dir {}",
        env!("CARGO_PKG_VERSION"),
        data_dir.display()
    );

    // Same bundled WormDB search as the GUI (never PATH).
    let wormdb_dirs = [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries")];
    let state = Arc::new(AppState::with_wormdb_search_dirs(data_dir, &wormdb_dirs));

    // Background FTS indexer + optional WormDB (same as GUI).
    search_index::start(state.clone());

    // Periodic board refresh so long-lived MCP sessions stay current.
    {
        let s = state.clone();
        std::thread::Builder::new()
            .name("mcp-board-refresh".into())
            .spawn(move || loop {
                if let Err(e) = refresh(&s) {
                    eprintln!("mcp board refresh: {e}");
                }
                std::thread::sleep(Duration::from_secs(60));
            })
            .ok();
    }

    if let Err(e) = mcp::run_stdio(state) {
        eprintln!("mcp server exited: {e}");
        std::process::exit(1);
    }
}

fn refresh(state: &AppState) -> Result<(), String> {
    let mut board = state.registry.board(&state.env);
    let _ = state.index.apply_generated_titles(&mut board);
    state.cache_installations(board.lanes.iter().filter_map(|lane| {
        lane.agent
            .installation
            .clone()
            .map(|inst| (lane.agent.id.clone(), inst))
    }));
    let _ = state.index.store_board(&board);
    state.publish_board(board);
    Ok(())
}

fn resolve_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_PORTAL_DATA_DIR") {
        return PathBuf::from(p);
    }
    // Match Tauri app_data_dir for identifier com.agentportal.desktop
    // Windows: %APPDATA%\com.agentportal.desktop
    // macOS: ~/Library/Application Support/com.agentportal.desktop
    // Linux: ~/.local/share/com.agentportal.desktop
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("com.agentportal.desktop");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("com.agentportal.desktop");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("com.agentportal.desktop");
        }
    }
    std::env::temp_dir().join("agent-portal")
}
