//! Claude Code adapter.
//!
//! Store layout (verified against a live 2.1.x install):
//! `~/.claude/projects/<slug>/<session-uuid>.jsonl` where `<slug>` is the cwd
//! with every non-alphanumeric char replaced by `-` (case preserved).
//! JSONL envelope fields: `sessionId`, `cwd`, `gitBranch`, `timestamp`,
//! `uuid`/`parentUuid`, `isMeta`. Record types include the documented
//! `user`/`assistant`/`summary`/`system` plus harness-variant extras
//! (`mode`, `permission-mode`, `ai-title` with an `aiTitle` field,
//! `file-history-snapshot`, `attachment`, `last-prompt`) — parsers must
//! tolerate anything unknown.

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
use portal_core::util::jsonl::{self, JsonlPeek};
use portal_core::util::paths::{cli_version, find_cli, label_from_cwd};

pub const ID: &str = "claude-code";

pub struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::JsonlPerSession,
            read: SupportLevel::Full,
            write_native: SupportLevel::Full,
            watch: true,
            launch_resume: true,
            launch_new: true,
            context_tokens: Some(200_000), // Opus / default; Sonnet 1M fits more
            write_confidence: Some("High".to_string()),
            version_range_tested: "2.0–2.1.x".to_string(),
            notes: vec![
                "Thinking blocks are dropped (provider signatures can't be reconstructed)"
                    .to_string(),
                "Subagent sidechains are recorded but not converted".to_string(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let store_root = env.store_root(ID, env.home.join(".claude").join("projects"));
        let cli = find_cli(&env.path_dirs, "claude");
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
        let store_root = PathBuf::from(&inst.store_root);
        let slug_to_cwd = load_slug_cwd_map(&store_root);

        let mut projects = Vec::new();
        for entry in std::fs::read_dir(&store_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let key = entry.file_name().to_string_lossy().to_string();
            let cwd = slug_to_cwd.get(&key).cloned();
            let label = cwd
                .as_deref()
                .map(label_from_cwd)
                .unwrap_or_else(|| key.clone());
            projects.push(ProjectRef { key, cwd, label });
        }
        Ok(projects)
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
            program: "claude".to_string(),
            args: vec!["--resume".to_string(), native_id.to_string()],
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
            program: "claude".to_string(),
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
            program: "claude".to_string(),
            args: vec![],
            cwd: cwd.to_string(),
        })
    }

    fn list_sessions(
        &self,
        inst: &Installation,
        project: &ProjectRef,
    ) -> Result<Vec<SessionSummary>> {
        let dir = PathBuf::from(&inst.store_root).join(&project.key);
        let mut sessions = Vec::new();
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(sessions),
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if !is_session_file(&path) {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() == 0 {
                continue;
            }
            match summarize_session(&path, project, meta.len(), meta.modified().ok()) {
                Some(summary) => sessions.push(summary),
                None => continue,
            }
        }
        Ok(sessions)
    }
}

