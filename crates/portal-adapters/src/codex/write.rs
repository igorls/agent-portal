//! Codex writer: CanonicalSession -> synthetic rollout JSONL that
//! `codex resume <id>` picks up natively, plus a title line in
//! `~/.codex/session_index.jsonl`.
//!
//! Emission rules (kept to fields proven present in real rollouts):
//! - one `session_meta` header with a fresh UUIDv7 id and the source cwd
//! - user/assistant text -> `response_item` message records
//! - ToolCall/ToolResult -> `function_call`/`function_call_output` paired by
//!   the source call_id (any string is accepted)
//! - thinking, compaction, and meta blocks are dropped (declared in
//!   plan_write; Codex reasoning is provider-encrypted and cannot be forged)
//! - unanswered tool calls are dropped so pairing stays coherent

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use portal_core::dto::Installation;
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    tool_args_text, tool_output_text, Block, CanonicalSession, LossCode, LossNote, Role,
};
use portal_core::migration::types::{
    ArtifactKind, WriteOptions, WritePlan, WrittenArtifact, WrittenSession,
};
use portal_core::util::paths::{atomic_write, quick_hash};

pub fn plan_write(inst: &Installation, session: &CanonicalSession) -> Result<WritePlan> {
    let mut losses = Vec::new();

    let mut thinking = 0u32;
    let mut skipped = 0u32;
    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        for block in &turn.blocks {
            match block {
                Block::Thinking { .. } => thinking += 1,
                Block::Compaction { .. } | Block::Meta { .. } => skipped += 1,
                _ => {}
            }
        }
    }
    if thinking > 0 {
        losses.push(LossNote {
            code: LossCode::ThinkingDropped,
            detail: format!(
                "{thinking} thinking block(s) cannot be represented in a Codex rollout"
            ),
            turn_id: None,
        });
    }
    if skipped > 0 {
        losses.push(LossNote {
            code: LossCode::ContentSkipped,
            detail: format!("{skipped} meta/compaction block(s) skipped"),
            turn_id: None,
        });
    }
    let unanswered = session.unanswered_tool_calls();
    if !unanswered.is_empty() {
        losses.push(LossNote {
            code: LossCode::InterruptedToolCall,
            detail: format!(
                "{} tool call(s) had no result (interrupted) and will be dropped",
                unanswered.len()
            ),
            turn_id: None,
        });
    }

    let now = Utc::now();
    let hint = PathBuf::from(&inst.store_root)
        .join(now.format("%Y").to_string())
        .join(now.format("%m").to_string())
        .join(now.format("%d").to_string())
        .join(format!(
            "rollout-{}-<new-session-id>.jsonl",
            now.format("%Y-%m-%dT%H-%M-%S")
        ));

    Ok(WritePlan {
        predicted_losses: losses,
        target_path_hint: hint.display().to_string(),
    })
}

