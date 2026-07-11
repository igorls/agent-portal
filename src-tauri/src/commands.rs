use std::sync::Arc;

use portal_core::adapter::SessionLocator;
use portal_core::dto::{AgentDescriptor, BoardSnapshot, Health};
use portal_core::error::PortalError;
use portal_core::ir::CanonicalSession;
use portal_core::launch;
use portal_core::migration::engine::{self, BriefConfig};
use portal_core::migration::ledger::LedgerEntry;
use portal_core::migration::ollama::{self, OllamaStatus};
use portal_core::migration::types::{
    CommandSpec, DryRunReport, MigrationKind, MigrationResult, UndoReport,
};

use crate::state::AppState;

#[tauri::command]
pub fn health(app: tauri::AppHandle) -> Result<Health, PortalError> {
    Ok(Health {
        app_version: app.package_info().version.to_string(),
        adapters_registered: portal_adapters::builtin_adapter_count(),
    })
}

#[tauri::command]
pub async fn detect_agents(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<AgentDescriptor>, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || Ok(s.registry.descriptors(&s.env))).await
}

/// Instant: the last cached board (None on a cold start). The UI shows this
/// immediately, then calls `refresh_board`.
#[tauri::command]
pub async fn get_cached_board(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Option<BoardSnapshot>, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || Ok(s.index.cached_board())).await
}

/// Full scan of every store: rebuilds the board, refreshes the install cache,
/// and updates the on-disk cache for next launch.
#[tauri::command]
pub async fn refresh_board(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<BoardSnapshot, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let board = s.registry.board(&s.env);
        s.cache_installations(board.lanes.iter().filter_map(|lane| {
            lane.agent
                .installation
                .clone()
                .map(|inst| (lane.agent.id.clone(), inst))
        }));
        if let Err(e) = s.index.store_board(&board) {
            eprintln!("failed to cache board: {e}");
        }
        Ok(board)
    })
    .await
}

#[tauri::command]
pub async fn get_session_preview(
    state: tauri::State<'_, Arc<AppState>>,
    agent_id: String,
    native_id: String,
    store_path: Option<String>,
) -> Result<CanonicalSession, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let adapter = s
            .adapter(&agent_id)
            .ok_or_else(|| PortalError::Other(format!("unknown agent '{agent_id}'")))?;
        let inst = s
            .installation(&agent_id)
            .ok_or_else(|| PortalError::Other(format!("agent '{agent_id}' not detected")))?;
        let session = adapter.read_session(
            &inst,
            &SessionLocator {
                native_id,
                store_path: store_path.map(Into::into),
            },
        )?;
        // Raw passthrough payloads stay on the Rust side.
        Ok(session.without_raw())
    })
    .await
}

#[tauri::command]
pub async fn check_ollama() -> Result<OllamaStatus, PortalError> {
    run_blocking(move || Ok(ollama::status(ollama::DEFAULT_BASE_URL))).await
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn plan_migration(
    state: tauri::State<'_, Arc<AppState>>,
    source_agent: String,
    source_native_id: String,
    source_store_path: Option<String>,
    target_agent: String,
    mode: String,
    enhance: Option<bool>,
) -> Result<DryRunReport, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let kind = match mode.as_str() {
            "brief" => MigrationKind::Brief,
            _ => MigrationKind::Native,
        };
        let source_adapter = s
            .adapter(&source_agent)
            .ok_or_else(|| PortalError::Other(format!("unknown agent '{source_agent}'")))?;
        let target_adapter = s
            .adapter(&target_agent)
            .ok_or_else(|| PortalError::Other(format!("unknown agent '{target_agent}'")))?;
        let source_inst = s
            .installation(&source_agent)
            .ok_or_else(|| PortalError::Other(format!("agent '{source_agent}' not detected")))?;
        let target_inst = s
            .installation(&target_agent)
            .ok_or_else(|| PortalError::Other(format!("agent '{target_agent}' not detected")))?;

        let brief_cfg = BriefConfig {
            enhance: enhance.unwrap_or(false),
            ..BriefConfig::default()
        };

        let planned = engine::plan(
            &source_adapter,
            &source_inst,
            &target_adapter,
            &target_inst,
            &SessionLocator {
                native_id: source_native_id,
                store_path: source_store_path.map(Into::into),
            },
            kind,
            &brief_cfg,
        )?;
        let report = planned.report.clone();
        s.store_plan(planned);
        Ok(report)
    })
    .await
}

#[tauri::command]
pub async fn execute_migration(
    state: tauri::State<'_, Arc<AppState>>,
    plan_id: String,
) -> Result<MigrationResult, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let planned = s
            .take_plan(&plan_id)
            .ok_or_else(|| PortalError::Other("plan expired; re-run the dry run".to_string()))?;
        let target_adapter = s
            .adapter(&planned.target_agent)
            .ok_or_else(|| PortalError::Other("target agent missing".to_string()))?;
        let target_inst = s
            .installation(&planned.target_agent)
            .ok_or_else(|| PortalError::Other("target agent not detected".to_string()))?;
        engine::execute(&planned, &target_adapter, &target_inst, &s.ledger)
    })
    .await
}

#[tauri::command]
pub async fn launch_session(
    state: tauri::State<'_, Arc<AppState>>,
    agent_id: String,
    native_id: String,
    cwd: String,
) -> Result<(), PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let adapter = s
            .adapter(&agent_id)
            .ok_or_else(|| PortalError::Other(format!("unknown agent '{agent_id}'")))?;
        let inst = s
            .installation(&agent_id)
            .ok_or_else(|| PortalError::Other(format!("agent '{agent_id}' not detected")))?;
        let spec = adapter.resume_command(&inst, &native_id, &cwd)?;
        launch::launch_in_terminal(&spec)
    })
    .await
}

/// Run an already-built command (the resume/launch command a migration
/// returned) in a terminal. Works for both native resume and brief launch.
#[tauri::command]
pub async fn launch_command(spec: CommandSpec) -> Result<(), PortalError> {
    run_blocking(move || launch::launch_in_terminal(&spec)).await
}

#[tauri::command]
pub async fn undo_migration(
    state: tauri::State<'_, Arc<AppState>>,
    migration_id: String,
    force: Option<bool>,
) -> Result<UndoReport, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let entry = s.ledger.get(&migration_id)?.ok_or_else(|| {
            PortalError::Other("migration not found in the activity log".to_string())
        })?;
        engine::undo(&entry, &s.ledger, force.unwrap_or(false))
    })
    .await
}

#[tauri::command]
pub async fn list_activity(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<LedgerEntry>, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let mut entries = s.ledger.list()?;
        entries.reverse(); // newest first
        Ok(entries)
    })
    .await
}

async fn run_blocking<T, F>(f: F) -> Result<T, PortalError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PortalError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| PortalError::Other(format!("task join error: {e}")))?
}
