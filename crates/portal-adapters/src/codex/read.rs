//! Full Codex rollout parse: {timestamp,type,payload} JSONL -> CanonicalSession.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use portal_core::adapter::SessionLocator;
use portal_core::dto::Installation;
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::{walk_rollouts, ID};

pub fn read_session(inst: &Installation, locator: &SessionLocator) -> Result<CanonicalSession> {
    let store_root = PathBuf::from(&inst.store_root);
    let path = resolve_path(&store_root, locator)?;

    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::new(file);

    let mut timeline: Vec<Turn> = Vec::new();
    let mut losses: Vec<LossNote> = Vec::new();
    let mut totals = UsageTotals::default();

    let mut cwd = String::new();
    let mut agent_version: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut encrypted_reasoning = 0usize;
    let mut unparseable = 0usize;

    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line.trim()) else {
            unparseable += 1;
            continue;
        };

        let timestamp = record["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let payload = &record["payload"];
        let turn_id = format!("L{}", line_no + 1);

        match record["type"].as_str() {
            Some("session_meta") => {
                if let Some(c) = payload["cwd"].as_str() {
                    cwd = c.to_string();
                }
                agent_version = payload["cli_version"].as_str().map(str::to_string);
            }
            Some("turn_context") => {
                if let Some(m) = payload["model"].as_str() {
                    current_model = Some(m.to_string());
                }
            }
            Some("response_item") => {
                if let Some(turn) = map_response_item(
                    payload,
                    turn_id,
                    timestamp,
                    current_model.clone(),
                    &record,
                    &mut encrypted_reasoning,
                ) {
                    timeline.push(turn);
                }
            }
            // Content lives in response_item records; event_msg only
            // contributes usage numbers.
            Some("event_msg") if payload["type"] == "token_count" => {
                let usage = &payload["info"]["total_token_usage"];
                if let Some(input) = usage["input_tokens"].as_u64() {
                    totals.input_tokens = input;
                    totals.output_tokens = usage["output_tokens"].as_u64().unwrap_or(0);
                    totals.known = true;
                }
            }
            _ => {}
        }
    }

    if unparseable > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{unparseable} unparseable line(s) skipped"),
            turn_id: None,
        });
    }
    if encrypted_reasoning > 0 {
        losses.push(LossNote {
            code: LossCode::EncryptedReasoning,
            detail: format!(
                "{encrypted_reasoning} reasoning item(s) are provider-encrypted; their content cannot be read or transferred"
            ),
            turn_id: None,
        });
    }

    let title = first_user_text(&timeline);

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

fn map_response_item(
    payload: &Value,
    turn_id: String,
    timestamp: Option<DateTime<Utc>>,
    model: Option<String>,
    raw: &Value,
    encrypted_reasoning: &mut usize,
) -> Option<Turn> {
    let (role, blocks, is_meta) = match payload["type"].as_str() {
        Some("message") => {
            let role = match payload["role"].as_str() {
                Some("assistant") => Role::Assistant,
                Some("developer") | Some("system") => Role::System,
                _ => Role::User,
            };
            let blocks = map_message_blocks(&payload["content"]);
            let is_meta = role == Role::System;
            (role, blocks, is_meta)
        }
        Some("reasoning") => {
            let summary_text = payload["summary"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|i| i["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let encrypted = payload.get("encrypted_content").is_some();
            if encrypted {
                *encrypted_reasoning += 1;
            }
            (
                Role::Assistant,
                vec![Block::Thinking {
                    text: summary_text,
                    encrypted,
                }],
                false,
            )
        }
        Some("function_call") | Some("custom_tool_call") => {
            let arguments_raw = payload["arguments"]
                .as_str()
                .or_else(|| payload["input"].as_str())
                .unwrap_or("");
            let arguments = serde_json::from_str::<Value>(arguments_raw)
                .unwrap_or_else(|_| Value::String(arguments_raw.to_string()));
            (
                Role::Assistant,
                vec![Block::ToolCall {
                    call_id: payload["call_id"].as_str().unwrap_or("").to_string(),
                    name: payload["name"].as_str().unwrap_or("").to_string(),
                    arguments,
                }],
                false,
            )
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            let output_raw = &payload["output"];
            let output = output_raw
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or_else(|| output_raw.clone());
            let is_error = output["metadata"]["exit_code"]
                .as_i64()
                .map(|code| code != 0)
                .unwrap_or(false);
            (
                Role::Tool,
                vec![Block::ToolResult {
                    call_id: payload["call_id"].as_str().unwrap_or("").to_string(),
                    output,
                    is_error,
                }],
                false,
            )
        }
        other => (
            Role::System,
            vec![Block::Meta {
                source_kind: format!("response_item/{}", other.unwrap_or("unknown")),
                raw: payload.clone(),
            }],
            true,
        ),
    };

    Some(Turn {
        id: turn_id,
        parent_id: None,
        role,
        timestamp,
        model: if role == Role::Assistant { model } else { None },
        is_meta,
        blocks,
        usage: None,
        raw: Some(raw.clone()),
    })
}

/// OpenAI message content: array of {type: input_text|output_text|text, text}.
fn map_message_blocks(content: &Value) -> Vec<Block> {
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
            Some("input_text") | Some("output_text") | Some("text") => Block::Text {
                text: item["text"].as_str().unwrap_or("").to_string(),
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
                // Codex wraps environment/context dumps in tags; skip those.
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

fn resolve_path(store_root: &Path, locator: &SessionLocator) -> Result<PathBuf> {
    if let Some(hint) = &locator.store_path {
        if hint.starts_with(store_root)
            && hint.is_file()
            && hint
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(&locator.native_id))
        {
            return Ok(hint.clone());
        }
    }
    let suffix = format!("-{}.jsonl", locator.native_id);
    walk_rollouts(store_root)
        .into_iter()
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
        })
        .ok_or_else(|| {
            PortalError::Other(format!(
                "rollout for session {} not found under {}",
                locator.native_id,
                store_root.display()
            ))
        })
}
