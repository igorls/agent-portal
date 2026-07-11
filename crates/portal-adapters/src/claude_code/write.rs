//! Claude Code writer: CanonicalSession -> synthetic project JSONL that
//! `claude --resume <id>` picks up natively.
//!
//! The hard part is shape, not translation. Claude stores conversation in
//! Anthropic message form: an assistant message carries text + tool_use
//! blocks, and every tool_use MUST be answered by a tool_result in the very
//! next user message. Codex (and the IR generally) store a flatter stream, so
//! this writer *regroups* the block stream into that alternating shape rather
//! than mapping records one-to-one.
//!
//! Emission rules:
//! - a coherent parentUuid chain, one fresh v4 uuid per record
//! - tool ids are reminted to `toolu_…` (a source id like Codex's `call_1`
//!   isn't a shape Claude's API accepts on the next turn), consistently across
//!   each tool_use and its tool_result
//! - thinking blocks are dropped (their provider signatures can't be forged,
//!   and a bad signature breaks the next API call) — declared as a loss
//! - tool calls with no result (interrupted) are dropped so pairing stays valid

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};

use portal_core::dto::Installation;
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    tool_output_text, Block, CanonicalSession, LossCode, LossNote, Role, RAW_ARGS_KEY,
};
use portal_core::migration::types::{
    ArtifactKind, WriteOptions, WritePlan, WrittenArtifact, WrittenSession,
};
use portal_core::util::paths::{atomic_write, quick_hash};

use super::claude_slug;

