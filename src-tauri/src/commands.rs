use std::collections::HashMap;
use std::sync::Arc;

use portal_core::adapter::SessionLocator;
use portal_core::dto::{
    AgentDescriptor, BoardSnapshot, Health, NamingCounts, NamingEntry, NamingReport,
};
use portal_core::error::PortalError;
use portal_core::index::session_revision;
use portal_core::ir::CanonicalSession;
use portal_core::launch;
use portal_core::migration::engine::{self, BriefConfig};
use portal_core::migration::ledger::LedgerEntry;
use portal_core::migration::ollama::{self, OllamaStatus};
use portal_core::migration::types::{
    CommandSpec, DryRunReport, MigrationKind, MigrationResult, UndoReport,
};
use portal_core::settings::AppSettings;

use crate::state::AppState;

#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle) {
    crate::tray::show_main(&app);
}

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
        let mut board = s.registry.board(&s.env);
        if let Err(e) = s.index.apply_generated_titles(&mut board) {
            eprintln!("failed to apply generated titles: {e}");
        }
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
pub async fn get_settings(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AppSettings, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || Ok(s.settings.load())).await
}

#[tauri::command]
pub async fn save_settings(
    state: tauri::State<'_, Arc<AppState>>,
    settings: AppSettings,
) -> Result<AppSettings, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        s.settings.save(&settings)?;
        Ok(s.settings.load())
    })
    .await
}

#[tauri::command]
pub async fn check_ollama(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<OllamaStatus, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let cfg = s.settings.load();
        let mut status = ollama::status(&cfg.ollama_host);
        status.default_model = cfg.ollama_model.clone();
        status.default_present = status.models.iter().any(|model| model == &cfg.ollama_model);
        Ok(status)
    })
    .await
}

#[tauri::command]
pub async fn pull_ollama_model(
    state: tauri::State<'_, Arc<AppState>>,
    model: String,
) -> Result<OllamaStatus, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let cfg = s.settings.load();
        let model = model.trim();
        if model.is_empty() {
            return Err(PortalError::Other("Model name is required".into()));
        }
        ollama::pull(&cfg.ollama_host, model).map_err(PortalError::Other)?;
        let mut status = ollama::status(&cfg.ollama_host);
        status.default_model = cfg.ollama_model.clone();
        status.default_present = status
            .models
            .iter()
            .any(|candidate| candidate == &cfg.ollama_model);
        Ok(status)
    })
    .await
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
            "compacted_native" => MigrationKind::CompactedNative,
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

        let settings = s.settings.load();
        let brief_cfg = BriefConfig {
            enhance: enhance.unwrap_or(false),
            base_url: settings.ollama_host,
            model: settings.ollama_model,
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
        let shell = s.settings.load().launch_shell;
        launch::launch_in_terminal(&spec, shell)
    })
    .await
}

/// Open an installed agent interactively in a project folder (no session resume,
/// no seed prompt). Powers the board "Open with" action.
#[tauri::command]
pub async fn launch_agent_on_project(
    state: tauri::State<'_, Arc<AppState>>,
    agent_id: String,
    cwd: String,
) -> Result<(), PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let cwd = cwd.trim();
        if cwd.is_empty() {
            return Err(PortalError::Other(
                "project folder path is unknown — cannot open an agent here".into(),
            ));
        }
        let adapter = s
            .adapter(&agent_id)
            .ok_or_else(|| PortalError::Other(format!("unknown agent '{agent_id}'")))?;
        if !adapter.capabilities().launch_new {
            return Err(PortalError::Other(format!(
                "agent '{agent_id}' cannot launch a new session"
            )));
        }
        let inst = s
            .installation(&agent_id)
            .ok_or_else(|| PortalError::Other(format!("agent '{agent_id}' not detected")))?;
        let spec = adapter.open_project_command(&inst, cwd)?;
        let shell = s.settings.load().launch_shell;
        launch::launch_in_terminal(&spec, shell)
    })
    .await
}

/// Run an already-built command (the resume/launch command a migration
/// returned) in a terminal. Works for both native resume and brief launch.
#[tauri::command]
pub async fn launch_command(
    state: tauri::State<'_, Arc<AppState>>,
    spec: CommandSpec,
) -> Result<(), PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let shell = s.settings.load().launch_shell;
        launch::launch_in_terminal(&spec, shell)
    })
    .await
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

/// Snapshot of the background session-naming worker for the Activity view:
/// whether the local model is available, how many sessions are named / stale /
/// pending, and the generated titles themselves.
#[tauri::command]
pub async fn naming_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<NamingReport, PortalError> {
    let s = state.inner().clone();
    run_blocking(move || {
        let settings = s.settings.load();
        let status = ollama::status(&settings.ollama_host);
        let naming_model = settings.ollama_naming_model;
        let model_present = status.models.iter().any(|m| m == &naming_model);

        // The cached board is what the worker overlays titles onto; fall back to
        // a fresh scan only on a cold cache.
        let board = s
            .index
            .cached_board()
            .unwrap_or_else(|| s.registry.board(&s.env));

        let titles = s.index.all_generated_titles()?;
        let title_by_session: HashMap<(&str, &str), &portal_core::index::StoredTitle> = titles
            .iter()
            .map(|t| ((t.agent_id.as_str(), t.native_id.as_str()), t))
            .collect();

        // Split counts into the recency window (the worker's active queue) and
        // the whole library (context). The lifetime "pending" is huge and not a
        // real backlog the worker chases, so it must not read as one.
        let cutoff =
            chrono::Utc::now() - chrono::Duration::hours(crate::naming::RECENT_WINDOW_HOURS);
        let mut recent = NamingCounts::default();
        let mut overall = NamingCounts::default();
        // Per session still on the board: its revision and project label.
        let mut info: HashMap<(&str, &str), (String, &str)> = HashMap::new();
        for (project, session) in board
            .lanes
            .iter()
            .flat_map(|l| l.projects.iter())
            .flat_map(|p| p.sessions.iter().map(move |s| (p.label.as_str(), s)))
        {
            let revision = session_revision(session);
            let key = (session.agent_id.as_str(), session.native_id.as_str());
            let is_recent = session.updated_at.is_some_and(|t| t >= cutoff);
            overall.total += 1;
            if is_recent {
                recent.total += 1;
            }
            let bump = |c: &mut NamingCounts| match title_by_session.get(&key) {
                None => c.pending += 1,
                Some(t) if t.source_revision == revision => c.named += 1,
                Some(_) => c.stale += 1,
            };
            bump(&mut overall);
            if is_recent {
                bump(&mut recent);
            }
            info.insert(key, (revision, project));
        }

        // Only surface titles for sessions still on the board; a title for a
        // deleted session is noise here.
        let mut entries: Vec<NamingEntry> = titles
            .iter()
            .filter_map(|t| {
                let (revision, project) = info.get(&(t.agent_id.as_str(), t.native_id.as_str()))?;
                Some(NamingEntry {
                    agent_id: t.agent_id.clone(),
                    native_id: t.native_id.clone(),
                    project: project.to_string(),
                    title: t.title.clone(),
                    current: *revision == t.source_revision,
                    updated_at: chrono::DateTime::from_timestamp_millis(t.updated_at)
                        .unwrap_or_default(),
                })
            })
            .collect();
        entries.sort_by_key(|b| std::cmp::Reverse(b.updated_at));

        Ok(NamingReport {
            ollama_available: status.available,
            model: naming_model,
            model_present,
            window_hours: crate::naming::RECENT_WINDOW_HOURS as u32,
            recent,
            overall,
            progress: s.naming_progress(),
            entries,
        })
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
