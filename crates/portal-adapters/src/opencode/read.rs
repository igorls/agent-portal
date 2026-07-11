//! OpenCode SQLite -> IR.

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::Connection;
use serde_json::Value;

use portal_core::dto::{ProjectRef, SessionSummary};
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, TokenUsage, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::ID;

fn ms_to_dt(ms: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms).single()
}

fn map_err(e: rusqlite::Error) -> PortalError {
    PortalError::Other(format!("opencode query: {e}"))
}

/// Enumerate top-level (non-subagent), non-archived sessions grouped by
/// project worktree. One query — SQLite makes this cheap, no file peeking.
pub fn snapshot(conn: &Connection) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.directory, s.title, s.model, s.time_created, s.time_updated,
                    (SELECT COUNT(*) FROM message m WHERE m.session_id = s.id) AS msg_count
             FROM session s
             WHERE s.parent_id IS NULL AND s.time_archived IS NULL",
        )
        .map_err(map_err)?;

    let now_ms = Utc::now().timestamp_millis();
    let rows = stmt
        .query_map([], |r| {
            let id: String = r.get(0)?;
            let directory: String = r.get(1)?;
            let title: Option<String> = r.get(2)?;
            let model_json: Option<String> = r.get(3)?;
            let created: Option<i64> = r.get(4)?;
            let updated: Option<i64> = r.get(5)?;
            let msg_count: i64 = r.get(6)?;
            Ok((
                id, directory, title, model_json, created, updated, msg_count,
            ))
        })
        .map_err(map_err)?;

    let mut by_project: HashMap<String, (ProjectRef, Vec<SessionSummary>)> = HashMap::new();
    for row in rows {
        let (id, directory, title, model_json, created, updated, msg_count) =
            row.map_err(map_err)?;
        let model = model_json.as_deref().and_then(model_id);
        let updated_ms = updated.unwrap_or(0);
        let summary = SessionSummary {
            agent_id: ID.to_string(),
            native_id: id,
            project_key: normalize_cwd(&directory),
            title: title.filter(|t| !t.is_empty()),
            cwd: Some(directory.clone()),
            git_branch: None,
            model,
            created_at: created.and_then(ms_to_dt),
            updated_at: updated.and_then(ms_to_dt),
            message_count: Some(msg_count as u32),
            message_count_exact: true,
            size_bytes: 0,
            store_path: String::new(),
            maybe_live: (now_ms - updated_ms) < 120_000 && updated_ms > 0,
        };

        let key = normalize_cwd(&directory);
        let entry = by_project.entry(key.clone()).or_insert_with(|| {
            (
                ProjectRef {
                    key,
                    cwd: Some(directory.clone()),
                    label: label_from_cwd(&directory),
                },
                Vec::new(),
            )
        });
        entry.1.push(summary);
    }

    Ok(by_project.into_values().collect())
}

