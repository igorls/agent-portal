//! Google Antigravity adapter (read + brief-target).
//!
//! Antigravity stores conversations as SQLite DBs across three roots under
//! `~/.gemini` (current IDE, legacy IDE, CLI). Each conversation DB has a
//! `steps` table whose `step_payload` is schema-less protobuf with plaintext
//! strings (see `proto`/`read`). Enumeration metadata (title, workspace) comes
//! from the IDE's `agyhub_summaries_proto.pb` and the CLI's
//! `conversation_summaries.db` (see `summaries`).
//!
//! v1 is read + brief-target; native write isn't attempted (the step protobuf
//! isn't fully specified). Opened read-only so Antigravity's live DBs are never
//! touched.

mod proto;
mod read;
mod summaries;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::{PortalError, Result};
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::CommandSpec;
use portal_core::util::paths::{cli_version, find_cli, label_from_cwd, normalize_cwd};

pub const ID: &str = "antigravity";

/// The conversation stores, newest layout first.
const ROOTS: &[&str] = &["antigravity", "antigravity-ide", "antigravity-cli"];

pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Antigravity"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::Sqlite,
            read: SupportLevel::Partial,
            write_native: SupportLevel::None,
            watch: true,
            launch_resume: true,
            launch_new: true,
            context_tokens: None, // brief-only target; native write not offered
            write_confidence: None,
            version_range_tested: "agy 0.x".to_string(),
            notes: vec![
                "Conversations are SQLite with protobuf step payloads".to_string(),
                "Read is best-effort text; a migration source and brief target".to_string(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let gemini = env.store_root(ID, env.home.join(".gemini"));
        let has_store = ROOTS
            .iter()
            .any(|r| gemini.join(r).join("conversations").is_dir());
        let cli = find_cli(&env.path_dirs, "agy");
        if !has_store && cli.is_none() {
            return None;
        }
        let version = cli.as_deref().and_then(|c| cli_version(c, "--version"));
        Some(Installation {
            cli_path: cli.map(|p| p.display().to_string()),
            version,
            store_root: gemini.display().to_string(),
        })
    }

    fn list_projects(&self, inst: &Installation) -> Result<Vec<ProjectRef>> {
        Ok(self
            .snapshot(inst)?
            .into_iter()
            .map(|(p, _)| p)
            .collect())
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
            .map(|(_, s)| s)
            .unwrap_or_default())
    }

    fn read_session(
        &self,
        inst: &Installation,
        locator: &SessionLocator,
    ) -> Result<CanonicalSession> {
        let gemini = PathBuf::from(&inst.store_root);
        let db = find_conversation_db(&gemini, &locator.native_id).ok_or_else(|| {
            PortalError::Other(format!("antigravity conversation {} not found", locator.native_id))
        })?;
        let index = summaries::build_index(&gemini);
        let cwd = index.get(&locator.native_id).and_then(|s| s.workspace.clone());
        read::read_session(&db, &locator.native_id, cwd)
    }

    fn snapshot(&self, inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
        let gemini = PathBuf::from(&inst.store_root);
        let index = summaries::build_index(&gemini);

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut by_project: std::collections::HashMap<String, (ProjectRef, Vec<SessionSummary>)> =
            std::collections::HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();

        for root in ROOTS {
            let dir = gemini.join(root).join("conversations");
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("db") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !seen.insert(id.to_string()) {
                    continue; // same conversation copied across roots
                }
                let summary = index.get(id).cloned().unwrap_or_default();
                if summary.is_subagent {
                    continue; // hide subagent trajectories, like Claude sidechains
                }
                let cwd = summary.workspace.clone();
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(chrono::DateTime::<chrono::Utc>::from);
                let updated = summary
                    .updated_ms
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .or(modified);
                let updated_ms = updated.map(|u| u.timestamp_millis()).unwrap_or(0);

                let project_key = cwd
                    .as_deref()
                    .map(normalize_cwd)
                    .unwrap_or_else(|| "unknown".to_string());
                let session = SessionSummary {
                    agent_id: ID.to_string(),
                    native_id: id.to_string(),
                    project_key: project_key.clone(),
                    title: summary.title.clone(),
                    cwd: cwd.clone(),
                    git_branch: None,
                    model: None,
                    created_at: None,
                    updated_at: updated,
                    message_count: summary.step_count,
                    message_count_exact: summary.step_count.is_some(),
                    size_bytes: entry.metadata().map(|m| m.len()).unwrap_or(0),
                    store_path: path.display().to_string(),
                    maybe_live: updated_ms > 0 && (now_ms - updated_ms) < 120_000,
                };

                let label = cwd
                    .as_deref()
                    .map(label_from_cwd)
                    .unwrap_or_else(|| "unknown".to_string());
                let entry = by_project
                    .entry(project_key.clone())
                    .or_insert_with(|| {
                        (
                            ProjectRef {
                                key: project_key,
                                cwd: cwd.clone(),
                                label,
                            },
                            Vec::new(),
                        )
                    });
                entry.1.push(session);
            }
        }

        Ok(by_project.into_values().collect())
    }

    fn resume_command(
        &self,
        _inst: &Installation,
        _native_id: &str,
        cwd: &str,
    ) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "agy".to_string(),
            args: vec![cwd.to_string()],
            cwd: cwd.to_string(),
        })
    }

    fn new_session_command(
        &self,
        _inst: &Installation,
        cwd: &str,
        initial_prompt: &str,
    ) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "agy".to_string(),
            args: vec![initial_prompt.to_string()],
            cwd: cwd.to_string(),
        })
    }
}

fn find_conversation_db(gemini: &Path, id: &str) -> Option<PathBuf> {
    let file = format!("{id}.db");
    ROOTS
        .iter()
        .map(|r| gemini.join(r).join("conversations").join(&file))
        .find(|p| p.is_file())
}
