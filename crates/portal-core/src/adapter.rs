use std::collections::HashMap;
use std::path::PathBuf;

use crate::dto::{Capabilities, Installation, ProjectRef, SessionSummary, SupportLevel};
use crate::error::{PortalError, Result};
use crate::ir::CanonicalSession;
use crate::migration::types::{CommandSpec, WriteOptions, WritePlan, WrittenSession};

/// How commands refer to a session. `store_path` is an optimization hint
/// (the UI already knows it from enumeration); adapters must validate it
/// lives under their store root and fall back to locating by native id.
#[derive(Debug, Clone)]
pub struct SessionLocator {
    pub native_id: String,
    pub store_path: Option<PathBuf>,
}

/// Host facts adapters need for detection: home dir, PATH entries, and
/// user-configured store-root overrides (settings). Kept explicit so adapter
/// detection is testable against synthetic environments.
#[derive(Debug, Clone)]
pub struct HostEnv {
    pub home: PathBuf,
    pub path_dirs: Vec<PathBuf>,
    /// agent id -> store root override
    pub store_overrides: HashMap<String, PathBuf>,
}

impl HostEnv {
    pub fn from_system() -> Self {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let path_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Self {
            home,
            path_dirs,
            store_overrides: HashMap::new(),
        }
    }

    pub fn store_root(&self, agent_id: &str, default: PathBuf) -> PathBuf {
        self.store_overrides
            .get(agent_id)
            .cloned()
            .unwrap_or(default)
    }
}

/// The heart of Agent Portal: everything the app knows about a specific
/// coding agent goes through this trait. Synchronous by design — all IO is
/// local fs/SQLite and Tauri commands wrap calls in spawn_blocking.
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;

    /// Whether this agent can receive a **native** migration from `source_agent_id`.
    ///
    /// Default: any source when `write_native` is `Full`. Adapters with
    /// origin-restricted native writers (e.g. Grok's `grok import` from Claude
    /// Code only) override this and usually set `write_native` to `Partial`.
    fn accepts_native_from(&self, _source_agent_id: &str) -> bool {
        self.capabilities().write_native == SupportLevel::Full
    }

    /// Detect installation: CLI on PATH and/or store dir present. Either
    /// alone counts (a store can outlive an uninstalled CLI).
    fn detect(&self, env: &HostEnv) -> Option<Installation>;

    fn list_projects(&self, inst: &Installation) -> Result<Vec<ProjectRef>>;

    /// Cheap enumeration: head/tail peeking only, never full parses.
    fn list_sessions(
        &self,
        inst: &Installation,
        project: &ProjectRef,
    ) -> Result<Vec<SessionSummary>>;

    /// Full parse of one session into the canonical IR. This is the only
    /// place a whole transcript is read.
    fn read_session(
        &self,
        _inst: &Installation,
        _locator: &SessionLocator,
    ) -> Result<CanonicalSession> {
        Err(PortalError::Unsupported("read_session"))
    }

    /// Predict what write_session would produce without touching disk.
    /// Drives the dry-run report. Only write-capable adapters implement it.
    fn plan_write(&self, _inst: &Installation, _session: &CanonicalSession) -> Result<WritePlan> {
        Err(PortalError::Unsupported("plan_write"))
    }

    /// Write a canonical session into this agent's store so the agent can
    /// natively resume it. Must only CREATE artifacts (atomic temp+rename)
    /// and report every one of them; the engine handles verify/rollback.
    fn write_session(
        &self,
        _inst: &Installation,
        _session: &CanonicalSession,
        _opts: &WriteOptions,
    ) -> Result<WrittenSession> {
        Err(PortalError::Unsupported("write_session"))
    }

    /// The command a user runs to resume a session of this agent.
    fn resume_command(
        &self,
        _inst: &Installation,
        _native_id: &str,
        _cwd: &str,
    ) -> Result<CommandSpec> {
        Err(PortalError::Unsupported("resume_command"))
    }

    /// The command that starts a *fresh* session seeded with an initial
    /// prompt — used by brief-mode migration to launch the target pointed at
    /// the handoff document. Support gates BriefOnly feasibility as a target.
    fn new_session_command(
        &self,
        _inst: &Installation,
        _cwd: &str,
        _initial_prompt: &str,
    ) -> Result<CommandSpec> {
        Err(PortalError::Unsupported("new_session_command"))
    }

    /// One-pass enumeration of the whole store. Default composes
    /// list_projects + list_sessions; adapters whose stores are cheaper to
    /// walk once (e.g. Codex's date-partitioned tree) override this.
    fn snapshot(&self, inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
        let mut out = Vec::new();
        for project in self.list_projects(inst)? {
            let sessions = self.list_sessions(inst, &project)?;
            out.push((project, sessions));
        }
        Ok(out)
    }
}
