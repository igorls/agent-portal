//! BentoKit Core 0.3.2: read-only per-profile SQLite stores.
//!
//! `store_root` is the BentoKit data directory (normally `~/.bentokit`).
//! Native ids are profile-qualified (`<agent>/<conversation>`) because two
//! profiles can contain the same conversation id. Native destination support
//! uses Core's versioned import CLI and guarded per-conversation undo.

mod read;
mod write;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::Result;
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::{
    CommandSpec, DestinationChoices, MigrationDestination, SessionArtifact, WriteOptions,
    WritePlan, WrittenSession,
};
use portal_core::util::paths::{cli_version, find_cli};

pub const ID: &str = "bentokit";

pub struct BentoKitAdapter;

impl AgentAdapter for BentoKitAdapter {
    fn capabilities_for(&self, inst: Option<&Installation>) -> Capabilities {
        let mut caps = self.capabilities();
        if inst.is_some_and(write::supports) {
            caps.write_native = SupportLevel::Full;
            caps.write_confidence = Some("Experimental (import protocol v1)".into());
        }
        caps
    }

    fn destination_choices(&self, inst: &Installation) -> Result<DestinationChoices> {
        write::choices(inst)
    }
    fn plan_write_to(
        &self,
        inst: &Installation,
        session: &CanonicalSession,
        destination: Option<&MigrationDestination>,
    ) -> Result<WritePlan> {
        write::plan(inst, session, destination)
    }
    fn write_session(
        &self,
        inst: &Installation,
        session: &CanonicalSession,
        opts: &WriteOptions,
    ) -> Result<WrittenSession> {
        write::write(inst, session, opts)
    }
    fn undo_session(&self, artifact: &SessionArtifact) -> Result<()> {
        write::undo(artifact)
    }
    fn resume_at(
        &self,
        inst: &Installation,
        id: &str,
        cwd: &str,
        destination: Option<&MigrationDestination>,
    ) -> Result<CommandSpec> {
        let native_id = if id == "<new-session-id>" {
            format!(
                "{}/<new-session-id>",
                destination
                    .map(|d| d.profile.as_str())
                    .unwrap_or("<profile>")
            )
        } else {
            id.into()
        };
        let target_cwd = if let Some(d) = destination {
            read::destination_cwd(inst, d)?
        } else {
            cwd.into()
        };
        let mut command = self.resume_command(inst, &native_id, &target_cwd)?;
        if let Some(endpoint) = destination
            .and_then(|d| d.endpoint.as_ref())
            .filter(|s| !s.is_empty())
        {
            command.args.extend(["--url".into(), endpoint.clone()]);
        }
        Ok(command)
    }
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "BentoKit Core"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::Sqlite,
            read: SupportLevel::Partial,
            write_native: SupportLevel::None,
            watch: true,
            launch_resume: true,
            launch_new: false,
            context_tokens: None,
            write_confidence: None,
            version_range_tested: "0.3.2 (source schema + synthetic fixtures)".into(),
            notes: vec![
                "Reads data/agents/<agent>/conversations.db under the BentoKit data directory".into(),
                "Resume uses bentokit tui and requires the matching runtime on localhost:10001 (BENTOKIT_API_KEY is inherited)".into(),
                "Tool traces contain display arguments; provider state, media, and profile settings are not transferred".into(),
                "Receiving migrations requires Core session import-protocol v1 and explicit destination profile/project selection".into(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let root = env.store_root(ID, env.home.join(".bentokit"));
        // `agent` is also used as a sidecar name, but is too generic to probe
        // on PATH. Core's install:cli command installs `bentokit`.
        let cli = find_cli(&env.path_dirs, "bentokit");
        if !root.is_dir() && cli.is_none() {
            return None;
        }
        Some(Installation {
            version: cli
                .as_deref()
                .and_then(|path| cli_version(path, "--version")),
            cli_path: cli.map(|path| path.display().to_string()),
            store_root: root.display().to_string(),
        })
    }

    fn list_projects(&self, inst: &Installation) -> Result<Vec<ProjectRef>> {
        Ok(self.snapshot(inst)?.into_iter().map(|(p, _)| p).collect())
    }

    fn list_sessions(
        &self,
        inst: &Installation,
        project: &ProjectRef,
    ) -> Result<Vec<SessionSummary>> {
        Ok(self
            .snapshot(inst)?
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
        inst: &Installation,
        native_id: &str,
        cwd: &str,
    ) -> Result<CommandSpec> {
        let (agent, session) = read::split_id(native_id)?;
        Ok(CommandSpec {
            program: inst.cli_path.clone().unwrap_or_else(|| "bentokit".into()),
            args: vec![
                "tui".into(),
                "--agent".into(),
                agent.into(),
                "--session".into(),
                session.into(),
                "--cwd".into(),
                cwd.into(),
            ],
            cwd: cwd.into(),
        })
    }
}