pub fn write_session(
    inst: &Installation,
    session: &CanonicalSession,
    opts: &WriteOptions,
) -> Result<WrittenSession> {
    let store_root = PathBuf::from(&inst.store_root);
    let id = uuid::Uuid::now_v7().to_string();
    let now = Utc::now();
    let now_iso = now.to_rfc3339_opts(SecondsFormat::Millis, true);

    let dir = store_root
        .join(now.format("%Y").to_string())
        .join(now.format("%m").to_string())
        .join(now.format("%d").to_string());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "rollout-{}-{}.jsonl",
        now.format("%Y-%m-%dT%H-%M-%S"),
        id
    ));
    if path.exists() {
        return Err(PortalError::Other(format!(
            "target rollout already exists: {}",
            path.display()
        )));
    }

    let mut lines: Vec<String> = Vec::new();
    let push = |lines: &mut Vec<String>, value: Value| {
        lines.push(value.to_string());
    };

    push(
        &mut lines,
        json!({
            "timestamp": now_iso,
            "type": "session_meta",
            "payload": {
                "id": id,
                "session_id": id,
                "timestamp": now_iso,
                "cwd": session.workspace.cwd,
                "originator": "agent_portal",
                "cli_version": cli_semver(inst),
                "source": "cli",
                "model_provider": "openai"
            }
        }),
    );

    let unanswered: HashSet<String> = session.unanswered_tool_calls().into_iter().collect();

    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        let ts = turn
            .timestamp
            .unwrap_or(now)
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let role = match turn.role {
            Role::Assistant => "assistant",
            Role::System => continue,
            _ => "user",
        };
        let text_kind = if turn.role == Role::Assistant {
            "output_text"
        } else {
            "input_text"
        };

        let mut text_buffer: Vec<&str> = Vec::new();
        let flush = |lines: &mut Vec<String>, buffer: &mut Vec<&str>| {
            let joined = buffer.join("\n\n");
            if !joined.trim().is_empty() {
                lines.push(
                    json!({
                        "timestamp": ts,
                        "type": "response_item",
                        "payload": {
                            "type": "message",
                            "role": role,
                            "content": [{"type": text_kind, "text": joined}]
                        }
                    })
                    .to_string(),
                );
            }
            buffer.clear();
        };

        for block in &turn.blocks {
            match block {
                Block::Text { text } => text_buffer.push(text),
                Block::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    if unanswered.contains(call_id) {
                        continue;
                    }
                    flush(&mut lines, &mut text_buffer);
                    let arguments_string = tool_args_text(arguments);
                    push(
                        &mut lines,
                        json!({
                            "timestamp": ts,
                            "type": "response_item",
                            "payload": {
                                "type": "function_call",
                                "name": name,
                                "arguments": arguments_string,
                                "call_id": call_id
                            }
                        }),
                    );
                }
                Block::ToolResult {
                    call_id, output, ..
                } => {
                    flush(&mut lines, &mut text_buffer);
                    let output_string = tool_output_text(output);
                    push(
                        &mut lines,
                        json!({
                            "timestamp": ts,
                            "type": "response_item",
                            "payload": {
                                "type": "function_call_output",
                                "call_id": call_id,
                                "output": output_string
                            }
                        }),
                    );
                }
                Block::Thinking { .. } | Block::Compaction { .. } | Block::Meta { .. } => {}
            }
        }
        flush(&mut lines, &mut text_buffer);
    }

    let mut content = lines.join("\n");
    content.push('\n');
    atomic_write(&path, content.as_bytes())?;

    let mut artifacts = vec![WrittenArtifact {
        kind: ArtifactKind::File,
        path: path.display().to_string(),
        backup: None,
        content_hash: Some(quick_hash(content.as_bytes())),
    }];

    // Title registration in ~/.codex/session_index.jsonl (sibling of the
    // sessions dir). Backed up first; single appended line.
    if let Some(codex_home) = store_root.parent() {
        let index_path = codex_home.join("session_index.jsonl");
        let title = opts
            .title
            .clone()
            .unwrap_or_else(|| format!("Migrated from {}", session.identity.agent_id));
        let index_line = json!({
            "id": id,
            "thread_name": title,
            "updated_at": now_iso
        })
        .to_string();

        if index_path.exists() {
            let backup = codex_home.join("session_index.jsonl.portal-bak");
            std::fs::copy(&index_path, &backup)?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().append(true).open(&index_path)?;
            file.write_all(format!("{index_line}\n").as_bytes())?;
            file.sync_all()?;
            artifacts.push(WrittenArtifact {
                kind: ArtifactKind::IndexAppendLine,
                path: index_path.display().to_string(),
                backup: Some(backup.display().to_string()),
                content_hash: None,
            });
        } else {
            let line = format!("{index_line}\n");
            atomic_write(&index_path, line.as_bytes())?;
            artifacts.push(WrittenArtifact {
                kind: ArtifactKind::File,
                path: index_path.display().to_string(),
                backup: None,
                content_hash: Some(quick_hash(line.as_bytes())),
            });
        }
    }

    Ok(WrittenSession {
        native_id: id,
        primary_path: path.display().to_string(),
        artifacts,
    })
}

/// "codex-cli 0.143.0" -> "0.143.0"; falls back to a neutral version.
fn cli_semver(inst: &Installation) -> String {
    inst.version
        .as_deref()
        .and_then(|v| {
            v.split_whitespace()
                .find(|tok| tok.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .unwrap_or("0.0.0")
        .to_string()
}
