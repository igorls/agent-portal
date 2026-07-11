//! xAI Grok Build adapter (directory-per-session store).
//!
//! Verified against Grok Build 0.2.93. Sessions live under
//! `~/.grok/sessions/<percent-encoded-cwd>/<uuid>/`; cheap board metadata is
//! in `summary.json`, while `chat_history.jsonl` contains the canonical chat.
//! The format is not documented as a write contract, so this adapter reads
//! and resumes native sessions but never synthesizes store files.

mod read;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::Result;
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::CommandSpec;
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
            write_native: SupportLevel::None,
            watch: true,
            launch_resume: true,
            launch_new: false,
            context_tokens: Some(256_000),
            write_confidence: None,
            version_range_tested: "0.2.93".to_string(),
            notes: vec![
                "Sessions live under ~/.grok/sessions, grouped by encoded workspace".into(),
                "Incoming migrations use a handoff brief; raw Grok store writes are disabled"
                    .into(),
                "Encrypted reasoning payloads cannot be transferred".into(),
                "ACP target creation is deferred until Portal can capture its returned session ID"
                    .into(),
            ],
        }
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
}
