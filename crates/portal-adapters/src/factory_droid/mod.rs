//! Factory Droid adapter (`droid` CLI).
//!
//! Store layout (verified against Droid 0.162.1):
//! `~/.factory/sessions/<encoded-cwd>/<uuid>.jsonl` with a sibling
//! `<uuid>.settings.json`. The first transcript line is `session_start`
//! (id, title, cwd); dialogue is Anthropic-shaped `message` records
//! (`text` / `thinking` / `tool_use` / `tool_result` content blocks).
//!
//! v1 is read + resume + brief-target. Native write is deferred until a
//! stable import path exists — raw JSONL synthesis risks breaking resume.

mod read;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::Result;
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::CommandSpec;
use portal_core::util::paths::{cli_version, find_cli};

pub const ID: &str = "factory-droid";

pub struct FactoryDroidAdapter;

impl AgentAdapter for FactoryDroidAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Factory Droid"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::JsonlPerSession,
            read: SupportLevel::Full,
            write_native: SupportLevel::None,
            watch: true,
            launch_resume: true,
            launch_new: true,
            context_tokens: Some(200_000),
            write_confidence: None,
            version_range_tested: "0.162.x".to_string(),
            notes: vec![
                "Sessions live under ~/.factory/sessions, grouped by encoded cwd".into(),
                "Native write deferred; migrate in via handoff brief".into(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let store_root = env.store_root(ID, env.home.join(".factory").join("sessions"));
        let cli = find_cli(&env.path_dirs, "droid");
        if !store_root.is_dir() && cli.is_none() {
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
            program: "droid".into(),
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
        Ok(CommandSpec {
            program: "droid".into(),
            args: vec!["--cwd".into(), cwd.into(), initial_prompt.into()],
            cwd: cwd.into(),
        })
    }

    fn open_project_command(&self, _inst: &Installation, cwd: &str) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "droid".into(),
            args: vec!["--cwd".into(), cwd.into()],
            cwd: cwd.into(),
        })
    }
}
