//! xAI Grok Build adapter (directory-per-session store).
//!
//! Verified against Grok Build 0.2.93. Sessions live under
//! `~/.grok/sessions/<percent-encoded-cwd>/<uuid>/`; cheap board metadata is
//! in `summary.json`, while `chat_history.jsonl` contains the canonical chat.
//!
//! Native **write** uses the official `grok import` CLI for Claude Code
//! origins only — raw store synthesis is intentionally unsupported.

mod read;
mod write;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::Result;
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::{CommandSpec, WriteOptions, WritePlan, WrittenSession};
use portal_core::util::paths::{cli_version, find_cli};

pub const ID: &str = "grok-build";

pub struct GrokAdapter;

impl AgentAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Grok Build"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::DirPerSession,
            read: SupportLevel::Partial,
            // Partial: native only from Claude Code via `grok import` (see accepts_native_from).
            write_native: SupportLevel::Partial,
            watch: true,
            launch_resume: true,
            // Brief-mode target for every other source (and as an alternative to native).
            launch_new: true,
            context_tokens: Some(256_000),
            write_confidence: Some("Experimental".into()),
            version_range_tested: "0.2.93".to_string(),
            notes: vec![
                "Sessions live under ~/.grok/sessions, grouped by encoded workspace".into(),
                "Native migration from Claude Code uses `grok import` (not raw store writes)"
                    .into(),
                "Other sources migrate via handoff brief".into(),
                "Encrypted reasoning payloads cannot be transferred".into(),
            ],
        }
    }

    fn accepts_native_from(&self, source_agent_id: &str) -> bool {
        write::accepts_source(source_agent_id)
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let store_root = env.store_root(ID, env.home.join(".grok").join("sessions"));
        let cli = find_cli(&env.path_dirs, "grok");
        if !store_root.is_dir() && cli.is_none() {
            return None;
        }
        let version = cli.as_deref().and_then(|path| cli_version(path, "version"));
        Some(Installation {
            cli_path: cli.map(|path| path.display().to_string()),
            version,
            store_root: store_root.display().to_string(),
        })
    }

    fn list_projects(&self, inst: &Installation) -> Result<Vec<ProjectRef>> {
        Ok(read::snapshot(inst)?
            .into_iter()
            .map(|(project, _)| project)
            .collect())
    }

    fn list_sessions(
        &self,
        inst: &Installation,
        project: &ProjectRef,
    ) -> Result<Vec<SessionSummary>> {
        Ok(read::snapshot(inst)?
            .into_iter()
            .find(|(candidate, _)| candidate.key == project.key)
            .map(|(_, sessions)| sessions)
            .unwrap_or_default())
    }

    fn snapshot(&self, inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
        read::snapshot(inst)
    }

    fn read_session(
        &self,
        inst: &Installation,
        locator: &SessionLocator,
    ) -> Result<CanonicalSession> {
        read::read_session(inst, locator)
    }

    fn plan_write(&self, inst: &Installation, session: &CanonicalSession) -> Result<WritePlan> {
        write::plan_write(inst, session)
    }

    fn write_session(
        &self,
        inst: &Installation,
        session: &CanonicalSession,
        opts: &WriteOptions,
    ) -> Result<WrittenSession> {
        write::write_session(inst, session, opts)
    }

    fn resume_command(
        &self,
        _inst: &Installation,
        native_id: &str,
        cwd: &str,
    ) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "grok".into(),
            args: vec![
                "--cwd".into(),
                cwd.into(),
                "--resume".into(),
                native_id.into(),
            ],
            cwd: cwd.into(),
        })
    }

    fn new_session_command(
        &self,
        _inst: &Installation,
        cwd: &str,
        initial_prompt: &str,
    ) -> Result<CommandSpec> {
        // Interactive TUI with an initial prompt (`grok --cwd <dir> "…"`).
        // Prefer this over `-p/--single`, which is headless one-shot and exits.
        Ok(CommandSpec {
            program: "grok".into(),
            args: vec!["--cwd".into(), cwd.into(), initial_prompt.into()],
            cwd: cwd.into(),
        })
    }
}
