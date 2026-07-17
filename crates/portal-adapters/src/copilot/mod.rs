//! GitHub Copilot Chat adapter (VS Code) — read + brief-target.
//!
//! Copilot Chat stores each conversation as an append-only JSON-Lines event log
//! at `%APPDATA%\<edition>\User\workspaceStorage\<hash>\chatSessions\<id>.jsonl`
//! (edition = "Code" or "Code - Insiders"). The `<hash>` folder's
//! `workspace.json` maps it to a `file:///` workspace uri. Event replay and the
//! IR mapping live in `read`.
//!
//! Native write isn't attempted (a VS Code chat can't be started from a session
//! file via CLI) — Copilot is a migration source and a brief target: the brief
//! is written into the workspace and the editor is opened there for the user to
//! hand to Copilot.

mod read;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::{PortalError, Result};
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::CommandSpec;
use portal_core::util::paths::{
    cli_version, file_uri_to_path, find_cli, label_from_cwd, normalize_cwd,
};

pub const ID: &str = "copilot";

/// VS Code editions to look under, newest/most-common first. `.1` is the CLI
/// name shipped on PATH for that edition.
const EDITIONS: &[(&str, &str)] = &[("Code - Insiders", "code-insiders"), ("Code", "code")];

pub struct CopilotAdapter;

impl AgentAdapter for CopilotAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "GitHub Copilot"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::JsonlPerSession,
            read: SupportLevel::Partial,
            write_native: SupportLevel::None,
            watch: true,
            launch_resume: false, // a VS Code chat can't be resumed from the CLI
            launch_new: true,
            context_tokens: None, // brief-only target; native write not offered
            write_confidence: None,
            version_range_tested: "VS Code 1.10x chat v3".to_string(),
            notes: vec![
                "Chat sessions are JSON-Lines event logs in workspaceStorage".to_string(),
                "A migration source; brief target opens the workspace in VS Code".to_string(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let store = env.store_root(ID, default_store(env)?);
        let cli = EDITIONS
            .iter()
            .find_map(|(_, bin)| find_cli(&env.path_dirs, bin));
        if !store.is_dir() && cli.is_none() {
            return None;
        }
        let version = cli.as_deref().and_then(|c| cli_version(c, "--version"));
        Some(Installation {
            cli_path: cli.map(|p| p.display().to_string()),
            version,
            store_root: store.display().to_string(),
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
            .map(|(_, s)| s)
            .unwrap_or_default())
    }

    fn read_session(
        &self,
        _inst: &Installation,
        locator: &SessionLocator,
    ) -> Result<CanonicalSession> {
        let path = locator
            .store_path
            .clone()
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                PortalError::Other(format!("copilot session {} not found", locator.native_id))
            })?;
        // The workspace uri lives in the <hash> dir two levels up from the file.
        let cwd = path.parent().and_then(Path::parent).and_then(workspace_cwd);
        read::read_session(&path, &locator.native_id, cwd)
    }

    fn snapshot(&self, inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
        let root = PathBuf::from(&inst.store_root);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut by_project: HashMap<String, (ProjectRef, Vec<SessionSummary>)> = HashMap::new();

        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        for hash_dir in entries.flatten().map(|e| e.path()) {
            let chat_dir = hash_dir.join("chatSessions");
            let Ok(sessions) = std::fs::read_dir(&chat_dir) else {
                continue;
            };
            let cwd = workspace_cwd(&hash_dir);
            let project_key = cwd
                .as_deref()
                .map(normalize_cwd)
                .unwrap_or_else(|| "unknown".to_string());
            let label = cwd
                .as_deref()
                .map(label_from_cwd)
                .unwrap_or_else(|| "unknown".to_string());

            for file in sessions.flatten().map(|e| e.path()) {
                if file.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(id) = file.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let peek = read::peek(&file);
                if peek.request_count == 0 {
                    continue; // empty scratch chat
                }
                let meta = file.metadata().ok();
                let modified = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .map(chrono::DateTime::<chrono::Utc>::from);
                let updated_ms = modified.map(|m| m.timestamp_millis()).unwrap_or(0);

                let session = SessionSummary {
                    agent_id: ID.to_string(),
                    native_id: id.to_string(),
                    project_key: project_key.clone(),
                    title: peek.title,
                    cwd: cwd.clone(),
                    git_branch: None,
                    model: peek.model,
                    created_at: peek
                        .created_ms
                        .and_then(chrono::DateTime::from_timestamp_millis),
                    updated_at: modified,
                    message_count: Some(peek.request_count),
                    message_count_exact: true,
                    size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    store_path: file.display().to_string(),
                    maybe_live: updated_ms > 0 && (now_ms - updated_ms) < 120_000,
                };
                by_project
                    .entry(project_key.clone())
                    .or_insert_with(|| {
                        (
                            ProjectRef {
                                key: project_key.clone(),
                                cwd: cwd.clone(),
                                label: label.clone(),
                            },
                            Vec::new(),
                        )
                    })
                    .1
                    .push(session);
            }
        }

        Ok(by_project.into_values().collect())
    }

    fn resume_command(
        &self,
        inst: &Installation,
        _native_id: &str,
        cwd: &str,
    ) -> Result<CommandSpec> {
        // No per-chat CLI resume; best effort is to open the workspace.
        Ok(CommandSpec {
            program: edition_cli(inst).to_string(),
            args: vec![cwd.to_string()],
            cwd: cwd.to_string(),
        })
    }

    fn new_session_command(
        &self,
        inst: &Installation,
        cwd: &str,
        _initial_prompt: &str,
    ) -> Result<CommandSpec> {
        // Open the workspace in VS Code; the handoff brief was written into it
        // for the user to reference in a new Copilot chat.
        Ok(CommandSpec {
            program: edition_cli(inst).to_string(),
            args: vec![cwd.to_string()],
            cwd: cwd.to_string(),
        })
    }

    fn open_project_command(&self, inst: &Installation, cwd: &str) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: edition_cli(inst).to_string(),
            args: vec![cwd.to_string()],
            cwd: cwd.to_string(),
        })
    }
}

/// `%APPDATA%\<edition>\User\workspaceStorage` for the first installed edition.
fn default_store(env: &HostEnv) -> Option<PathBuf> {
    let appdata = env.home.join("AppData").join("Roaming");
    EDITIONS
        .iter()
        .map(|(dir, _)| appdata.join(dir).join("User").join("workspaceStorage"))
        .find(|p| p.is_dir())
}

/// Read a `<hash>` dir's `workspace.json` and resolve its `folder` uri to a path.
fn workspace_cwd(hash_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(hash_dir.join("workspace.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    json.get("folder")
        .and_then(|v| v.as_str())
        .and_then(file_uri_to_path)
}

/// The CLI name for whichever edition this installation points at.
fn edition_cli(inst: &Installation) -> &'static str {
    if inst.store_root.contains("Insiders") {
        "code-insiders"
    } else {
        "code"
    }
}