pub fn plan_write(inst: &Installation, session: &CanonicalSession) -> Result<WritePlan> {
    let mut losses = Vec::new();

    let mut thinking = 0u32;
    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        for block in &turn.blocks {
            if matches!(block, Block::Thinking { .. }) {
                thinking += 1;
            }
        }
    }
    if thinking > 0 {
        losses.push(LossNote {
            code: LossCode::ThinkingDropped,
            detail: format!(
                "{thinking} thinking block(s) dropped — provider reasoning signatures can't be reconstructed"
            ),
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

    let slug = claude_slug(&session.workspace.cwd);
    let hint = PathBuf::from(&inst.store_root)
        .join(slug)
        .join("<new-session-id>.jsonl");

    Ok(WritePlan {
        predicted_losses: losses,
        target_path_hint: hint.display().to_string(),
    })
}

pub fn write_session(
    inst: &Installation,
    session: &CanonicalSession,
    _opts: &WriteOptions,
) -> Result<WrittenSession> {
    if session.workspace.cwd.is_empty() {
        return Err(PortalError::Other(
            "cannot write a Claude session without a working directory".to_string(),
        ));
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let slug = claude_slug(&session.workspace.cwd);
    let dir = PathBuf::from(&inst.store_root).join(&slug);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{session_id}.jsonl"));
    if path.exists() {
        return Err(PortalError::Other(format!(
            "target session already exists: {}",
            path.display()
        )));
    }

    let version = claude_semver(inst);
    let branch = session.workspace.git_branch.clone().unwrap_or_default();
    let now = Utc::now();

    let mut emitter = Emitter {
        records: Vec::new(),
        parent: Value::Null,
        session_id: &session_id,
        cwd: &session.workspace.cwd,
        version: &version,
        branch: &branch,
        fallback_ts: now,
    };

    let unanswered: std::collections::HashSet<String> =
        session.unanswered_tool_calls().into_iter().collect();
    let mut tool_ids: HashMap<String, String> = HashMap::new();
    let mut remap = |call_id: &str| -> String {
        tool_ids
            .entry(call_id.to_string())
            .or_insert_with(|| format!("toolu_{}", uuid::Uuid::new_v4().simple()))
            .clone()
    };

    // Regrouping state:
    //   assistant_buf — text + tool_use blocks awaiting a flush
    //   result_buf    — tool_result blocks awaiting a flush (as one user msg)
    let mut assistant_buf: Vec<Value> = Vec::new();
    let mut assistant_model: Option<String> = None;
    let mut assistant_ts: Option<DateTime<Utc>> = None;
    let mut result_buf: Vec<Value> = Vec::new();
    let mut result_ts: Option<DateTime<Utc>> = None;

    macro_rules! flush_assistant {
        () => {
            if !assistant_buf.is_empty() {
                emitter.push_assistant(
                    std::mem::take(&mut assistant_buf),
                    assistant_model.take(),
                    assistant_ts.take(),
                );
            }
        };
    }
    macro_rules! flush_results {
        () => {
            if !result_buf.is_empty() {
                emitter.push_user_blocks(std::mem::take(&mut result_buf), result_ts.take());
            }
        };
    }

    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        let assistant_turn = turn.role == Role::Assistant;
        for block in &turn.blocks {
            match block {
                Block::Thinking { .. } | Block::Compaction { .. } | Block::Meta { .. } => {}
                Block::Text { text } if text.trim().is_empty() => {}
                Block::Text { text } => {
                    if assistant_turn {
                        flush_results!();
                        assistant_buf.push(json!({ "type": "text", "text": text }));
                        assistant_model = assistant_model.or_else(|| turn.model.clone());
                        assistant_ts = assistant_ts.or(turn.timestamp);
                    } else {
                        flush_results!();
                        flush_assistant!();
                        emitter.push_user_text(text, turn.timestamp);
                    }
                }
                Block::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    if unanswered.contains(call_id) {
                        continue;
                    }
                    flush_results!();
                    let id = remap(call_id);
                    // Anthropic tool_use.input must be an object. Object args pass
                    // through; a non-object arg (e.g. apply_patch's patch string)
                    // is wrapped under RAW_ARGS_KEY so it's preserved, not dropped.
                    let input = if arguments.is_object() {
                        arguments.clone()
                    } else {
                        let mut m = serde_json::Map::new();
                        m.insert(RAW_ARGS_KEY.to_string(), arguments.clone());
                        Value::Object(m)
                    };
                    assistant_buf.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input,
                    }));
                    assistant_model = assistant_model.or_else(|| turn.model.clone());
                    assistant_ts = assistant_ts.or(turn.timestamp);
                }
                Block::ToolResult {
                    call_id,
                    output,
                    is_error,
                } => {
                    // The assistant message carrying this call must land right
                    // before its result.
                    flush_assistant!();
                    let id = remap(call_id);
                    result_buf.push(json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": tool_output_text(output),
                        "is_error": is_error,
                    }));
                    result_ts = result_ts.or(turn.timestamp);
                }
            }
        }
    }
    flush_results!();
    flush_assistant!();

    if emitter.records.is_empty() {
        return Err(PortalError::Other(
            "session has no transferable content".to_string(),
        ));
    }

    let mut content = emitter
        .records
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    content.push('\n');
    atomic_write(&path, content.as_bytes())?;

    let artifacts = vec![WrittenArtifact {
        kind: ArtifactKind::File,
        path: path.display().to_string(),
        backup: None,
        content_hash: Some(quick_hash(content.as_bytes())),
    }];

    Ok(WrittenSession {
        native_id: session_id,
        primary_path: path.display().to_string(),
        artifacts,
    })
}

struct Emitter<'a> {
    records: Vec<Value>,
    parent: Value,
    session_id: &'a str,
    cwd: &'a str,
    version: &'a str,
    branch: &'a str,
    fallback_ts: DateTime<Utc>,
}

