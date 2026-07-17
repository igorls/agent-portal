//! Junie store reader: index.jsonl + per-session events.jsonl.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use portal_core::adapter::SessionLocator;
use portal_core::dto::{Installation, ProjectRef, SessionSummary};
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Attachment, Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::ID;

#[derive(Debug, Default, Clone)]
struct IndexEntry {
    session_id: String,
    project_dir: Option<String>,
    task_name: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    status: Option<String>,
}

pub fn snapshot(inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
    let root = PathBuf::from(&inst.store_root);
    let index = load_index(&root);
    let mut by_id: HashMap<String, IndexEntry> = index
        .into_iter()
        .map(|e| (e.session_id.clone(), e))
        .collect();

    // Walk session dirs so unindexed sessions still appear on the board.
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("session-") {
                continue;
            }
            by_id.entry(name.clone()).or_insert_with(|| IndexEntry {
                session_id: name,
                ..Default::default()
            });
        }
    }

    let mut by_project: HashMap<String, (ProjectRef, Vec<SessionSummary>)> = HashMap::new();

    for (session_id, entry) in by_id {
        let session_dir = root.join(&session_id);
        if !session_dir.is_dir() {
            continue;
        }
        let Some(summary) = summarize_session(&session_dir, &session_id, &entry) else {
            continue;
        };
        let cwd = summary.cwd.clone().unwrap_or_else(|| "unknown".to_string());
        let key = normalize_cwd(&cwd);
        let project_entry = by_project.entry(key.clone()).or_insert_with(|| {
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
        summary.project_key = project_entry.0.key.clone();
        project_entry.1.push(summary);
    }

    let mut projects: Vec<_> = by_project.into_values().collect();
    for (_, sessions) in &mut projects {
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    }
    Ok(projects)
}

fn load_index(root: &Path) -> Vec<IndexEntry> {
    let path = root.join("index.jsonl");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(session_id) = v["sessionId"].as_str().map(str::to_string) else {
            continue;
        };
        out.push(IndexEntry {
            session_id,
            project_dir: v["projectDir"].as_str().map(str::to_string),
            task_name: v["taskName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            created_at: parse_ms(
                v["createdAt"]
                    .as_i64()
                    .or_else(|| v["createdAt"].as_u64().map(|n| n as i64)),
            ),
            updated_at: parse_ms(
                v["updatedAt"]
                    .as_i64()
                    .or_else(|| v["updatedAt"].as_u64().map(|n| n as i64)),
            ),
            status: v["status"].as_str().map(str::to_string),
        });
    }
    // Later index lines win (append-only rewrites).
    let mut map = HashMap::new();
    for e in out {
        map.insert(e.session_id.clone(), e);
    }
    map.into_values().collect()
}

fn summarize_session(
    session_dir: &Path,
    session_id: &str,
    index: &IndexEntry,
) -> Option<SessionSummary> {
    let events_path = session_dir.join("events.jsonl");
    if !events_path.is_file() {
        return None;
    }
    let meta = std::fs::metadata(&events_path).ok()?;
    if meta.len() == 0 {
        return None;
    }

    let cwd = index
        .project_dir
        .clone()
        .or_else(|| cwd_from_state(session_dir))
        .or_else(|| cwd_from_events_head(&events_path));

    let title = index
        .task_name
        .clone()
        .or_else(|| first_prompt(&events_path));
    let created_at = index.created_at.or_else(|| first_event_ts(&events_path));
    let updated_at = index
        .updated_at
        .or_else(|| meta.modified().ok().map(DateTime::<Utc>::from));

    let model = last_model_from_events(&events_path);
    let (message_count, message_count_exact) = count_prompts(&events_path);

    let maybe_live = meta
        .modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|e| e.as_secs() < 120)
        || index
            .status
            .as_deref()
            .is_some_and(|s| !s.is_empty() && s != "COMPLETED" && s != "FINISHED");

    Some(SessionSummary {
        agent_id: ID.into(),
        native_id: session_id.into(),
        project_key: String::new(),
        title,
        cwd,
        git_branch: None,
        model,
        created_at,
        updated_at,
        message_count,
        message_count_exact,
        size_bytes: directory_size(session_dir),
        store_path: session_dir.display().to_string(),
        maybe_live,
    })
}

