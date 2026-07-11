use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use portal_core::adapter::{AgentAdapter, HostEnv};
use portal_core::dto::Installation;
use portal_core::index::IndexStore;
use portal_core::migration::engine::PlannedMigration;
use portal_core::migration::ledger::Ledger;
use portal_core::registry::AgentRegistry;

pub struct AppState {
    pub env: HostEnv,
    pub registry: AgentRegistry,
    pub ledger: Ledger,
    pub index: IndexStore,
    /// Detection results are cached: detect() shells out to `<cli> --version`
    /// which costs seconds for npm shims. Board refreshes repopulate this.
    installations: Mutex<HashMap<String, Installation>>,
    /// Dry-run plans awaiting execute, keyed by plan_id.
    plans: Mutex<HashMap<String, Arc<PlannedMigration>>>,
}

impl AppState {
    pub fn new(app_data_dir: std::path::PathBuf) -> Self {
        let index = IndexStore::open(&app_data_dir).unwrap_or_else(|e| {
            // A broken cache must never take down the app; fall back to temp.
            eprintln!("index cache unavailable ({e}); using temp");
            IndexStore::open(&std::env::temp_dir().join("agent-portal")).expect("temp index")
        });
        Self {
            env: HostEnv::from_system(),
            registry: AgentRegistry::new(portal_adapters::builtin_adapters()),
            ledger: Ledger::new(app_data_dir),
            index,
            installations: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
        }
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

    pub fn store_plan(&self, plan: PlannedMigration) -> Arc<PlannedMigration> {
        let arc = Arc::new(plan);
        self.plans
            .lock()
            .unwrap()
            .insert(arc.report.plan_id.clone(), arc.clone());
        arc
    }

    pub fn take_plan(&self, plan_id: &str) -> Option<Arc<PlannedMigration>> {
        self.plans.lock().unwrap().remove(plan_id)
    }
}