impl Emitter<'_> {
    fn ts(&self, t: Option<DateTime<Utc>>) -> String {
        t.unwrap_or(self.fallback_ts)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    fn push(&mut self, kind: &str, message: Value, ts: Option<DateTime<Utc>>) {
        let uuid = uuid::Uuid::new_v4().to_string();
        self.records.push(json!({
            "parentUuid": self.parent,
            "isSidechain": false,
            "userType": "external",
            "isMeta": false,
            "type": kind,
            "message": message,
            "uuid": uuid,
            "timestamp": self.ts(ts),
            "sessionId": self.session_id,
            "cwd": self.cwd,
            "version": self.version,
            "gitBranch": self.branch,
        }));
        self.parent = Value::String(uuid);
    }

    fn push_user_text(&mut self, text: &str, ts: Option<DateTime<Utc>>) {
        self.push("user", json!({ "role": "user", "content": text }), ts);
    }

    fn push_user_blocks(&mut self, blocks: Vec<Value>, ts: Option<DateTime<Utc>>) {
        self.push("user", json!({ "role": "user", "content": blocks }), ts);
    }

    fn push_assistant(
        &mut self,
        blocks: Vec<Value>,
        model: Option<String>,
        ts: Option<DateTime<Utc>>,
    ) {
        let mut message = json!({ "role": "assistant", "content": blocks });
        if let Some(model) = model {
            message["model"] = Value::String(model);
        }
        self.push("assistant", message, ts);
    }
}

/// "2.1.206 (Claude Code)" -> "2.1.206".
fn claude_semver(inst: &Installation) -> String {
    inst.version
        .as_deref()
        .and_then(|v| v.split_whitespace().next())
        .unwrap_or("0.0.0")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use portal_core::ir::{Fidelity, SessionIdentity, Turn, UsageTotals, Workspace, IR_VERSION};
    use portal_core::migration::types::WriteOptions;

    fn turn(role: Role, block: Block) -> Turn {
        Turn {
            id: "t".into(),
            parent_id: None,
            role,
            timestamp: None,
            model: None,
            is_meta: false,
            blocks: vec![block],
            usage: None,
            raw: None,
        }
    }

    #[test]
    fn non_object_tool_args_are_preserved_not_dropped() {
        // Regression: Codex's apply_patch stores its patch as a raw (non-object)
        // string. The writer must wrap it, not replace it with `{}`, or the
        // whole patch is silently lost and read-back verify fails.
        let patch = "*** Begin Patch\n*** Add File: a.py\n+x = 1\n*** End Patch";
        let session = CanonicalSession {
            ir_version: IR_VERSION,
            identity: SessionIdentity {
                portal_id: "p".into(),
                native_id: "n".into(),
                agent_id: "codex".into(),
                store_path: String::new(),
                agent_version: None,
                read_at: Utc::now(),
            },
            workspace: Workspace {
                cwd: r"P:\some\project".into(),
                cwd_normalized: "p:/some/project".into(),
                git_branch: None,
                project_label: "project".into(),
            },
            title: None,
            timeline: vec![
                turn(
                    Role::Assistant,
                    Block::ToolCall {
                        call_id: "c1".into(),
                        name: "apply_patch".into(),
                        arguments: Value::String(patch.into()),
                    },
                ),
                turn(
                    Role::Tool,
                    Block::ToolResult {
                        call_id: "c1".into(),
                        output: Value::String("done".into()),
                        is_error: false,
                    },
                ),
            ],
            attachments: vec![],
            usage: UsageTotals::default(),
            losses: vec![],
            fidelity: Fidelity::Full,
        };

        let dir = std::env::temp_dir().join(format!("portal-w-{}", uuid::Uuid::new_v4().simple()));
        let inst = Installation {
            cli_path: None,
            version: Some("2.0.0".into()),
            store_root: dir.display().to_string(),
        };

        let written = write_session(&inst, &session, &WriteOptions::default()).unwrap();
        let content = std::fs::read_to_string(&written.primary_path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            content.contains(RAW_ARGS_KEY),
            "raw args should be wrapped, not dropped"
        );
        assert!(
            content.contains("Begin Patch"),
            "patch text must be preserved"
        );
        assert!(
            !content.contains(r#""input":{}"#),
            "args must not collapse to an empty object"
        );
    }
}
