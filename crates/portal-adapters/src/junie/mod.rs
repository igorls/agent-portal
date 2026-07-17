//! JetBrains Junie adapter (`junie` CLI).
//!
//! Store layout (verified against Junie 26.7.13 / 2285.5):
//! `~/.junie/sessions/index.jsonl` for cheap board metadata, plus
//! `session-<id>/events.jsonl` (and optional `state.json`) for the full
//! event stream. Dialogue is reconstructed from `UserPromptEvent` and
//! nested `SessionA2uxEvent.agentEvent` block updates (tools, terminal,
//! thoughts, result markdown).
//!
//! v1 is read + resume + brief-target. Native write is deferred — Junie
//! owns session ids, task folders, and the event protocol.

mod read;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::Result;
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::CommandSpec;
use portal_core::util::paths::{cli_version, find_cli};

pub const ID: &str = "junie";

pub struct JunieAdapter;

impl AgentAdapter for JunieAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Junie"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::DirPerSession,
            read: SupportLevel::Partial,
            write_native: SupportLevel::None,
            watch: true,
            launch_resume: true,
            launch_new: true,
            context_tokens: None, // model-dependent (BYOK / JetBrains AI)
            write_confidence: None,
            version_range_tested: "26.7.x".to_string(),
            notes: vec![
                "Sessions live under ~/.junie/sessions with an index.jsonl".into(),
                "Transcript is reconstructed from the UI event stream (partial fidelity)".into(),
                "Native write deferred; migrate in via handoff brief".into(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let store_root = env.store_root(ID, env.home.join(".junie").join("sessions"));
        let cli = find_cli(&env.path_dirs, "junie");
        // CLI alone, sessions dir, or parent ~/.junie config all count.
        let junie_home = env.home.join(".junie");
        if !store_root.is_dir() && !junie_home.is_dir() && cli.is_none() {
            return None;
        }
        let version = cli.as_deref().and_then(|c| cli_version(c, "--version"));
        Some(Installation {
            cli_path: cli.map(|p| p.display().to_string()),
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
            .find(|(p, _)| p.key == project.key)
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

    fn resume_command(
        &self,
        _inst: &Installation,
        native_id: &str,
        cwd: &str,
    ) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "junie".into(),
            args: vec![
                "--session-id".into(),
                native_id.into(),
                "--resume".into(),
                "--project".into(),
                cwd.into(),
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
        // Interactive TUI with an initial prompt already submitted.
        Ok(CommandSpec {
            program: "junie".into(),
            args: vec![
                "--project".into(),
                cwd.into(),
                "--prompt".into(),
                initial_prompt.into(),
            ],
            cwd: cwd.into(),
        })
    }

    fn open_project_command(
        &self,
        _inst: &Installation,
        cwd: &str,
    ) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "junie".into(),
            args: vec!["--project".into(), cwd.into()],
            cwd: cwd.into(),
        })
    }
}
