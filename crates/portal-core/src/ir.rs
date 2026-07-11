//! Canonical session IR — the single representation every adapter reads into
//! and writes from. Preview rendering, migration, verification, and handoff
//! briefs all consume this and nothing agent-specific.
//!
//! Design rules:
//! - Timeline is the linearized active thread (Claude's DAG is walked back
//!   from the resume leaf; abandoned branches become loss notes).
//! - Nothing is silently dropped: unknown records become `Block::Meta` or a
//!   `LossNote`, and every turn can carry its raw source line for
//!   same-agent passthrough.
//! - Lossiness is explicit and travels with the session (`losses`,
//!   `fidelity`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub const IR_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalSession {
    pub ir_version: u32,
    pub identity: SessionIdentity,
    pub workspace: Workspace,
    pub title: Option<String>,
    pub timeline: Vec<Turn>,
    pub attachments: Vec<Attachment>,
    pub usage: UsageTotals,
    pub losses: Vec<LossNote>,
    pub fidelity: Fidelity,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentity {
    /// Portal-assigned, stable across migrations (uuid v7).
    pub portal_id: String,
    pub native_id: String,
    pub agent_id: String,
    pub store_path: String,
    pub agent_version: Option<String>,
    pub read_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub cwd: String,
    pub cwd_normalized: String,
    pub git_branch: Option<String>,
    pub project_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    /// Source-native id when the store has one; synthesized (`L<line>`) when not.
    pub id: String,
    pub parent_id: Option<String>,
    pub role: Role,
    pub timestamp: Option<DateTime<Utc>>,
    pub model: Option<String>,
    /// Harness noise (command caveats, mode records): hidden by default in
    /// preview, excluded from briefs and title extraction.
    pub is_meta: bool,
    pub blocks: Vec<Block>,
    pub usage: Option<TokenUsage>,
    /// Original source line(s), verbatim, for same-agent passthrough and
    /// debugging. Stripped before crossing IPC to the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        /// Provider-encrypted reasoning (e.g. Codex Fernet blobs): content is
        /// unavailable, transfer is impossible, and a LossNote records it.
        encrypted: bool,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        output: Value,
        is_error: bool,
    },
    /// Compaction boundary / conversation summary checkpoint.
    Compaction {
        summary: String,
    },
    /// Known-but-non-content or unknown record kinds, preserved verbatim.
    Meta {
        source_kind: String,
        raw: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    /// Subagent transcript (e.g. Claude's agent-*.jsonl). Recorded, not yet
    /// converted — conversion is a fidelity upgrade for later.
    Sidechain {
        id: String,
        file: String,
        size_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    #[ts(type = "number")]
    pub cache_read_tokens: u64,
    #[ts(type = "number")]
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    /// False when the source store had no usable usage records.
    pub known: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LossCode {
    EncryptedReasoning,
    SidechainNotConverted,
    AbandonedBranch,
    UnknownRecord,
    ToolPairingIncomplete,
    UsageUnavailable,
    /// Target format cannot represent thinking blocks; they are dropped.
    ThinkingDropped,
    /// Non-text content (images, unknown blocks) the target cannot represent.
    ContentSkipped,
    /// Tool call had no result (session interrupted mid-call); dropped so the
    /// written session satisfies the pairing invariant.
    InterruptedToolCall,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LossNote {
    pub code: LossCode,
    pub detail: String,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    Full,
    Partial,
    BriefOnly,
}

/// Canonical text form of a tool-result payload. Adapters store outputs
/// differently (a raw string, or structured JSON), and writers flatten them to
/// a string for target stores. Using this everywhere — readers, writers, and
/// verification — keeps a result's identity stable across a migration.
pub fn tool_output_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Sentinel key writers use to wrap a non-object tool argument (e.g. Codex's
/// `apply_patch` patch text, a raw string) so it satisfies stores whose
/// `tool_use` input must be a JSON object — without losing the value.
/// [`tool_args_text`] unwraps it so the wrapped form is identity-equal to the
/// original for verification.
pub const RAW_ARGS_KEY: &str = "__portal_raw";

/// Canonical text form of tool-call arguments, used for verification so an
/// object and its re-parsed twin hash identically (serde_json::Value orders
/// object keys deterministically). A single-key `RAW_ARGS_KEY` wrapper is
/// peeled first, so `"…patch…"` and `{"__portal_raw": "…patch…"}` agree.
pub fn tool_args_text(value: &Value) -> String {
    if let Value::Object(map) = value {
        if map.len() == 1 {
            if let Some(inner) = map.get(RAW_ARGS_KEY) {
                return tool_args_text(inner);
            }
        }
    }
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl CanonicalSession {
    /// Structural invariants every adapter's output must satisfy. Returns
    /// human-readable violations; empty = valid.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.timeline.is_empty() {
            issues.push("timeline is empty".to_string());
        }

        // Every ToolResult must reference an earlier ToolCall with the same
        // call_id — both Claude and Codex require coherent pairing to resume.
        let mut seen_calls: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for turn in &self.timeline {
            for block in &turn.blocks {
                match block {
                    Block::ToolCall { call_id, .. } => {
                        seen_calls.insert(call_id.as_str());
                    }
                    Block::ToolResult { call_id, .. } if !seen_calls.contains(call_id.as_str()) => {
                        issues.push(format!(
                            "turn {}: tool_result {} has no preceding tool_call",
                            turn.id, call_id
                        ));
                    }
                    _ => {}
                }
            }
        }

        issues
    }

    /// Unanswered tool calls (call_id present, no matching result). These are
    /// normal at a live session's tail but must be handled before migration.
    pub fn unanswered_tool_calls(&self) -> Vec<String> {
        let mut calls: indexmap_lite::OrderedSet = Default::default();
        for turn in &self.timeline {
            for block in &turn.blocks {
                match block {
                    Block::ToolCall { call_id, .. } => calls.insert(call_id.clone()),
                    Block::ToolResult { call_id, .. } => calls.remove(call_id),
                    _ => {}
                }
            }
        }
        calls.into_vec()
    }

    /// Strip raw passthrough payloads (before sending across IPC).
    pub fn without_raw(mut self) -> Self {
        for turn in &mut self.timeline {
            turn.raw = None;
        }
        self
    }
}

/// Tiny insertion-ordered set so unanswered_tool_calls reports in encounter
/// order without pulling in a dependency.
mod indexmap_lite {
    #[derive(Default)]
    pub struct OrderedSet {
        items: Vec<String>,
    }

    impl OrderedSet {
        pub fn insert(&mut self, value: String) {
            if !self.items.contains(&value) {
                self.items.push(value);
            }
        }
        pub fn remove(&mut self, value: &str) {
            self.items.retain(|v| v != value);
        }
        pub fn into_vec(self) -> Vec<String> {
            self.items
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_turn(id: &str, role: Role) -> Turn {
        Turn {
            id: id.to_string(),
            parent_id: None,
            role,
            timestamp: None,
            model: None,
            is_meta: false,
            blocks: vec![Block::Text {
                text: "hi".to_string(),
            }],
            usage: None,
            raw: None,
        }
    }

    fn base_session(timeline: Vec<Turn>) -> CanonicalSession {
        CanonicalSession {
            ir_version: IR_VERSION,
            identity: SessionIdentity {
                portal_id: "p".into(),
                native_id: "n".into(),
                agent_id: "a".into(),
                store_path: "s".into(),
                agent_version: None,
                read_at: Utc::now(),
            },
            workspace: Workspace {
                cwd: "P:\\x".into(),
                cwd_normalized: "p:/x".into(),
                git_branch: None,
                project_label: "x".into(),
            },
            title: None,
            timeline,
            attachments: vec![],
            usage: UsageTotals::default(),
            losses: vec![],
            fidelity: Fidelity::Full,
        }
    }

    #[test]
    fn orphan_tool_result_is_a_violation() {
        let mut turn = text_turn("t1", Role::User);
        turn.blocks = vec![Block::ToolResult {
            call_id: "c1".into(),
            output: serde_json::json!("out"),
            is_error: false,
        }];
        let session = base_session(vec![turn]);
        assert_eq!(session.validate().len(), 1);
    }

    #[test]
    fn paired_calls_validate_and_unanswered_are_reported() {
        let mut call = text_turn("t1", Role::Assistant);
        call.blocks = vec![
            Block::ToolCall {
                call_id: "c1".into(),
                name: "Read".into(),
                arguments: serde_json::json!({}),
            },
            Block::ToolCall {
                call_id: "c2".into(),
                name: "Bash".into(),
                arguments: serde_json::json!({}),
            },
        ];
        let mut result = text_turn("t2", Role::Tool);
        result.blocks = vec![Block::ToolResult {
            call_id: "c1".into(),
            output: serde_json::json!("ok"),
            is_error: false,
        }];
        let session = base_session(vec![call, result]);
        assert!(session.validate().is_empty());
        assert_eq!(session.unanswered_tool_calls(), vec!["c2".to_string()]);
    }

    #[test]
    fn raw_string_args_survive_object_wrapping() {
        // A non-object arg (e.g. apply_patch's patch text) that a writer had to
        // wrap under RAW_ARGS_KEY must hash identically to the raw source, so
        // read-back verification stays Exact instead of seeing a dropped `{}`.
        let patch = "*** Begin Patch\n*** Add File: a.py\n+print(1)\n*** End Patch";
        let raw = Value::String(patch.to_string());
        let wrapped = serde_json::json!({ RAW_ARGS_KEY: patch });
        assert_eq!(tool_args_text(&raw), tool_args_text(&wrapped));
        assert_eq!(tool_args_text(&wrapped), patch);

        // A genuine object argument is untouched (no accidental unwrapping).
        let obj = serde_json::json!({ "command": ["bash", "-c", "ls"] });
        assert_eq!(tool_args_text(&obj), obj.to_string());
        // A real single-key object that isn't the sentinel is left alone.
        let single = serde_json::json!({ "path": "src/main.rs" });
        assert_eq!(tool_args_text(&single), single.to_string());
    }
}
