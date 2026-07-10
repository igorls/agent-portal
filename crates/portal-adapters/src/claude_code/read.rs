//! Full Claude Code session parse: JSONL DAG -> CanonicalSession.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use portal_core::adapter::SessionLocator;
use portal_core::dto::Installation;
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Attachment, Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity,
    TokenUsage, Turn, UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::ID;

/// Record kinds that are deliberately non-content: skipping them loses
/// nothing a conversation needs (they are harness state, not dialogue).
const KNOWN_NOISE: &[&str] = &[
    "mode",
    "permission-mode",
    "last-prompt",
    "file-history-snapshot",
    "attachment",
    "queue-operation",
    "agent-name",
];

pub fn read_session(inst: &Installation, locator: &SessionLocator) -> Result<CanonicalSession> {
    let store_root = PathBuf::from(&inst.store_root);
    let path = resolve_path(&store_root, locator)?;

    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::new(file);

    let mut records: Vec<Value> = Vec::new();
    let mut unparseable = 0usize;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line.trim()) {
            Ok(v) => records.push(v),
            Err(_) => unparseable += 1,
        }
    }

    let mut losses: Vec<LossNote> = Vec::new();
    if unparseable > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{unparseable} unparseable line(s) skipped"),
            turn_id: None,
        });
    }

    // --- session-level facts -------------------------------------------------
    let envelope = records.iter().find(|v| v.get("cwd").is_some());
    let cwd = envelope
        .and_then(|v| v["cwd"].as_str())
        .unwrap_or("")
        .to_string();
    let git_branch = envelope
        .and_then(|v| v["gitBranch"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let agent_version = envelope
        .and_then(|v| v["version"].as_str())
        .map(str::to_string);

    let mut title_ai = None;
    let mut title_summary = None;

    // --- DAG: active chain from the resume leaf ------------------------------
    // The leaf is the last content record; walking parentUuid back to the root
    // yields the thread `claude --resume` would continue. The graph must span
    // ALL uuid-bearing records — non-content records (attachment,
    // queue-operation, …) participate in the parent chain even though they
    // never enter the timeline. Anything off the chain is an abandoned branch.
    let by_uuid: HashMap<&str, &Value> = records
        .iter()
        .filter_map(|v| v["uuid"].as_str().map(|u| (u, v)))
        .collect();
    let leaf_uuid = records
        .iter()
        .rev()
        .find(|v| is_content_type(v["type"].as_str()))
        .and_then(|v| v["uuid"].as_str());

    let mut active: HashSet<&str> = HashSet::new();
    let mut cursor = leaf_uuid;
    while let Some(uuid) = cursor {
        if !active.insert(uuid) {
            break; // cycle guard
        }
        cursor = by_uuid
            .get(uuid)
            .and_then(|v| v["parentUuid"].as_str())
            .filter(|parent| by_uuid.contains_key(parent));
    }

    // --- timeline -------------------------------------------------------------
    let mut timeline: Vec<Turn> = Vec::new();
    let mut abandoned = 0usize;
    let mut unknown_kinds: HashMap<String, usize> = HashMap::new();

    for record in &records {
        let kind = record["type"].as_str().unwrap_or("");
        match kind {
            "ai-title" => {
                if let Some(t) = record["aiTitle"].as_str() {
                    title_ai = Some(t.to_string());
                }
            }
            "summary" => {
                if let Some(t) = record["summary"].as_str() {
                    title_summary = Some(t.to_string());
                }
            }
            k if is_content_type(Some(k)) => {
                let uuid = record["uuid"].as_str().unwrap_or("");
                if !uuid.is_empty() && !active.contains(uuid) {
                    abandoned += 1;
                    continue;
                }
                if let Some(turn) = map_turn(record) {
                    timeline.push(turn);
                }
            }
            k if KNOWN_NOISE.contains(&k) => {}
            k => {
                *unknown_kinds.entry(k.to_string()).or_default() += 1;
            }
        }
    }

    if abandoned > 0 {
        losses.push(LossNote {
            code: LossCode::AbandonedBranch,
            detail: format!("{abandoned} record(s) on abandoned DAG branches not in timeline"),
            turn_id: None,
        });
    }
    for (kind, count) in unknown_kinds {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{count} record(s) of unknown type '{kind}' skipped"),
            turn_id: None,
        });
    }

    // --- usage ------------------------------------------------------------
    let mut totals = UsageTotals::default();
    for turn in &timeline {
        if let Some(u) = &turn.usage {
            totals.input_tokens += u.input_tokens;
            totals.output_tokens += u.output_tokens;
            totals.known = true;
        }
    }

    // --- sidechains ---------------------------------------------------------
    let attachments = find_sidechains(&path, &locator.native_id);
    if !attachments.is_empty() {
        losses.push(LossNote {
            code: LossCode::SidechainNotConverted,
            detail: format!(
                "{} subagent sidechain transcript(s) attached but not converted",
                attachments.len()
            ),
            turn_id: None,
        });
    }

    let title = title_ai
        .or(title_summary)
        .or_else(|| first_user_text(&timeline));

    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: locator.native_id.clone(),
            agent_id: ID.to_string(),
            store_path: path.display().to_string(),
            agent_version,
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&cwd),
            project_label: label_from_cwd(&cwd),
            cwd,
            git_branch,
        },
        title,
        timeline,
        attachments,
        usage: totals,
        losses,
        fidelity: Fidelity::Full,
    })
}

fn is_content_type(kind: Option<&str>) -> bool {
    matches!(kind, Some("user") | Some("assistant") | Some("system"))
}

