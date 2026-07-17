//! Pi agent store reader: project-encoded dirs of timestamped JSONL files.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use portal_core::adapter::SessionLocator;
use portal_core::dto::{Installation, ProjectRef, SessionSummary};
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    tool_output_text, Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity,
    TokenUsage, Turn, UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::jsonl;
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::ID;

pub fn snapshot(inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
    let root = PathBuf::from(&inst.store_root);
    let mut projects = Vec::new();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(projects),
        Err(error) => return Err(error.into()),
    };

    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().to_string();
        let mut sessions = Vec::new();

        let Ok(files) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(summary) = summarize_session(&path, &key) {
                sessions.push(summary);
            }
        }

        if sessions.is_empty() {
            continue;
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        let cwd = sessions
            .iter()
            .find_map(|s| s.cwd.clone())
            .or_else(|| decode_project_key(&key));
        let label = cwd
            .as_deref()
            .map(label_from_cwd)
            .unwrap_or_else(|| key.clone());
        projects.push((ProjectRef { key, cwd, label }, sessions));
    }
    Ok(projects)
}

fn summarize_session(path: &Path, project_key: &str) -> Option<SessionSummary> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 {
        return None;
    }
    let peek = jsonl::peek(path, jsonl::DEFAULT_WINDOW).ok()?;
    let header = peek
        .head
        .iter()
        .find(|v| v["type"].as_str() == Some("session"))?;
    let native_id = header["id"].as_str()?.to_string();
    let cwd = header["cwd"].as_str().map(str::to_string);
    let created_at = header["timestamp"].as_str().and_then(parse_ts);
    let updated_at = peek
        .tail
        .iter()
        .rev()
        .find_map(|v| v["timestamp"].as_str().and_then(parse_ts))
        .or_else(|| meta.modified().ok().map(DateTime::<Utc>::from));

    // Prefer an explicit session name if present; else first user text.
    let title = peek
        .head
        .iter()
        .find_map(|v| {
            if v["type"].as_str() == Some("session_name") || v["type"].as_str() == Some("name") {
                v["name"]
                    .as_str()
                    .or_else(|| v["title"].as_str())
                    .map(str::to_string)
            } else {
                None
            }
        })
        .or_else(|| first_user_title(&peek.head));

    let model = peek
        .tail
        .iter()
        .rev()
        .chain(peek.head.iter().rev())
        .find_map(|v| match v["type"].as_str() {
            Some("message") => v["message"]["model"].as_str().map(str::to_string),
            Some("model_change") => v["modelId"].as_str().map(str::to_string),
            _ => None,
        });

    let (message_count, message_count_exact) = match peek.exact_line_count {
        Some(n) => {
            if peek.size <= jsonl::DEFAULT_WINDOW * 2 {
                let content = peek
                    .head
                    .iter()
                    .filter(|v| v["type"].as_str() == Some("message"))
                    .count() as u32;
                (Some(content), true)
            } else {
                (Some(n.saturating_sub(1)), false)
            }
        }
        None => (peek.estimated_line_count, false),
    };

    let maybe_live = meta
        .modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|e| e.as_secs() < 120);

    Some(SessionSummary {
        agent_id: ID.into(),
        native_id,
        project_key: project_key.into(),
        title,
        cwd,
        git_branch: None,
        model,
        created_at,
        updated_at,
        message_count,
        message_count_exact,
        size_bytes: meta.len(),
        store_path: path.display().to_string(),
        maybe_live,
    })
}

fn first_user_title(records: &[Value]) -> Option<String> {
    for v in records {
        if v["type"].as_str() != Some("message") {
            continue;
        }
        if v["message"]["role"].as_str() != Some("user") {
            continue;
        }
        let text = first_text_block(&v["message"]["content"])?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        const MAX: usize = 72;
        if trimmed.chars().count() <= MAX {
            return Some(trimmed.to_string());
        }
        let short: String = trimmed.chars().take(MAX).collect();
        return Some(format!("{short}…"));
    }
    None
}

