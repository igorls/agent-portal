use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portal_core::adapter::{AgentAdapter, HostEnv};
use portal_core::dto::{BoardSnapshot, Installation, NamingProgress};
use portal_core::index::IndexStore;
use portal_core::migration::engine::PlannedMigration;
use portal_core::migration::ledger::Ledger;
use portal_core::registry::AgentRegistry;
use portal_core::search::{resolve_wormdb_binary, SearchStore, WormdbSidecar};
use portal_core::settings::SettingsStore;

use crate::mcp::{McpHttpRuntime, McpHttpServer};
use crate::semantic_index::SemanticProgress;

pub struct AppState {
    pub env: HostEnv,
    pub registry: AgentRegistry,
    pub ledger: Ledger,
    pub index: IndexStore,
    /// Lexical content index (`search.db`). None if open failed (search degrades to meta).
    pub search: Option<SearchStore>,
    /// Optional WormDB sidecar (HNSW semantic backend). Started on demand.
    pub wormdb: Option<WormdbSidecar>,
    pub settings: SettingsStore,
    /// In-process HTTP MCP (loopback). None when disabled / failed to bind.
    mcp_http: Mutex<Option<McpHttpServer>>,
    /// Detection results are cached: detect() shells out to `<cli> --version`
    /// which costs seconds for npm shims. Board refreshes repopulate this.
    installations: Mutex<HashMap<String, Installation>>,
    /// Dry-run plans awaiting execute, keyed by plan_id.
    plans: Mutex<HashMap<String, Arc<PlannedMigration>>>,
    /// Live state of the background naming worker, surfaced in the Activity view.
    naming: Mutex<NamingProgress>,
    plan_dir: PathBuf,
    /// Hot board for instant metadata search (no per-keystroke SQLite deserialize).
    board: Mutex<Option<Arc<BoardSnapshot>>>,
    /// Live semantic indexer progress (Settings / status probes).
    semantic: Mutex<SemanticProgress>,
    /// Set when UI requests an immediate reindex pass (cleared by worker).
    semantic_kick: AtomicBool,
    /// User-requested pause of the background semantic embedder (resume continues).
    semantic_paused: AtomicBool,
}