fn map_turn(record: &Value) -> Option<Turn> {
    let kind = record["type"].as_str()?;
    let id = record["uuid"].as_str().unwrap_or("").to_string();
    let parent_id = record["parentUuid"].as_str().map(str::to_string);
    let timestamp = record["timestamp"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let is_meta = record["isMeta"].as_bool().unwrap_or(false);

    let (role, blocks, model, usage) = match kind {
        "user" => {
            let blocks = map_message_content(&record["message"]["content"]);
            (Role::User, blocks, None, None)
        }
        "assistant" => {
            let message = &record["message"];
            let blocks = map_message_content(&message["content"]);
            let model = message["model"].as_str().map(str::to_string);
            let usage = message.get("usage").map(|u| TokenUsage {
                input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
                cache_write_tokens: u["cache_creation_input_tokens"]
                    .as_u64()
                    .or_else(|| u["cache_creation"]["ephemeral_5m_input_tokens"].as_u64())
                    .unwrap_or(0),
            });
            (Role::Assistant, blocks, model, usage)
        }
        "system" => {
            let subtype = record["subtype"].as_str().unwrap_or("");
            let block = if subtype == "compact_boundary" {
                Block::Compaction {
                    summary: record["compactMetadata"]["trigger"]
                        .as_str()
                        .map(|t| format!("compaction ({t})"))
                        .unwrap_or_else(|| "compaction boundary".to_string()),
                }
            } else {
                Block::Meta {
                    source_kind: format!("system/{subtype}"),
                    raw: record.clone(),
                }
            };
            (Role::System, vec![block], None, None)
        }
        _ => return None,
    };

    Some(Turn {
        id,
        parent_id,
        role,
        timestamp,
        model,
        is_meta: is_meta || matches!(role_of(record), Role::System),
        blocks,
        usage,
        raw: Some(record.clone()),
    })
}

fn role_of(record: &Value) -> Role {
    match record["type"].as_str() {
        Some("assistant") => Role::Assistant,
        Some("system") => Role::System,
        _ => Role::User,
    }
}

/// Anthropic-style content: a plain string or an array of typed blocks.
/// Claude nests tool_result blocks inside user messages.
fn map_message_content(content: &Value) -> Vec<Block> {
    if let Some(text) = content.as_str() {
        return vec![Block::Text {
            text: text.to_string(),
        }];
    }
    let Some(items) = content.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| match item["type"].as_str() {
            Some("text") => Block::Text {
                text: item["text"].as_str().unwrap_or("").to_string(),
            },
            Some("thinking") => Block::Thinking {
                text: item["thinking"].as_str().unwrap_or("").to_string(),
                encrypted: false,
            },
            Some("redacted_thinking") => Block::Thinking {
                text: String::new(),
                encrypted: true,
            },
            Some("tool_use") => Block::ToolCall {
                call_id: item["id"].as_str().unwrap_or("").to_string(),
                name: item["name"].as_str().unwrap_or("").to_string(),
                arguments: item["input"].clone(),
            },
            Some("tool_result") => Block::ToolResult {
                call_id: item["tool_use_id"].as_str().unwrap_or("").to_string(),
                output: item["content"].clone(),
                is_error: item["is_error"].as_bool().unwrap_or(false),
            },
            other => Block::Meta {
                source_kind: other.unwrap_or("unknown-block").to_string(),
                raw: item.clone(),
            },
        })
        .collect()
}

fn first_user_text(timeline: &[Turn]) -> Option<String> {
    for turn in timeline {
        if turn.role != Role::User || turn.is_meta {
            continue;
        }
        for block in &turn.blocks {
            if let Block::Text { text } = block {
                let cleaned = text.trim();
                if cleaned.is_empty() || cleaned.starts_with('<') {
                    continue;
                }
                let mut title: String = cleaned.chars().take(90).collect();
                if cleaned.chars().count() > 90 {
                    title.push('…');
                }
                return Some(title);
            }
        }
    }
    None
}

/// Sidechain transcripts: sibling `agent-*.jsonl` files and
/// `<sessionId>/subagents/*.jsonl` (both layouts observed in the wild).
fn find_sidechains(session_path: &Path, native_id: &str) -> Vec<Attachment> {
    let mut out = Vec::new();
    let Some(dir) = session_path.parent() else {
        return out;
    };

    let mut push = |path: PathBuf| {
        let size = std::fs::metadata(&path).map(|m| m.len()).ok();
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(Attachment::Sidechain {
            id,
            file: path.display().to_string(),
            size_bytes: size,
        });
    };

    let subagents = dir.join(native_id).join("subagents");
    if let Ok(rd) = std::fs::read_dir(&subagents) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                push(p);
            }
        }
    }
    out
}

fn resolve_path(store_root: &Path, locator: &SessionLocator) -> Result<PathBuf> {
    if let Some(hint) = &locator.store_path {
        if hint.starts_with(store_root)
            && hint.is_file()
            && hint
                .file_stem()
                .is_some_and(|s| s.to_string_lossy() == locator.native_id)
        {
            return Ok(hint.clone());
        }
    }
    // Locate by id across project dirs.
    let file_name = format!("{}.jsonl", locator.native_id);
    if let Ok(rd) = std::fs::read_dir(store_root) {
        for entry in rd.flatten() {
            let candidate = entry.path().join(&file_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(PortalError::Other(format!(
        "session {} not found under {}",
        locator.native_id,
        store_root.display()
    )))
}