fn first_text_block(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    content.as_array()?.iter().find_map(|item| {
        if item["type"].as_str() == Some("text") {
            item["text"].as_str().map(str::to_string)
        } else {
            None
        }
    })
}

pub fn read_session(inst: &Installation, locator: &SessionLocator) -> Result<CanonicalSession> {
    let root = PathBuf::from(&inst.store_root);
    let path = resolve_path(&root, locator)?;

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

    let mut losses = Vec::new();
    if unparseable > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{unparseable} unparseable line(s) skipped"),
            turn_id: None,
        });
    }

    let header = records
        .iter()
        .find(|v| v["type"].as_str() == Some("session"));
    let cwd = header
        .and_then(|v| v["cwd"].as_str())
        .unwrap_or("")
        .to_string();
    let native_id = header
        .and_then(|v| v["id"].as_str())
        .unwrap_or(&locator.native_id)
        .to_string();
    let created_at = header.and_then(|v| v["timestamp"].as_str().and_then(parse_ts));

    // Active chain from the last content-bearing record (message or compaction).
    let by_id: HashMap<&str, &Value> = records
        .iter()
        .filter_map(|v| v["id"].as_str().map(|id| (id, v)))
        .collect();
    let leaf = records
        .iter()
        .rev()
        .find(|v| {
            matches!(
                v["type"].as_str(),
                Some("message") | Some("compaction") | Some("model_change")
                    | Some("thinking_level_change")
            )
        })
        .and_then(|v| v["id"].as_str());

    let mut active: HashSet<&str> = HashSet::new();
    let mut cursor = leaf;
    while let Some(id) = cursor {
        if !active.insert(id) {
            break;
        }
        cursor = by_id.get(id).and_then(|v| v["parentId"].as_str());
    }

    let abandoned = records
        .iter()
        .filter(|v| {
            v["type"].as_str() == Some("message")
                && v["id"]
                    .as_str()
                    .is_some_and(|id| !active.contains(id))
        })
        .count();
    if abandoned > 0 {
        losses.push(LossNote {
            code: LossCode::AbandonedBranch,
            detail: format!("{abandoned} message(s) off the resume parent chain"),
            turn_id: None,
        });
    }

    let mut timeline = Vec::new();
    let mut usage = UsageTotals::default();
    let mut unknown = 0usize;
    let mut current_model: Option<String> = None;
    let mut title: Option<String> = None;

    // Session header as meta.
    if let Some(header) = header {
        timeline.push(Turn {
            id: header["id"].as_str().unwrap_or("session").to_string(),
            parent_id: None,
            role: Role::System,
            timestamp: created_at,
            model: None,
            is_meta: true,
            blocks: vec![Block::Meta {
                source_kind: "session".into(),
                raw: header.clone(),
            }],
            usage: None,
            raw: Some(header.clone()),
        });
    }

    let chain: Vec<&Value> = records
        .iter()
        .filter(|v| {
            // Always include session header above; skip it here.
            if v["type"].as_str() == Some("session") {
                return false;
            }
            v["id"].as_str().is_some_and(|id| active.contains(id))
        })
        .collect();

    for record in chain {
        let turn_id = record["id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("L{}", timeline.len()));
        let timestamp = record["timestamp"].as_str().and_then(parse_ts);
        let parent_id = record["parentId"].as_str().map(str::to_string);

        match record["type"].as_str() {
            Some("model_change") => {
                current_model = record["modelId"].as_str().map(str::to_string);
                timeline.push(Turn {
                    id: turn_id,
                    parent_id,
                    role: Role::System,
                    timestamp,
                    model: current_model.clone(),
                    is_meta: true,
                    blocks: vec![Block::Meta {
                        source_kind: "model_change".into(),
                        raw: record.clone(),
                    }],
                    usage: None,
                    raw: Some(record.clone()),
                });
            }
            Some("thinking_level_change") => {
                timeline.push(Turn {
                    id: turn_id,
                    parent_id,
                    role: Role::System,
                    timestamp,
                    model: None,
                    is_meta: true,
                    blocks: vec![Block::Meta {
                        source_kind: "thinking_level_change".into(),
                        raw: record.clone(),
                    }],
                    usage: None,
                    raw: Some(record.clone()),
                });
            }
            Some("compaction") => {
                let summary = record["summary"].as_str().unwrap_or_default().to_string();
                timeline.push(Turn {
                    id: turn_id,
                    parent_id,
                    role: Role::System,
                    timestamp,
                    model: None,
                    is_meta: false,
                    blocks: vec![Block::Compaction { summary }],
                    usage: None,
                    raw: Some(record.clone()),
                });
            }
            Some("session_name") | Some("name") => {
                if title.is_none() {
                    title = record["name"]
                        .as_str()
                        .or_else(|| record["title"].as_str())
                        .map(str::to_string);
                }
                timeline.push(Turn {
                    id: turn_id,
                    parent_id,
                    role: Role::System,
                    timestamp,
                    model: None,
                    is_meta: true,
                    blocks: vec![Block::Meta {
                        source_kind: "session_name".into(),
                        raw: record.clone(),
                    }],
                    usage: None,
                    raw: Some(record.clone()),
                });
            }
            Some("message") => {
                let msg = &record["message"];
                let role = match msg["role"].as_str() {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    Some("system") => Role::System,
                    Some("toolResult") | Some("tool") => Role::Tool,
                    other => {
                        unknown += 1;
                        timeline.push(Turn {
                            id: turn_id,
                            parent_id,
                            role: Role::System,
                            timestamp,
                            model: None,
                            is_meta: true,
                            blocks: vec![Block::Meta {
                                source_kind: other.unwrap_or("unknown-role").into(),
                                raw: record.clone(),
                            }],
                            usage: None,
                            raw: Some(record.clone()),
                        });
                        continue;
                    }
                };

                let blocks = if role == Role::Tool {
                    vec![tool_result_block(msg)]
                } else {
                    content_blocks(msg)
                };
                if blocks.is_empty() {
                    continue;
                }

                if title.is_none() && role == Role::User {
                    if let Some(text) = first_text_block(&msg["content"]) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            const MAX: usize = 72;
                            title = Some(if trimmed.chars().count() <= MAX {
                                trimmed.to_string()
                            } else {
                                let short: String = trimmed.chars().take(MAX).collect();
                                format!("{short}…")
                            });
                        }
                    }
                }

                let model = msg["model"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| current_model.clone());
                let turn_usage = extract_usage(msg);
                if let Some(ref u) = turn_usage {
                    usage.known = true;
                    usage.input_tokens += u.input_tokens;
                    usage.output_tokens += u.output_tokens;
                }

                timeline.push(Turn {
                    id: turn_id,
                    parent_id,
                    role,
                    timestamp,
                    model: (role == Role::Assistant).then_some(model).flatten(),
                    is_meta: false,
                    blocks,
                    usage: turn_usage,
                    raw: Some(record.clone()),
                });
            }
            Some(other) => {
                unknown += 1;
                timeline.push(Turn {
                    id: turn_id,
                    parent_id,
                    role: Role::System,
                    timestamp,
                    model: None,
                    is_meta: true,
                    blocks: vec![Block::Meta {
                        source_kind: other.into(),
                        raw: record.clone(),
                    }],
                    usage: None,
                    raw: Some(record.clone()),
                });
            }
            None => {
                unknown += 1;
            }
        }
    }

    if unknown > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{unknown} record(s) with unknown shape preserved as metadata"),
            turn_id: None,
        });
    }
    if !usage.known {
        losses.push(LossNote {
            code: LossCode::UsageUnavailable,
            detail: "no per-turn token usage found in Pi transcript".into(),
            turn_id: None,
        });
    }

    let fidelity = if losses
        .iter()
        .any(|l| matches!(l.code, LossCode::AbandonedBranch | LossCode::UnknownRecord))
    {
        Fidelity::Partial
    } else {
        Fidelity::Full
    };

    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id,
            agent_id: ID.into(),
            store_path: path.display().to_string(),
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
        attachments: Vec::new(),
        usage,
        losses,
        fidelity,
    })
}