pub fn read_session(inst: &Installation, locator: &SessionLocator) -> Result<CanonicalSession> {
    let root = PathBuf::from(&inst.store_root);
    let session_dir = resolve_session_dir(&root, locator)?;
    let events_path = session_dir.join("events.jsonl");
    if !events_path.is_file() {
        return Err(PortalError::Other(format!(
            "Junie events.jsonl missing under {}",
            session_dir.display()
        )));
    }

    let file = std::fs::File::open(&events_path)?;
    let reader = std::io::BufReader::new(file);

    let mut timeline = Vec::new();
    let mut losses = Vec::new();
    let mut usage = UsageTotals::default();
    let mut unknown = 0usize;
    let mut invalid = 0usize;
    let mut current_model: Option<String> = None;
    let mut title: Option<String> = None;
    let mut cwd = cwd_from_state(&session_dir).unwrap_or_default();

    // Collapse multi-fire block updates by stepId → last completed (or last) event.
    let mut block_latest: HashMap<String, Value> = HashMap::new();
    // Chronological slots: prompts immediately, block stepIds on first sight
    // (final COMPLETED payload resolved from block_latest at materialize time).
    #[derive(Clone)]
    enum Slot {
        Prompt(Value),
        Block(String),
        Result(String),
        Meta(Value),
    }
    let mut slots: Vec<Slot> = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line.trim()) else {
            invalid += 1;
            continue;
        };
        match record["kind"].as_str() {
            Some("UserPromptEvent") => {
                if title.is_none() {
                    title = record["presentablePrompt"]
                        .as_str()
                        .or_else(|| record["prompt"].as_str())
                        .map(str::to_string);
                }
                slots.push(Slot::Prompt(record));
            }
            Some("SystemMessageEvent") => {
                slots.push(Slot::Meta(record));
            }
            Some("TaskStartedEvent")
            | Some("SkillsStatusEvent")
            | Some("UserMessagesCommittedToHistory")
            | Some("TaskState") => {
                // harness noise — skip silently
            }
            Some("SessionA2uxEvent") => {
                let agent_event = record["event"]["agentEvent"].clone();
                let kind = agent_event["kind"].as_str().unwrap_or("").to_string();

                // Cwd can appear mid-stream.
                if let Some(dir) = agent_event["currentDirectory"].as_str() {
                    if !dir.is_empty() {
                        cwd = dir.to_string();
                    }
                }

                match kind.as_str() {
                    "LlmResponseMetadataEvent" => {
                        if let Some(arr) = agent_event["modelUsage"].as_array() {
                            for m in arr {
                                if let Some(model) = m["model"].as_str() {
                                    current_model = Some(model.to_string());
                                }
                                let input = m["inputTokens"].as_u64().unwrap_or(0);
                                let output = m["outputTokens"].as_u64().unwrap_or(0);
                                let cache_read = m["cacheInputTokens"].as_u64().unwrap_or(0);
                                let cache_write = m["cacheCreateTokens"].as_u64().unwrap_or(0);
                                if input > 0 || output > 0 || cache_read > 0 || cache_write > 0 {
                                    usage.known = true;
                                    usage.input_tokens += input;
                                    usage.output_tokens += output;
                                }
                            }
                        }
                    }
                    "AgentTaskNameUpdatedEvent" => {
                        if title.is_none() {
                            title = agent_event["taskName"]
                                .as_str()
                                .or_else(|| agent_event["name"].as_str())
                                .map(str::to_string);
                        }
                    }
                    "AgentThoughtBlockUpdatedEvent"
                    | "MarkdownBlockUpdatedEvent"
                    | "ToolBlockUpdatedEvent"
                    | "TerminalBlockUpdatedEvent"
                    | "ViewFilesBlockUpdatedEvent"
                    | "FileChangesBlockUpdatedEvent"
                    | "CustomAgentBlockUpdatedEvent"
                    | "ResultBlockUpdatedEvent" => {
                        let step_id = agent_event["stepId"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("anon-{}", slots.len()));

                        let is_new = !block_latest.contains_key(&step_id);
                        // Prefer COMPLETED; otherwise keep latest.
                        let status = agent_event["status"].as_str();
                        let replace = match status {
                            Some("COMPLETED") => true,
                            Some("IN_PROGRESS") => !block_latest
                                .get(&step_id)
                                .and_then(|r| r["event"]["agentEvent"]["status"].as_str())
                                .is_some_and(|s| s == "COMPLETED"),
                            _ => true, // no status: last write wins
                        };
                        if replace {
                            block_latest.insert(step_id.clone(), record);
                        }
                        if is_new {
                            if kind == "ResultBlockUpdatedEvent" {
                                slots.push(Slot::Result(step_id));
                            } else {
                                slots.push(Slot::Block(step_id));
                            }
                        }
                    }
                    "AgentCurrentStatusUpdatedEvent"
                    | "CurrentDirectoryUpdatedEvent"
                    | "EnvironmentVariablesUpdatedEvent"
                    | "TipSuggestionCreatedEvent"
                    | "AgentPlanUpdatedEvent"
                    | "AgentPatchCreatedEvent"
                    | "AgentFailureEvent" => {
                        // status noise — skip
                    }
                    "" => {
                        unknown += 1;
                    }
                    other => {
                        // Unknown a2ux kind — preserve once as meta if useful.
                        if !other.ends_with("UpdatedEvent") {
                            unknown += 1;
                            slots.push(Slot::Meta(record));
                        }
                    }
                }
            }
            Some(other) => {
                unknown += 1;
                slots.push(Slot::Meta(record.clone()));
                let _ = other;
            }
            None => {
                unknown += 1;
            }
        }
    }

    // Fallback cwd from index if still empty.
    if cwd.is_empty() {
        let index = load_index(&root);
        if let Some(entry) = index.iter().find(|e| e.session_id == locator.native_id) {
            if let Some(ref dir) = entry.project_dir {
                cwd = dir.clone();
            }
            if title.is_none() {
                title = entry.task_name.clone();
            }
        }
    }

    // Materialize timeline from slots, reading final block states.
    let mut emitted_blocks: HashMap<String, bool> = HashMap::new();
    for slot in slots {
        match slot {
            Slot::Prompt(record) => {
                let text = record["presentablePrompt"]
                    .as_str()
                    .or_else(|| record["prompt"].as_str())
                    .unwrap_or("")
                    .to_string();
                let turn_id = record["requestId"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("prompt-{}", timeline.len()));
                let mut blocks = vec![Block::Text { text }];
                if let Some(atts) = record["attachments"].as_array() {
                    for a in atts {
                        if let Some(path) = a.as_str() {
                            blocks.push(Block::Meta {
                                source_kind: "attachment".into(),
                                raw: Value::String(path.into()),
                            });
                        }
                    }
                }
                timeline.push(Turn {
                    id: turn_id,
                    parent_id: None,
                    role: Role::User,
                    timestamp: parse_ms(record["timestampMs"].as_i64()),
                    model: None,
                    is_meta: false,
                    blocks,
                    usage: None,
                    raw: Some(record),
                });
            }
            Slot::Block(step_id) => {
                if emitted_blocks.insert(step_id.clone(), true).is_some() {
                    continue;
                }
                let Some(record) = block_latest.get(&step_id) else {
                    continue;
                };
                if let Some(turn) = block_to_turn(record, current_model.as_deref()) {
                    timeline.push(turn);
                }
            }
            Slot::Result(step_id) => {
                if emitted_blocks.insert(step_id.clone(), true).is_some() {
                    continue;
                }
                let Some(record) = block_latest.get(&step_id) else {
                    continue;
                };
                if let Some(turn) = block_to_turn(record, current_model.as_deref()) {
                    timeline.push(turn);
                }
            }
            Slot::Meta(record) => {
                let kind = record["kind"].as_str().unwrap_or("unknown");
                timeline.push(Turn {
                    id: format!("meta-{}", timeline.len()),
                    parent_id: None,
                    role: Role::System,
                    timestamp: parse_ms(record["timestampMs"].as_i64()),
                    model: None,
                    is_meta: true,
                    blocks: vec![Block::Meta {
                        source_kind: kind.into(),
                        raw: record.clone(),
                    }],
                    usage: None,
                    raw: Some(record),
                });
            }
        }
    }

    // Sidechain: task-* and subagents dirs.
    let mut attachments = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&session_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("task-") || name == "subagents" {
                attachments.push(Attachment::Sidechain {
                    id: name.clone(),
                    file: path.display().to_string(),
                    size_bytes: Some(directory_size(&path)),
                });
            }
        }
    }
    if !attachments.is_empty() {
        losses.push(LossNote {
            code: LossCode::SidechainNotConverted,
            detail: format!(
                "{} Junie task/subagent folder(s) recorded but not expanded",
                attachments.len()
            ),
            turn_id: None,
        });
    }

    if invalid > 0 || unknown > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!(
                "{unknown} unsupported event kind(s); {invalid} invalid line(s) skipped"
            ),
            turn_id: None,
        });
    }
    if !usage.known {
        losses.push(LossNote {
            code: LossCode::UsageUnavailable,
            detail: "no LlmResponseMetadataEvent token usage found".into(),
            turn_id: None,
        });
    }
    losses.push(LossNote {
        code: LossCode::ContentSkipped,
        detail: "Junie dialogue reconstructed from UI block events; intermediate assistant prose may be incomplete".into(),
        turn_id: None,
    });

    let native_id = session_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| locator.native_id.clone());

    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id,
            agent_id: ID.into(),
            store_path: session_dir.display().to_string(),
            agent_version: inst.version.clone(),
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&cwd),
            project_label: label_from_cwd(&cwd),
            cwd,
            git_branch: None,
        },
        title,
        timeline,
        attachments,
        usage,
        losses,
        fidelity: Fidelity::Partial,
    })
}