impl AppState {
    /// Build app state, resolving WormDB only from bundled paths / `PORTAL_WORMDB`.
    ///
    /// Pass Tauri resource dir and `src-tauri/binaries` via `wormdb_search_dirs`
    /// so packaged and `tauri dev` builds find the shipped binary without PATH.
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self::with_wormdb_search_dirs(app_data_dir, &[])
    }

    pub fn with_wormdb_search_dirs(app_data_dir: PathBuf, wormdb_search_dirs: &[PathBuf]) -> Self {
        let index = IndexStore::open(&app_data_dir).unwrap_or_else(|e| {
            // A broken cache must never take down the app; fall back to temp.
            eprintln!("index cache unavailable ({e}); using temp");
            IndexStore::open(&std::env::temp_dir().join("agent-portal")).expect("temp index")
        });
        let search = match SearchStore::open(&app_data_dir) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("search.db unavailable ({e}); content search disabled");
                None
            }
        };
        let wormdb_bin = resolve_wormdb_binary(None, wormdb_search_dirs);
        let wormdb = if wormdb_bin.is_file() {
            eprintln!("WormDB binary: {}", wormdb_bin.display());
            Some(WormdbSidecar::new(&app_data_dir, wormdb_bin, 0))
        } else {
            eprintln!(
                "WormDB binary not found (looked under bundled dirs → {}); semantic sidecar disabled. \
                 Vendor with scripts/vendor-wormdb.ps1 (or .sh) into src-tauri/binaries/",
                wormdb_bin.display()
            );
            None
        };
        Self {
            env: HostEnv::from_system(),
            registry: AgentRegistry::new(portal_adapters::builtin_adapters()),
            ledger: Ledger::new(app_data_dir.clone()),
            settings: SettingsStore::new(&app_data_dir),
            index,
            search,
            wormdb,
            mcp_http: Mutex::new(None),
            installations: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
            naming: Mutex::new(NamingProgress::default()),
            plan_dir: app_data_dir.join("migration-plans"),
            board: Mutex::new(None),
            semantic: Mutex::new(SemanticProgress::default()),
            semantic_kick: AtomicBool::new(false),
            semantic_paused: AtomicBool::new(false),
        }
    }

    pub fn semantic_progress(&self) -> SemanticProgress {
        let mut p = self.semantic.lock().unwrap().clone();
        p.paused = self.semantic_paused();
        p
    }

    pub fn set_semantic_progress(&self, f: impl FnOnce(&mut SemanticProgress)) {
        let mut g = self.semantic.lock().unwrap();
        f(&mut g);
        g.paused = self.semantic_paused.load(Ordering::SeqCst);
    }

    pub fn kick_semantic_indexer(&self) {
        self.semantic_kick.store(true, Ordering::SeqCst);
    }

    pub fn take_semantic_kick(&self) -> bool {
        self.semantic_kick.swap(false, Ordering::SeqCst)
    }

    pub fn semantic_paused(&self) -> bool {
        self.semantic_paused.load(Ordering::SeqCst)
    }

    /// Pause or resume background semantic embedding. Resume kicks the worker.
    pub fn set_semantic_paused(&self, paused: bool) {
        self.semantic_paused.store(paused, Ordering::SeqCst);
        self.set_semantic_progress(|p| {
            p.paused = paused;
            if paused {
                p.active = false;
                p.current = Some("paused".into());
            } else if p.current.as_deref() == Some("paused") {
                p.current = Some("resuming…".into());
            }
        });
        if !paused {
            self.kick_semantic_indexer();
        }
    }

    /// Start or restart HTTP MCP from current settings. No-op when disabled.
    pub fn apply_mcp_http(self: &Arc<Self>) {
        let cfg = self.settings.load();
        // Drop previous listener first.
        {
            let mut slot = self.mcp_http.lock().unwrap();
            if let Some(srv) = slot.take() {
                srv.stop();
            }
        }
        if !cfg.mcp_http_enabled {
            eprintln!("mcp-http: disabled in settings");
            return;
        }
        let token = {
            let t = cfg.mcp_http_token.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        };
        match crate::mcp::start_http(self.clone(), cfg.mcp_http_port, token) {
            Ok(srv) => {
                let rt = srv.runtime_snapshot();
                eprintln!(
                    "mcp-http: ready at http://127.0.0.1:{}/mcp (auth={})",
                    rt.port.unwrap_or(0),
                    if rt.token_required { "bearer" } else { "none" }
                );
                *self.mcp_http.lock().unwrap() = Some(srv);
            }
            Err(e) => eprintln!("mcp-http: failed to start: {e}"),
        }
    }

    pub fn mcp_http_runtime(&self) -> McpHttpRuntime {
        self.mcp_http
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.runtime_snapshot())
            .unwrap_or_default()
    }

    /// Start WormDB sidecar if configured; returns bound port.
    pub fn ensure_wormdb(&self) -> portal_core::error::Result<u16> {
        let sc = self.wormdb.as_ref().ok_or_else(|| {
            portal_core::error::PortalError::Other(
                "WormDB binary not found at startup (shipped under src-tauri/binaries; \
                 override with PORTAL_WORMDB path if needed)"
                    .into(),
            )
        })?;
        sc.ensure_running()
    }

    /// Publish the latest board into the hot in-process cache (search + future indexers).
    pub fn publish_board(&self, board: BoardSnapshot) {
        *self.board.lock().unwrap() = Some(Arc::new(board));
    }

    /// Hot board snapshot for metadata search. Falls back to disk cache once.
    pub fn hot_board(&self) -> Option<Arc<BoardSnapshot>> {
        if let Some(b) = self.board.lock().unwrap().clone() {
            return Some(b);
        }
        let cached = self.index.cached_board()?;
        let arc = Arc::new(cached);
        *self.board.lock().unwrap() = Some(arc.clone());
        Some(arc)
    }

    /// Snapshot the worker's live progress.
    pub fn naming_progress(&self) -> NamingProgress {
        self.naming.lock().unwrap().clone()
    }

    /// Update the worker's live progress and return the new snapshot.
    pub fn update_naming_progress(&self, f: impl FnOnce(&mut NamingProgress)) -> NamingProgress {
        let mut guard = self.naming.lock().unwrap();
        f(&mut guard);
        guard.clone()
    }

    pub fn adapter(&self, agent_id: &str) -> Option<Arc<dyn AgentAdapter>> {
        self.registry
            .adapters()
            .iter()
            .find(|a| a.id() == agent_id)
            .cloned()
    }

    pub fn installation(&self, agent_id: &str) -> Option<Installation> {
        if let Some(inst) = self.installations.lock().unwrap().get(agent_id) {
            return Some(inst.clone());
        }
        let adapter = self.adapter(agent_id)?;
        let inst = adapter.detect(&self.env)?;
        self.installations
            .lock()
            .unwrap()
            .insert(agent_id.to_string(), inst.clone());
        Some(inst)
    }

    pub fn cache_installations(&self, pairs: impl IntoIterator<Item = (String, Installation)>) {
        let mut cache = self.installations.lock().unwrap();
        for (id, inst) in pairs {
            cache.insert(id, inst);
        }
    }

    pub fn store_plan(
        &self,
        plan: PlannedMigration,
    ) -> portal_core::error::Result<Arc<PlannedMigration>> {
        let arc = Arc::new(plan);
        self.plans
            .lock()
            .unwrap()
            .insert(arc.report.plan_id.clone(), arc.clone());
        Ok(arc)
    }

    pub fn take_plan(
        &self,
        plan_id: &str,
    ) -> portal_core::error::Result<Option<Arc<PlannedMigration>>> {
        if plan_id.is_empty()
            || plan_id.len() > 64
            || !plan_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        {
            return Ok(None);
        }
        if let Some(plan) = self.plans.lock().unwrap().get(plan_id).cloned() {
            if plan.target_agent == "bentokit" {
                std::fs::create_dir_all(&self.plan_dir)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        &self.plan_dir,
                        std::fs::Permissions::from_mode(0o700),
                    )?;
                }
                let body = serde_json::to_vec(&*plan)
                    .map_err(|e| portal_core::error::PortalError::Other(e.to_string()))?;
                portal_core::util::paths::atomic_write(
                    &self.plan_dir.join(format!("{plan_id}.json")),
                    &body,
                )?;
            }
            return Ok(Some(plan));
        }
        let path = self.plan_dir.join(format!("{plan_id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        Ok(Some(Arc::new(serde_json::from_slice(&bytes).map_err(
            |e| portal_core::error::PortalError::Other(e.to_string()),
        )?)))
    }

    pub fn finish_plan(&self, id: &str) {
        self.plans.lock().unwrap().remove(id);
        let _ = std::fs::remove_file(self.plan_dir.join(format!("{id}.json")));
    }

    pub fn recoverable_plans(&self) -> Vec<portal_core::migration::types::DryRunReport> {
        let Ok(entries) = std::fs::read_dir(&self.plan_dir) else {
            return vec![];
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let bytes = std::fs::read(entry.path()).ok()?;
                let plan: PlannedMigration = serde_json::from_slice(&bytes).ok()?;
                if self
                    .ledger
                    .get(&plan.report.plan_id)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    return None;
                }
                Some(plan.report)
            })
            .collect()
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use portal_core::adapter::SessionLocator;
    use portal_core::dto::Installation;
    use portal_core::migration::{engine, types::MigrationDestination};

    #[test]
    fn executed_import_plans_survive_restart_while_dry_runs_do_not_write_plans() {
        let root =
            std::env::temp_dir().join(format!("portal-plan-recovery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let state = AppState::new(root.clone());
        let source: Arc<dyn AgentAdapter> =
            Arc::new(portal_adapters::claude_code::ClaudeCodeAdapter);
        let target: Arc<dyn AgentAdapter> = Arc::new(portal_adapters::codex::CodexAdapter);
        let source_inst = Installation {
            cli_path: None,
            version: None,
            store_root: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../crates/portal-adapters/fixtures/claude-code/v2.1/store")
                .display()
                .to_string(),
        };
        let target_inst = Installation {
            cli_path: None,
            version: None,
            store_root: root.join("target").display().to_string(),
        };
        let mut plan = engine::plan_native(
            &source,
            &source_inst,
            &target,
            &target_inst,
            &SessionLocator {
                native_id: "11111111-1111-1111-1111-111111111111".into(),
                store_path: None,
            },
        )
        .unwrap();
        plan.target_agent = "bentokit".into();
        plan.destination = Some(MigrationDestination {
            profile: "synthetic".into(),
            project_id: None,
            endpoint: None,
            operation_id: Some(plan.report.plan_id.clone()),
        });
        let id = plan.report.plan_id.clone();
        state.store_plan(plan).unwrap();
        assert!(!root.join("migration-plans").exists());
        state.take_plan(&id).unwrap().unwrap();
        drop(state);
        let restarted = AppState::new(root.clone());
        assert_eq!(restarted.recoverable_plans().len(), 1);
        let recovered = restarted.take_plan(&id).unwrap().unwrap();
        assert_eq!(
            recovered
                .destination
                .as_ref()
                .unwrap()
                .operation_id
                .as_deref(),
            Some(id.as_str())
        );
        assert!(restarted.take_plan("../escape").unwrap().is_none());
        restarted.finish_plan(&id);
        assert!(restarted.recoverable_plans().is_empty());
        drop(restarted);
        let _ = std::fs::remove_dir_all(root);
    }
}
