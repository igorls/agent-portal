//! OpenAI Codex CLI adapter.
//!
//! Store layout (verified against a live 0.143/0.144 install):
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl`, lines of
//! `{timestamp, type, payload}` with types `session_meta` (id/session_id,
//! cwd, cli_version, originator), `turn_context` (payload.model),
//! `response_item`, `event_msg`. Session titles live in a separate
//! `~/.codex/session_index.jsonl` of `{id, thread_name, updated_at}`.

mod read;
mod write;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::Result;
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::{CommandSpec, WriteOptions, WritePlan, WrittenSession};
use portal_core::util::jsonl;
use portal_core::util::paths::{cli_version, find_cli, label_from_cwd, normalize_cwd};

pub const ID: &str = "codex";

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Codex CLI"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::JsonlPerSession,
            read: SupportLevel::Full,
            write_native: SupportLevel::Full,
            watch: true,
            launch_resume: true,
            launch_new: true,
            context_tokens: Some(272_000), // GPT-5-class window
            write_confidence: Some("High".to_string()),
            version_range_tested: "0.143–0.144".to_string(),
            notes: vec![
                "Reasoning items carry encrypted_content and cannot be transferred".to_string(),
                "Titles come from ~/.codex/session_index.jsonl".to_string(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let store_root = env.store_root(ID, env.home.join(".codex").join("sessions"));
        let cli = find_cli(&env.path_dirs, "codex");
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
        Ok(self
            .snapshot(inst)?
            .into_iter()
            .map(|(project, _)| project)
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
            .map(|(_, sessions)| sessions)
            .unwrap_or_default())
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
            program: "codex".to_string(),
            args: vec!["resume".to_string(), native_id.to_string()],
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
            program: "codex".to_string(),
            args: vec![initial_prompt.to_string()],
            cwd: cwd.to_string(),
        })
    }

    fn open_project_command(
        &self,
        _inst: &Installation,
        cwd: &str,
    ) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "codex".to_string(),
            args: vec![],
            cwd: cwd.to_string(),
        })
    }

    /// The store is date-partitioned with no per-project structure, so one
    /// walk enumerates everything and groups by normalized cwd.
    fn snapshot(&self, inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
        let store_root = PathBuf::from(&inst.store_root);
        let titles = load_title_index(&store_root);

        let mut by_project: HashMap<String, (ProjectRef, Vec<SessionSummary>)> = HashMap::new();

        for path in walk_rollouts(&store_root) {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.len() == 0 {
                continue;
            }
            let Some(summary) = summarize_rollout(&path, meta.len(), meta.modified().ok(), &titles)
            else {
                continue;
            };

            let cwd = summary.cwd.clone().unwrap_or_else(|| "unknown".to_string());
            let key = normalize_cwd(&cwd);
            let entry = by_project.entry(key.clone()).or_insert_with(|| {
                (
                    ProjectRef {
                        key,
                        cwd: Some(cwd.clone()),
                        label: label_from_cwd(&cwd),
                    },
                    Vec::new(),
                )
            });
            let mut summary = summary;
            summary.project_key = entry.0.key.clone();
            entry.1.push(summary);
        }

        Ok(by_project.into_values().collect())
    }
}

/// `YYYY/MM/DD` tree, exactly three levels of directories with rollout files
/// at the leaves. Skips `archived_sessions` and anything unexpected.
pub(crate) fn walk_rollouts(store_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let years = match std::fs::read_dir(store_root) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for year in years.flatten().filter(|e| e.path().is_dir()) {
        let Ok(months) = std::fs::read_dir(year.path()) else {
            continue;
        };
        for month in months.flatten().filter(|e| e.path().is_dir()) {
            let Ok(days) = std::fs::read_dir(month.path()) else {
                continue;
            };
            for day in days.flatten().filter(|e| e.path().is_dir()) {
                let Ok(files) = std::fs::read_dir(day.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let path = file.path();
                    let name = file.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                        out.push(path);
                    }
                }
            }
        }
    }
    out
}

fn summarize_rollout(
    path: &Path,
    size: u64,
    modified: Option<std::time::SystemTime>,
    titles: &HashMap<String, (String, Option<DateTime<Utc>>)>,
) -> Option<SessionSummary> {
    // Codex front-loads 100KB+ of instruction payloads into its first lines,
    // so the metadata head is read line-wise with a byte budget instead of a
    // fixed window; the first turn_context (line ~8) is the stop marker.
    let head =
        jsonl::head_lines(path, 16, 2 * 1024 * 1024, |v| v["type"] == "turn_context").ok()?;
    let peek = jsonl::peek(path, jsonl::DEFAULT_WINDOW).ok()?;

    let session_meta = head
        .iter()
        .find(|v| v["type"] == "session_meta")
        .map(|v| &v["payload"])?;

    let native_id = session_meta["id"]
        .as_str()
        .or_else(|| session_meta["session_id"].as_str())?
        .to_string();
    let cwd = session_meta["cwd"].as_str().map(str::to_string);

    let created_at = session_meta["timestamp"]
        .as_str()
        .and_then(parse_ts)
        .or_else(|| head.iter().find_map(record_ts));

    let index_entry = titles.get(&native_id);
    let title = index_entry.map(|(name, _)| name.clone());
    let updated_at = peek
        .tail
        .iter()
        .rev()
        .find_map(record_ts)
        .or_else(|| index_entry.and_then(|(_, ts)| *ts))
        .or_else(|| modified.map(DateTime::<Utc>::from));

    // Prefer the most recent turn_context (tail) — model can change
    // mid-session — falling back to the first one from the head read.
    let model = peek
        .tail
        .iter()
        .rev()
        .chain(head.iter().rev())
        .find(|v| v["type"] == "turn_context")
        .and_then(|v| v["payload"]["model"].as_str())
        .map(str::to_string);

    let (message_count, message_count_exact) = match peek.exact_line_count {
        Some(n) => (Some(n), true),
        None => (peek.estimated_line_count, false),
    };

    let maybe_live = modified
        .and_then(|m| m.elapsed().ok())
        .map(|elapsed| elapsed.as_secs() < 120)
        .unwrap_or(false);

    Some(SessionSummary {
        agent_id: ID.to_string(),
        native_id,
        project_key: String::new(), // filled by snapshot() grouping
        title,
        cwd,
        git_branch: None,
        model,
        created_at,
        updated_at,
        message_count,
        message_count_exact,
        size_bytes: size,
        store_path: path.display().to_string(),
        maybe_live,
    })
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn record_ts(v: &Value) -> Option<DateTime<Utc>> {
    v["timestamp"].as_str().and_then(parse_ts)
}

/// `~/.codex/session_index.jsonl`: `{id, thread_name, updated_at}` — the only
/// place session titles exist. Lives next to (not inside) the sessions dir.
fn load_title_index(store_root: &Path) -> HashMap<String, (String, Option<DateTime<Utc>>)> {
    let mut map = HashMap::new();
    let index_path = match store_root.parent() {
        Some(codex_home) => codex_home.join("session_index.jsonl"),
        None => return map,
    };
    let Ok(raw) = std::fs::read_to_string(&index_path) else {
        return map;
    };
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str()) else {
            continue;
        };
        let ts = v["updated_at"].as_str().and_then(parse_ts);
        // Later lines win: the index is append-only and re-titles happen.
        map.insert(id.to_string(), (name.to_string(), ts));
    }
    map
}