fn content_blocks(msg: &Value) -> Vec<Block> {
    let content = &msg["content"];
    if let Some(text) = content.as_str() {
        return (!text.is_empty())
            .then(|| Block::Text { text: text.into() })
            .into_iter()
            .collect();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| match item["type"].as_str() {
            Some("text") => Block::Text {
                text: item["text"].as_str().unwrap_or_default().into(),
            },
            Some("thinking") => Block::Thinking {
                text: item["thinking"]
                    .as_str()
                    .or_else(|| item["text"].as_str())
                    .unwrap_or_default()
                    .into(),
                encrypted: false,
            },
            Some("toolCall") | Some("tool_use") => Block::ToolCall {
                call_id: item["id"].as_str().unwrap_or_default().into(),
                name: item["name"].as_str().unwrap_or_default().into(),
                arguments: item
                    .get("arguments")
                    .or_else(|| item.get("input"))
                    .cloned()
                    .unwrap_or(Value::Null),
            },
            Some("image") => Block::Meta {
                source_kind: "image".into(),
                raw: item.clone(),
            },
            other => Block::Meta {
                source_kind: other.unwrap_or("unknown-content").into(),
                raw: item.clone(),
            },
        })
        .collect()
}

fn tool_result_block(msg: &Value) -> Block {
    let call_id = msg["toolCallId"]
        .as_str()
        .or_else(|| msg["tool_call_id"].as_str())
        .unwrap_or_default()
        .to_string();
    let is_error = msg["isError"].as_bool().unwrap_or(false);
    let content = &msg["content"];
    let output = if let Some(s) = content.as_str() {
        Value::String(s.into())
    } else if let Some(arr) = content.as_array() {
        let text = arr
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Value::String(text)
    } else {
        Value::String(tool_output_text(content))
    };
    Block::ToolResult {
        call_id,
        output,
        is_error,
    }
}