fn block_to_turn(record: &Value, current_model: Option<&str>) -> Option<Turn> {
    let agent_event = &record["event"]["agentEvent"];
    let kind = agent_event["kind"].as_str()?;
    let step_id = agent_event["stepId"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| kind.to_string());
    let timestamp = parse_ms(record["timestampMs"].as_i64());

    match kind {
        "AgentThoughtBlockUpdatedEvent" => {
            let text = agent_event["text"].as_str().unwrap_or("").to_string();
            if text.is_empty() {
                return None;
            }
            Some(Turn {
                id: step_id,
                parent_id: None,
                role: Role::Assistant,
                timestamp,
                model: current_model.map(str::to_string),
                is_meta: false,
                blocks: vec![Block::Thinking {
                    text,
                    encrypted: false,
                }],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        "MarkdownBlockUpdatedEvent" => {
            // Often subagent request/result text; keep as assistant content.
            let text = agent_event["text"].as_str().unwrap_or("").to_string();
            if text.is_empty() {
                return None;
            }
            let agent_name = agent_event["agent"]["name"].as_str().unwrap_or("main");
            let is_main = agent_name == "main";
            Some(Turn {
                id: step_id,
                parent_id: None,
                role: Role::Assistant,
                timestamp,
                model: current_model.map(str::to_string),
                is_meta: !is_main,
                blocks: vec![Block::Text { text }],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        "ResultBlockUpdatedEvent" => {
            let text = agent_event["result"].as_str().unwrap_or("").to_string();
            if text.is_empty() {
                return None;
            }
            Some(Turn {
                id: step_id,
                parent_id: None,
                role: Role::Assistant,
                timestamp,
                model: current_model.map(str::to_string),
                is_meta: false,
                blocks: vec![Block::Text { text }],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        "ToolBlockUpdatedEvent" => {
            if agent_event["status"].as_str() == Some("IN_PROGRESS") {
                return None;
            }
            let call_id = step_id.clone();
            let name = agent_event["toolType"]
                .as_str()
                .or_else(|| agent_event["text"].as_str())
                .unwrap_or("tool")
                .to_string();
            let summary = agent_event["text"].as_str().unwrap_or("").to_string();
            let details = agent_event["details"]
                .as_str()
                .or_else(|| agent_event["output"].as_str())
                .unwrap_or("")
                .to_string();
            let arguments = serde_json::json!({
                "summary": summary,
                "details": agent_event["details"].clone(),
            });
            Some(Turn {
                id: step_id.clone(),
                parent_id: None,
                role: Role::Assistant,
                timestamp,
                model: current_model.map(str::to_string),
                is_meta: false,
                blocks: vec![
                    Block::ToolCall {
                        call_id: call_id.clone(),
                        name,
                        arguments,
                    },
                    Block::ToolResult {
                        call_id,
                        output: Value::String(details),
                        is_error: agent_event["status"].as_str() == Some("FAILED"),
                    },
                ],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        "TerminalBlockUpdatedEvent" => {
            if agent_event["status"].as_str() == Some("IN_PROGRESS") {
                return None;
            }
            let call_id = step_id.clone();
            let command = agent_event["command"].as_str().unwrap_or("").to_string();
            let output = agent_event["presentableOutput"]
                .as_str()
                .or_else(|| agent_event["output"].as_str())
                .unwrap_or("")
                .to_string();
            Some(Turn {
                id: step_id.clone(),
                parent_id: None,
                role: Role::Assistant,
                timestamp,
                model: current_model.map(str::to_string),
                is_meta: false,
                blocks: vec![
                    Block::ToolCall {
                        call_id: call_id.clone(),
                        name: "terminal".into(),
                        arguments: serde_json::json!({ "command": command }),
                    },
                    Block::ToolResult {
                        call_id,
                        output: Value::String(output),
                        is_error: agent_event["status"].as_str() == Some("FAILED"),
                    },
                ],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        "ViewFilesBlockUpdatedEvent" => {
            if agent_event["status"].as_str() == Some("IN_PROGRESS") {
                return None;
            }
            let call_id = step_id.clone();
            let files = agent_event.get("files").cloned().unwrap_or(Value::Null);
            let details = agent_event["details"].as_str().unwrap_or("").to_string();
            Some(Turn {
                id: step_id.clone(),
                parent_id: None,
                role: Role::Assistant,
                timestamp,
                model: current_model.map(str::to_string),
                is_meta: false,
                blocks: vec![
                    Block::ToolCall {
                        call_id: call_id.clone(),
                        name: "view_files".into(),
                        arguments: serde_json::json!({ "files": files }),
                    },
                    Block::ToolResult {
                        call_id,
                        output: Value::String(details),
                        is_error: false,
                    },
                ],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        "FileChangesBlockUpdatedEvent" => {
            if agent_event["status"].as_str() == Some("IN_PROGRESS") {
                return None;
            }
            let call_id = step_id.clone();
            let changes = agent_event.get("changes").cloned().unwrap_or(Value::Null);
            let details = agent_event["details"].as_str().unwrap_or("").to_string();
            // Don't embed huge afterContent blobs in arguments — keep a summary.
            let paths: Vec<String> = changes
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    c["path"]
                        .as_str()
                        .or_else(|| c["relativePath"].as_str())
                        .map(str::to_string)
                })
                .collect();
            Some(Turn {
                id: step_id.clone(),
                parent_id: None,
                role: Role::Assistant,
                timestamp,
                model: current_model.map(str::to_string),
                is_meta: false,
                blocks: vec![
                    Block::ToolCall {
                        call_id: call_id.clone(),
                        name: "file_change".into(),
                        arguments: serde_json::json!({
                            "paths": paths,
                            "details": details,
                        }),
                    },
                    Block::ToolResult {
                        call_id,
                        output: Value::String(details),
                        is_error: false,
                    },
                ],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        "CustomAgentBlockUpdatedEvent" => {
            let text = agent_event["text"]
                .as_str()
                .or_else(|| agent_event["details"].as_str())
                .unwrap_or("")
                .to_string();
            Some(Turn {
                id: step_id,
                parent_id: None,
                role: Role::System,
                timestamp,
                model: None,
                is_meta: true,
                blocks: vec![Block::Meta {
                    source_kind: "custom_agent".into(),
                    raw: if text.is_empty() {
                        agent_event.clone()
                    } else {
                        Value::String(text)
                    },
                }],
                usage: None,
                raw: Some(record.clone()),
            })
        }
        _ => None,
    }
}

fn resolve_session_dir(store_root: &Path, locator: &SessionLocator) -> Result<PathBuf> {
    if let Some(ref p) = locator.store_path {
        let path = PathBuf::from(p);
        if path.is_dir() {
            validate_under_root(store_root, &path)?;
            return Ok(path);
        }
        // Allow pointing at events.jsonl directly.
        if path.is_file() {
            if let Some(parent) = path.parent() {
                validate_under_root(store_root, parent)?;
                return Ok(parent.to_path_buf());
            }
        }
    }
    let direct = store_root.join(&locator.native_id);
    if direct.is_dir() {
        return Ok(direct);
    }
    // Prefix match: user might pass bare id without "session-" prefix.
    if let Ok(entries) = std::fs::read_dir(store_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == locator.native_id || name.ends_with(&locator.native_id) {
                let path = entry.path();
                if path.is_dir() {
                    return Ok(path);
                }
            }
        }
    }
    Err(PortalError::Other(format!(
        "Junie session {} not found under {}",
        locator.native_id,
        store_root.display()
    )))
}

fn validate_under_root(root: &Path, path: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if path.starts_with(&root) {
        Ok(())
    } else {
        Err(PortalError::Other(format!(
            "store_path {} escapes adapter root {}",
            path.display(),
            root.display()
        )))
    }
}

fn cwd_from_state(session_dir: &Path) -> Option<String> {
    let path = session_dir.join("state.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    // Common: event.agentEvent.blob is a JSON string containing currentDirectory.
    if let Some(blob) = value["event"]["agentEvent"]["blob"].as_str() {
        if let Ok(inner) = serde_json::from_str::<Value>(blob) {
            if let Some(dir) = inner["currentDirectory"].as_str() {
                if !dir.is_empty() {
                    return Some(dir.to_string());
                }
            }
        }
    }
    value["event"]["agentEvent"]["currentDirectory"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            value["currentDirectory"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
}

fn cwd_from_events_head(events_path: &Path) -> Option<String> {
    let file = std::fs::File::open(events_path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(80).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(dir) = v["event"]["agentEvent"]["currentDirectory"].as_str() {
            if !dir.is_empty() {
                return Some(dir.to_string());
            }
        }
    }
    None
}

fn first_prompt(events_path: &Path) -> Option<String> {
    let file = std::fs::File::open(events_path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(200).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["kind"].as_str() == Some("UserPromptEvent") {
            return v["presentablePrompt"]
                .as_str()
                .or_else(|| v["prompt"].as_str())
                .map(str::to_string);
        }
    }
    None
}

fn first_event_ts(events_path: &Path) -> Option<DateTime<Utc>> {
    let file = std::fs::File::open(events_path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(20).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(ts) = parse_ms(v["timestampMs"].as_i64()) {
            return Some(ts);
        }
    }
    None
}

fn last_model_from_events(events_path: &Path) -> Option<String> {
    // Tail-read: scan last ~64KB for LlmResponseMetadataEvent.
    let meta = std::fs::metadata(events_path).ok()?;
    let file = std::fs::File::open(events_path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    use std::io::{Seek, SeekFrom};
    let size = meta.len();
    let window = 64 * 1024u64;
    if size > window {
        reader.seek(SeekFrom::End(-(window as i64))).ok()?;
        let mut skip = String::new();
        let _ = reader.read_line(&mut skip);
    }
    let mut last = None;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["kind"].as_str() != Some("SessionA2uxEvent") {
            continue;
        }
        if v["event"]["agentEvent"]["kind"].as_str() != Some("LlmResponseMetadataEvent") {
            continue;
        }
        if let Some(model) = v["event"]["agentEvent"]["modelUsage"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|m| m["model"].as_str())
        {
            last = Some(model.to_string());
        }
    }
    last
}

fn count_prompts(events_path: &Path) -> (Option<u32>, bool) {
    let Ok(file) = std::fs::File::open(events_path) else {
        return (None, false);
    };
    let reader = std::io::BufReader::new(file);
    let mut n = 0u32;
    for line in reader.lines().map_while(Result::ok) {
        if line.contains("\"UserPromptEvent\"") {
            n += 1;
        }
    }
    (Some(n), true)
}

fn directory_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        } else if p.is_dir() {
            total += directory_size(&p);
        }
    }
    total
}

fn parse_ms(ms: Option<i64>) -> Option<DateTime<Utc>> {
    let ms = ms?;
    Utc.timestamp_millis_opt(ms).single()
}