/// Top-level `<uuid>.jsonl` files only: sidechain transcripts (`agent-*`) and
/// anything non-uuid-shaped are not primary sessions.
fn is_session_file(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return false;
    }
    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s,
        None => return false,
    };
    stem.len() == 36 && stem.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn summarize_session(
    path: &Path,
    project: &ProjectRef,
    size: u64,
    modified: Option<std::time::SystemTime>,
) -> Option<SessionSummary> {
    let peek = jsonl::peek(path, jsonl::DEFAULT_WINDOW).ok()?;
    if peek.head.is_empty() && peek.tail.is_empty() {
        return None;
    }

    let native_id = path.file_stem()?.to_str()?.to_string();

    // Envelope facts come from the first record that carries them; the very
    // first line may be a `mode`/`permission-mode` record without a cwd.
    let enveloped = peek.head.iter().find(|v| v.get("cwd").is_some());
    let cwd = enveloped
        .and_then(|v| v["cwd"].as_str())
        .map(str::to_string)
        .or_else(|| project.cwd.clone());
    let git_branch = enveloped
        .and_then(|v| v["gitBranch"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let created_at = peek.head.iter().find_map(timestamp_of);
    let updated_at = peek
        .tail
        .iter()
        .rev()
        .find_map(timestamp_of)
        .or_else(|| modified.map(DateTime::<Utc>::from));

    let title = find_title(&peek).or_else(|| first_user_text(&peek.head));

    let model = peek
        .tail
        .iter()
        .rev()
        .filter(|v| v["type"] == "assistant")
        .find_map(|v| v["message"]["model"].as_str())
        .map(str::to_string);

    // Exact turn count if a system/turn_duration record made it into the tail
    // window; otherwise fall back to line counts.
    let exact_from_tail = peek
        .tail
        .iter()
        .rev()
        .filter(|v| v["type"] == "system" && v["subtype"] == "turn_duration")
        .find_map(|v| v["messageCount"].as_u64())
        .map(|n| n as u32);
    let (message_count, message_count_exact) = match (exact_from_tail, peek.exact_line_count) {
        (Some(n), _) => (Some(n), true),
        (None, Some(n)) => (Some(n), true),
        (None, None) => (peek.estimated_line_count, false),
    };

    let maybe_live = modified
        .and_then(|m| m.elapsed().ok())
        .map(|elapsed| elapsed.as_secs() < 120)
        .unwrap_or(false);

    Some(SessionSummary {
        agent_id: ID.to_string(),
        native_id,
        project_key: project.key.clone(),
        title,
        cwd,
        git_branch,
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

fn timestamp_of(v: &Value) -> Option<DateTime<Utc>> {
    v["timestamp"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// `{"type":"ai-title","aiTitle":"..."}` (harness variant, usually near one
/// end) or `{"type":"summary","summary":"..."}` (public format).
fn find_title(peek: &JsonlPeek) -> Option<String> {
    let scan = peek.tail.iter().rev().chain(peek.head.iter());
    for v in scan {
        match v["type"].as_str() {
            Some("ai-title") => {
                if let Some(t) = v["aiTitle"].as_str() {
                    return Some(t.to_string());
                }
            }
            Some("summary") => {
                if let Some(t) = v["summary"].as_str() {
                    return Some(t.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn first_user_text(head: &[Value]) -> Option<String> {
    for v in head {
        if v["type"] != "user" || v["isMeta"].as_bool().unwrap_or(false) {
            continue;
        }
        let content = &v["message"]["content"];
        let text = if let Some(s) = content.as_str() {
            Some(s)
        } else {
            content
                .as_array()
                .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
                .and_then(|b| b["text"].as_str())
        };
        if let Some(text) = text {
            let cleaned = text.trim();
            if cleaned.is_empty() || cleaned.starts_with('<') {
                continue;
            }
            return Some(truncate(cleaned, 90));
        }
    }
    None
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// `~/.claude.json` keeps a projects map keyed by forward-slash cwd
/// (`P:/rioblocks/bentokit`). Slugifying those keys reproduces the store dir
/// names, giving us slug -> real cwd without ever inverting a slug.
fn load_slug_cwd_map(store_root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let claude_json = match store_root.parent().and_then(Path::parent) {
        Some(home) => home.join(".claude.json"),
        None => return map,
    };
    let Ok(raw) = std::fs::read_to_string(&claude_json) else {
        return map;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return map;
    };
    if let Some(projects) = value["projects"].as_object() {
        for key in projects.keys() {
            map.insert(claude_slug(key), key.clone());
        }
    }
    map
}

/// Claude's project-dir slug: every non-alphanumeric char becomes `-`,
/// case preserved. `P:\agent-portal` and `P:/agent-portal` both slug to
/// `P--agent-portal`.
pub fn claude_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_real_store_dirs() {
        assert_eq!(claude_slug("P:/agent-portal"), "P--agent-portal");
        assert_eq!(claude_slug(r"P:\agent-portal"), "P--agent-portal");
        assert_eq!(
            claude_slug("P:/rioblocks/bentokit"),
            "P--rioblocks-bentokit"
        );
        assert_eq!(claude_slug("C:/Users/igorl"), "C--Users-igorl");
        assert_eq!(
            claude_slug("/home/igorls/dev/meshguard"),
            "-home-igorls-dev-meshguard"
        );
    }

    #[test]
    fn session_file_filter() {
        assert!(is_session_file(Path::new(
            "x/d2b94988-2790-4fdf-ab16-8f329b1b9e6a.jsonl"
        )));
        assert!(!is_session_file(Path::new("x/agent-abc123.jsonl")));
        assert!(!is_session_file(Path::new("x/notes.jsonl")));
        assert!(!is_session_file(Path::new(
            "x/d2b94988-2790-4fdf-ab16-8f329b1b9e6a.json"
        )));
    }
}