fn extract_usage(msg: &Value) -> Option<TokenUsage> {
    let u = msg.get("usage")?;
    let input = u["input"].as_u64().or_else(|| u["input_tokens"].as_u64()).unwrap_or(0);
    let output = u["output"]
        .as_u64()
        .or_else(|| u["output_tokens"].as_u64())
        .unwrap_or(0);
    let cache_read = u["cacheRead"]
        .as_u64()
        .or_else(|| u["cache_read_tokens"].as_u64())
        .unwrap_or(0);
    let cache_write = u["cacheWrite"]
        .as_u64()
        .or_else(|| u["cache_write_tokens"].as_u64())
        .unwrap_or(0);
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return None;
    }
    Some(TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    })
}

fn resolve_path(store_root: &Path, locator: &SessionLocator) -> Result<PathBuf> {
    if let Some(ref p) = locator.store_path {
        let path = PathBuf::from(p);
        if path.is_file() {
            validate_under_root(store_root, &path)?;
            return Ok(path);
        }
    }
    let entries = std::fs::read_dir(store_root).map_err(|e| PortalError::Other(e.to_string()))?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            // Filenames are `<ISO-ts>_<uuid>.jsonl` or just contain the uuid.
            if name == locator.native_id || name.ends_with(&format!("_{}", locator.native_id)) {
                return Ok(path);
            }
        }
    }
    Err(PortalError::Other(format!(
        "Pi session {} not found under {}",
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

/// `--P--rioblocks-bentokit--` → `P:\rioblocks\bentokit` (best-effort fallback).
fn decode_project_key(key: &str) -> Option<String> {
    let trimmed = key.trim_matches('-');
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split("--").filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let drive = parts[0];
    if drive.len() != 1 || !drive.chars().next()?.is_ascii_alphabetic() {
        // Non-Windows / relative encodings: join with `/`.
        return Some(parts.join("/"));
    }
    let mut path = format!("{}:", drive.to_ascii_uppercase());
    for seg in &parts[1..] {
        path.push('\\');
        path.push_str(seg);
    }
    Some(path)
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}