pub fn read_session(conn: &Connection, session_id: &str) -> Result<CanonicalSession> {
    // Session-level facts.
    let (directory, title, version, agent_version) = conn
        .query_row(
            "SELECT directory, title, version FROM session WHERE id = ?1",
            [session_id],
            |r| {
                let directory: String = r.get(0)?;
                let title: Option<String> = r.get(1)?;
                let version: Option<String> = r.get(2)?;
                Ok((directory, title, version.clone(), version))
            },
        )
        .map_err(|_| PortalError::Other(format!("opencode session {session_id} not found")))?;
    let _ = version;

    // Messages in order, each with its parts.
    let mut msg_stmt = conn
        .prepare(
            "SELECT id, time_created, data FROM message WHERE session_id = ?1 ORDER BY time_created ASC, id ASC",
        )
        .map_err(map_err)?;
    let messages = msg_stmt
        .query_map([session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(map_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_err)?;

    let mut part_stmt = conn
        .prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created ASC, id ASC")
        .map_err(map_err)?;

    let mut timeline = Vec::new();
    let mut totals = UsageTotals::default();
    let mut reasoning_count = 0usize;
    let mut unknown_parts: HashMap<String, usize> = HashMap::new();

    for (msg_id, msg_created, msg_data) in messages {
        let data: Value = serde_json::from_str(&msg_data).unwrap_or(Value::Null);
        let role = match data["role"].as_str() {
            Some("assistant") => Role::Assistant,
            Some("user") => Role::User,
            _ => Role::System,
        };
        let model = data["modelID"].as_str().map(str::to_string);
        let usage = data.get("tokens").map(|t| TokenUsage {
            input_tokens: t["input"].as_u64().unwrap_or(0),
            output_tokens: t["output"].as_u64().unwrap_or(0),
            cache_read_tokens: t["cache"]["read"].as_u64().unwrap_or(0),
            cache_write_tokens: t["cache"]["write"].as_u64().unwrap_or(0),
        });
        if let Some(u) = &usage {
            totals.input_tokens += u.input_tokens;
            totals.output_tokens += u.output_tokens;
            totals.known = true;
        }

        let mut blocks = Vec::new();
        let parts = part_stmt
            .query_map([&msg_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        for part in parts {
            let part: String = part.map_err(map_err)?;
            let pv: Value = match serde_json::from_str(&part) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match pv["type"].as_str() {
                Some("text") => {
                    if let Some(text) = pv["text"].as_str() {
                        if !text.trim().is_empty() {
                            blocks.push(Block::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("reasoning") => {
                    reasoning_count += 1;
                    blocks.push(Block::Thinking {
                        text: pv["text"].as_str().unwrap_or("").to_string(),
                        encrypted: false,
                    });
                }
                Some("tool") => {
                    // One part = a call and its result together.
                    let call_id = pv["callID"].as_str().unwrap_or("").to_string();
                    let name = pv["tool"].as_str().unwrap_or("").to_string();
                    let state = &pv["state"];
                    blocks.push(Block::ToolCall {
                        call_id: call_id.clone(),
                        name,
                        arguments: state["input"].clone(),
                    });
                    let status = state["status"].as_str().unwrap_or("");
                    // Only completed/errored tools carry a result; a pending
                    // one (interrupted) yields just the call.
                    if !state["output"].is_null() || status == "completed" || status == "error" {
                        blocks.push(Block::ToolResult {
                            call_id,
                            output: state["output"].clone(),
                            is_error: status == "error",
                        });
                    }
                }
                Some(other) => {
                    // step-start / step-finish / patch / snapshot / file — kept
                    // as meta so nothing is silently dropped.
                    *unknown_parts.entry(other.to_string()).or_default() += 1;
                    blocks.push(Block::Meta {
                        source_kind: format!("opencode/{other}"),
                        raw: pv,
                    });
                }
                None => {}
            }
        }

        if blocks.is_empty() {
            continue;
        }
        timeline.push(Turn {
            id: msg_id,
            parent_id: data["parentID"].as_str().map(str::to_string),
            role,
            timestamp: msg_created.and_then(ms_to_dt),
            model,
            is_meta: false,
            blocks,
            usage,
            raw: None,
        });
    }

    let mut losses = Vec::new();
    if reasoning_count > 0 {
        // Readable here, but dropped when written to a target that can't
        // reconstruct signed reasoning.
        losses.push(LossNote {
            code: LossCode::ThinkingDropped,
            detail: format!("{reasoning_count} reasoning block(s) will drop on native migration"),
            turn_id: None,
        });
    }

    let title = title
        .filter(|t| !t.is_empty())
        .or_else(|| first_user_text(&timeline));

    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: session_id.to_string(),
            agent_id: ID.to_string(),
            store_path: String::new(),
            agent_version,
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&directory),
            project_label: label_from_cwd(&directory),
            cwd: directory,
            git_branch: None,
        },
        title,
        timeline,
        attachments: Vec::new(),
        usage: totals,
        losses,
        fidelity: Fidelity::Full,
    })
}

/// `{"id":"glm-5.2","providerID":"...","variant":"..."}` -> "glm-5.2".
fn model_id(json: &str) -> Option<String> {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| v["id"].as_str().map(str::to_string))
}

fn first_user_text(timeline: &[Turn]) -> Option<String> {
    for turn in timeline {
        if turn.role != Role::User {
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
