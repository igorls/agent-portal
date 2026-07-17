//! Factory Droid store reader: project-encoded dirs of UUID JSONL files.

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
            let native_id = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if uuid::Uuid::parse_str(&native_id).is_err() {
                continue;
            }
            if let Some(summary) = summarize_session(&path, &key, &native_id) {
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

fn summarize_session(path: &Path, project_key: &str, native_id: &str) -> Option<SessionSummary> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 {
        return None;
    }
    let peek = jsonl::peek(path, jsonl::DEFAULT_WINDOW).ok()?;
    let start = peek
        .head
        .iter()
        .find(|v| v["type"].as_str() == Some("session_start"))?;
    if start["id"].as_str()? != native_id {
        return None;
    }

    let cwd = start["cwd"].as_str().map(str::to_string);
    let title = start["title"]
        .as_str()
        .filter(|t| !t.is_empty() && *t != "New Session")
        .map(str::to_string);
    let created_at = peek
        .head
        .iter()
        .find_map(|v| v["timestamp"].as_str().and_then(parse_ts));
    let updated_at = peek
        .tail
        .iter()
        .rev()
        .find_map(|v| v["timestamp"].as_str().and_then(parse_ts))
        .or_else(|| meta.modified().ok().map(DateTime::<Utc>::from));

    let model = model_from_settings(path)
        .or_else(|| {
            peek.tail
                .iter()
                .rev()
                .chain(peek.head.iter().rev())
                .find_map(|v| {
                    v["message"]["modelId"]
                        .as_str()
                        .or_else(|| v["message"]["model"].as_str())
                        .map(str::to_string)
                })
        });

    let (message_count, message_count_exact) = match peek.exact_line_count {
        Some(n) => {
            // Exclude session_start / todo_state from the board count.
            let content = peek
                .head
                .iter()
                .filter(|v| v["type"].as_str() == Some("message"))
                .count() as u32;
            // When the whole file fit in the peek window, head == all lines.
            if peek.size <= jsonl::DEFAULT_WINDOW * 2 {
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
        native_id: native_id.into(),
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

fn model_from_settings(jsonl_path: &Path) -> Option<String> {
    let settings = jsonl_path.with_extension("settings.json");
    let raw = std::fs::read_to_string(settings).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value["model"].as_str().map(str::to_string)
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

    let start = records
        .iter()
        .find(|v| v["type"].as_str() == Some("session_start"));
    let cwd = start
        .and_then(|v| v["cwd"].as_str())
        .unwrap_or("")
        .to_string();
    let title = start
        .and_then(|v| v["title"].as_str())
        .filter(|t| !t.is_empty() && *t != "New Session")
        .map(str::to_string);
    let native_id = start
        .and_then(|v| v["id"].as_str())
        .unwrap_or(&locator.native_id)
        .to_string();

    // Active chain: walk parentId from the last message. Non-message records
    // (todo_state, …) usually sit off-chain and become Meta only if on-chain.
    let by_id: HashMap<&str, &Value> = records
        .iter()
        .filter_map(|v| v["id"].as_str().map(|id| (id, v)))
        .collect();
    let leaf = records
        .iter()
        .rev()
        .find(|v| v["type"].as_str() == Some("message"))
        .and_then(|v| v["id"].as_str());

    let mut active: HashSet<&str> = HashSet::new();
    let mut cursor = leaf;
    while let Some(id) = cursor {
        if !active.insert(id) {
            break;
        }
        cursor = by_id.get(id).and_then(|v| v["parentId"].as_str());
    }

    // Preserve file order for the active set.
    let chain: Vec<&Value> = records
        .iter()
        .filter(|v| v["id"].as_str().is_some_and(|id| active.contains(id)))
        .collect();

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

    // session_start itself is metadata; include title-bearing context as Meta.
    if let Some(start) = start {
        timeline.push(Turn {
            id: start["id"]
                .as_str()
                .unwrap_or("session_start")
                .to_string(),
            parent_id: None,
            role: Role::System,
            timestamp: None,
            model: None,
            is_meta: true,
            blocks: vec![Block::Meta {
                source_kind: "session_start".into(),
                raw: start.clone(),
            }],
            usage: None,
            raw: Some(start.clone()),
        });
    }

    for record in chain {
        let turn_id = record["id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("L{}", timeline.len()));
        let timestamp = record["timestamp"].as_str().and_then(parse_ts);
        let parent_id = record["parentId"].as_str().map(str::to_string);

        match record["type"].as_str() {
            Some("message") => {
                let msg = &record["message"];
                let role = match msg["role"].as_str() {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    Some("system") => Role::System,
                    Some("tool") => Role::Tool,
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

                let blocks = content_blocks(msg);
                if blocks.is_empty() {
                    continue;
                }

                // Tool results embedded in user messages (Anthropic style)
                // become Tool turns so the IR pairing model stays consistent.
                let only_tool_results = !blocks.is_empty()
                    && blocks.iter().all(|b| matches!(b, Block::ToolResult { .. }));
                let role = if only_tool_results && role == Role::User {
                    Role::Tool
                } else {
                    role
                };

                let model = msg["modelId"]
                    .as_str()
                    .or_else(|| msg["model"].as_str())
                    .map(str::to_string);
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
                // On-chain harness noise (rare for Droid).
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

    // Off-chain harness records that are useful as Meta (todo_state, …).
    for record in &records {
        if record["type"].as_str() == Some("message") || record["type"].as_str() == Some("session_start")
        {
            continue;
        }
        if record["id"]
            .as_str()
            .is_some_and(|id| active.contains(id))
        {
            continue;
        }
        let Some(kind) = record["type"].as_str() else {
            continue;
        };
        timeline.push(Turn {
            id: record["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("meta-{}", timeline.len())),
            parent_id: None,
            role: Role::System,
            timestamp: record["timestamp"].as_str().and_then(parse_ts),
            model: None,
            is_meta: true,
            blocks: vec![Block::Meta {
                source_kind: kind.into(),
                raw: record.clone(),
            }],
            usage: None,
            raw: Some(record.clone()),
        });
    }

    if unknown > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{unknown} record(s) with unknown shape preserved as metadata"),
            turn_id: None,
        });
    }
    if !usage.known {
        // Prefer settings.json aggregate when per-turn usage is missing.
        if let Some(settings_usage) = settings_usage(&path) {
            usage = settings_usage;
        } else {
            losses.push(LossNote {
                code: LossCode::UsageUnavailable,
                detail: "no per-turn token usage found in Factory Droid transcript".into(),
                turn_id: None,
            });
        }
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
            Some("tool_use") => Block::ToolCall {
                call_id: item["id"].as_str().unwrap_or_default().into(),
                name: item["name"].as_str().unwrap_or_default().into(),
                arguments: item.get("input").cloned().unwrap_or(Value::Null),
            },
            Some("tool_result") => {
                let output = item.get("content").cloned().unwrap_or(Value::Null);
                let output = if output.is_array() {
                    Value::String(
                        output
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                } else if output.is_string() {
                    output
                } else {
                    Value::String(tool_output_text(&output))
                };
                Block::ToolResult {
                    call_id: item["tool_use_id"]
                        .as_str()
                        .or_else(|| item["toolUseId"].as_str())
                        .unwrap_or_default()
                        .into(),
                    output,
                    is_error: item["is_error"].as_bool().unwrap_or(false),
                }
            },
            other => Block::Meta {
                source_kind: other.unwrap_or("unknown-content").into(),
                raw: item.clone(),
            },
        })
        .collect()
}

fn extract_usage(msg: &Value) -> Option<TokenUsage> {
    let u = msg.get("usage")?;
    let input = u["input_tokens"]
        .as_u64()
        .or_else(|| u["inputTokens"].as_u64())
        .or_else(|| u["input"].as_u64())
        .unwrap_or(0);
    let output = u["output_tokens"]
        .as_u64()
        .or_else(|| u["outputTokens"].as_u64())
        .or_else(|| u["output"].as_u64())
        .unwrap_or(0);
    let cache_read = u["cache_read_input_tokens"]
        .as_u64()
        .or_else(|| u["cacheReadTokens"].as_u64())
        .or_else(|| u["cacheRead"].as_u64())
        .unwrap_or(0);
    let cache_write = u["cache_creation_input_tokens"]
        .as_u64()
        .or_else(|| u["cacheWriteTokens"].as_u64())
        .or_else(|| u["cacheWrite"].as_u64())
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

fn settings_usage(jsonl_path: &Path) -> Option<UsageTotals> {
    let settings = jsonl_path.with_extension("settings.json");
    let raw = std::fs::read_to_string(settings).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let u = value.get("tokenUsage")?;
    let input = u["inputTokens"].as_u64().unwrap_or(0);
    let output = u["outputTokens"].as_u64().unwrap_or(0);
    if input == 0 && output == 0 {
        return None;
    }
    Some(UsageTotals {
        input_tokens: input,
        output_tokens: output,
        known: true,
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
    // Walk project dirs for `<uuid>.jsonl`.
    let entries = std::fs::read_dir(store_root).map_err(|e| PortalError::Other(e.to_string()))?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let candidate = dir.join(format!("{}.jsonl", locator.native_id));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(PortalError::Other(format!(
        "Factory Droid session {} not found under {}",
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

/// `-P-rioblocks-bentokit` → `P:\rioblocks\bentokit` (best-effort fallback).
fn decode_project_key(key: &str) -> Option<String> {
    let rest = key.strip_prefix('-')?;
    let mut parts = rest.split('-').filter(|s| !s.is_empty());
    let drive = parts.next()?;
    if drive.len() != 1 || !drive.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    let mut path = format!("{}:", drive.to_ascii_uppercase());
    for seg in parts {
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
