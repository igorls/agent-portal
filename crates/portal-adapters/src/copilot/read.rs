//! VS Code Copilot Chat event-log -> IR.
//!
//! Each `chatSessions/<id>.jsonl` is an append-only event log, not a snapshot.
//! Three event kinds rebuild the session object:
//!
//! - `kind:0` — the initial snapshot (`v` is the base session object);
//! - `kind:1` — set the value `v` at path `k` (a mixed key/index pointer);
//! - `kind:2` — append the array `v` to the array at path `k`.
//!
//! Replaying them in order reconstructs `requests[]`, including response parts
//! that were streamed in after a request was first appended
//! (`kind:2 ["requests", N, "response"]`).
//!
//! A request is one user message plus an ordered `response[]` of parts: plain
//! markdown (assistant prose), `thinking` (readable reasoning), and
//! `toolInvocationSerialized` (tool activity). Tool call/result structure isn't
//! cleanly recoverable from the combined serialization, so tools are preserved
//! as text — fidelity is Partial, which is honest for a read/brief source.

use std::path::Path;

use chrono::{TimeZone, Utc};
use serde_json::Value;

use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::ID;

/// One path segment of an event's `k` pointer: an object key or an array index.
enum Seg {
    Key(String),
    Idx(usize),
}

fn parse_path(k: &Value) -> Vec<Seg> {
    k.as_array()
        .map(|a| {
            a.iter()
                .map(|s| match s.as_u64() {
                    Some(n) => Seg::Idx(n as usize),
                    None => Seg::Key(s.as_str().unwrap_or_default().to_string()),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Descend to a mutable ref at `path`, creating missing object keys as `Null`
/// along the way. Returns None if the path crosses a non-object/non-array.
fn nav_create<'a>(mut cur: &'a mut Value, path: &[Seg]) -> Option<&'a mut Value> {
    for seg in path {
        cur = match seg {
            Seg::Key(k) => {
                if !cur.is_object() {
                    return None;
                }
                cur.as_object_mut()
                    .unwrap()
                    .entry(k.clone())
                    .or_insert(Value::Null)
            }
            Seg::Idx(i) => cur.as_array_mut()?.get_mut(*i)?,
        };
    }
    Some(cur)
}

fn apply_set(root: &mut Value, path: &[Seg], val: Value) {
    let Some((last, parent_path)) = path.split_last() else {
        *root = val;
        return;
    };
    let Some(parent) = nav_create(root, parent_path) else {
        return;
    };
    match last {
        Seg::Key(k) => {
            if let Some(o) = parent.as_object_mut() {
                o.insert(k.clone(), val);
            }
        }
        Seg::Idx(i) => {
            if let Some(a) = parent.as_array_mut() {
                if *i < a.len() {
                    a[*i] = val;
                } else if *i == a.len() {
                    a.push(val);
                }
            }
        }
    }
}

fn apply_append(root: &mut Value, path: &[Seg], val: Value) {
    let items = match val {
        Value::Array(a) => a,
        other => vec![other],
    };
    match nav_create(root, path) {
        Some(Value::Array(arr)) => arr.extend(items),
        Some(slot @ Value::Null) => *slot = Value::Array(items),
        _ => {}
    }
}

/// Replay the event log into the final session object. Malformed lines are
/// skipped rather than aborting the read.
pub fn replay(text: &str) -> Value {
    let mut session = Value::Null;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match ev.get("kind").and_then(Value::as_u64) {
            Some(0) => session = ev.get("v").cloned().unwrap_or(Value::Null),
            Some(1) => {
                let path = parse_path(ev.get("k").unwrap_or(&Value::Null));
                apply_set(&mut session, &path, ev.get("v").cloned().unwrap_or(Value::Null));
            }
            Some(2) => {
                let path = parse_path(ev.get("k").unwrap_or(&Value::Null));
                apply_append(&mut session, &path, ev.get("v").cloned().unwrap_or(Value::Null));
            }
            _ => {}
        }
    }
    session
}

pub fn read_session(path: &Path, id: &str, cwd: Option<String>) -> Result<CanonicalSession> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| PortalError::Other(format!("read copilot session: {e}")))?;
    let session = replay(&text);

    let empty = Vec::new();
    let requests = session
        .get("requests")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let mut timeline = Vec::new();
    let mut tool_parts = 0usize;
    let mut dropped = 0usize;

    for (ri, req) in requests.iter().enumerate() {
        let ts = req
            .get("timestamp")
            .and_then(Value::as_i64)
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single());

        // User message.
        if let Some(t) = req.pointer("/message/text").and_then(Value::as_str) {
            let t = t.trim();
            if !t.is_empty() {
                timeline.push(Turn {
                    id: format!("r{ri}u"),
                    parent_id: None,
                    role: Role::User,
                    timestamp: ts,
                    model: None,
                    is_meta: false,
                    blocks: vec![Block::Text { text: t.to_string() }],
                    usage: None,
                    raw: None,
                });
            }
        }

        // Assistant response: an ordered list of parts.
        let mut blocks = Vec::new();
        if let Some(parts) = req.get("response").and_then(Value::as_array) {
            for part in parts {
                match part.get("kind").and_then(Value::as_str) {
                    // A MarkdownString part has no `kind`; its `value` is the prose.
                    None => match part.get("value").and_then(Value::as_str) {
                        Some(s) if !s.trim().is_empty() => blocks.push(Block::Text {
                            text: s.trim().to_string(),
                        }),
                        _ => dropped += 1,
                    },
                    Some("thinking") => {
                        if let Some(s) = part.get("value").and_then(Value::as_str) {
                            if !s.trim().is_empty() {
                                blocks.push(Block::Thinking {
                                    text: s.trim().to_string(),
                                    encrypted: false,
                                });
                            }
                        }
                    }
                    Some("toolInvocationSerialized") => {
                        tool_parts += 1;
                        let msg = part
                            .pointer("/invocationMessage/value")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        let tool = part.get("toolId").and_then(Value::as_str).unwrap_or("tool");
                        blocks.push(Block::Text {
                            text: match msg {
                                Some(m) => format!("⚙ {m}"),
                                None => format!("⚙ {tool}"),
                            },
                        });
                    }
                    // edit groups, references, code-block markers, server pings.
                    Some(_) => dropped += 1,
                }
            }
        }
        if !blocks.is_empty() {
            let model = req.get("modelId").and_then(Value::as_str).map(str::to_string);
            timeline.push(Turn {
                id: format!("r{ri}a"),
                parent_id: None,
                role: Role::Assistant,
                timestamp: ts,
                model,
                is_meta: false,
                blocks,
                usage: None,
                raw: None,
            });
        }
    }

    let title = session
        .get("customTitle")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|t| !t.is_empty())
        .or_else(|| {
            timeline
                .iter()
                .find(|t| t.role == Role::User)
                .and_then(|t| t.blocks.first())
                .and_then(|b| match b {
                    Block::Text { text } => Some(truncate(text, 80)),
                    _ => None,
                })
        });

    let mut losses = Vec::new();
    if tool_parts > 0 {
        losses.push(LossNote {
            code: LossCode::ToolPairingIncomplete,
            detail: format!("{tool_parts} tool invocation(s) preserved as text"),
            turn_id: None,
        });
    }
    if dropped > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{dropped} response part(s) had no extractable text (edits/references/markers)"),
            turn_id: None,
        });
    }

    let cwd = cwd.unwrap_or_default();
    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: id.to_string(),
            agent_id: ID.to_string(),
            store_path: path.display().to_string(),
            agent_version: None,
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
        usage: UsageTotals::default(),
        losses,
        fidelity: Fidelity::Partial,
    })
}

/// Cheap card metadata without building the IR: request count (one `requestId`
/// per request), plus title/model/created pulled from the small marker events.
pub struct Peek {
    pub title: Option<String>,
    pub model: Option<String>,
    pub request_count: u32,
    pub created_ms: Option<i64>,
}

pub fn peek(path: &Path) -> Peek {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Peek {
            title: None,
            model: None,
            request_count: 0,
            created_ms: None,
        };
    };
    let request_count = text.matches("\"requestId\"").count() as u32;

    let mut title = None;
    let mut model = None;
    let mut created_ms = None;
    for line in text.lines() {
        // Only the small marker events are worth parsing; skip the big request
        // and response-append lines entirely.
        if line.len() > 4096 {
            continue;
        }
        if created_ms.is_none() && line.contains("\"kind\":0") {
            if let Ok(ev) = serde_json::from_str::<Value>(line) {
                created_ms = ev.pointer("/v/creationDate").and_then(Value::as_i64);
            }
        }
        if title.is_none() && line.contains("\"customTitle\"") {
            if let Ok(ev) = serde_json::from_str::<Value>(line) {
                title = ev
                    .get("v")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|t| !t.is_empty());
            }
        }
        if model.is_none() && line.contains("selectedModel") {
            if let Ok(ev) = serde_json::from_str::<Value>(line) {
                model = ev
                    .pointer("/v/metadata/id")
                    .or_else(|| ev.pointer("/v/identifier"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
    }
    Peek {
        title,
        model,
        request_count,
        created_ms,
    }
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    let mut out: String = t.chars().take(max).collect();
    if t.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_snapshot_set_and_nested_append() {
        // kind:0 base with empty requests; kind:1 sets a title; kind:2 appends a
        // request with a partial response; kind:2 streams one more response part
        // into requests[0].response.
        let log = [
            r#"{"kind":0,"v":{"customTitle":null,"requests":[]}}"#,
            r#"{"kind":1,"k":["customTitle"],"v":"Fix the parser"}"#,
            r#"{"kind":2,"k":["requests"],"v":[{"requestId":"a","message":{"text":"hi"},"response":[{"value":"hello there"}]}]}"#,
            r#"{"kind":2,"k":["requests",0,"response"],"v":[{"kind":"thinking","value":"pondering"}]}"#,
        ]
        .join("\n");
        let s = replay(&log);
        assert_eq!(s.pointer("/customTitle").and_then(Value::as_str), Some("Fix the parser"));
        let resp = s.pointer("/requests/0/response").and_then(Value::as_array).unwrap();
        assert_eq!(resp.len(), 2); // initial markdown + streamed thinking
    }

    #[test]
    fn builds_ir_with_thinking_and_tool_text() {
        let log = [
            r#"{"kind":0,"v":{"requests":[]}}"#,
            r#"{"kind":2,"k":["requests"],"v":[{"requestId":"a","timestamp":1782008720582,"modelId":"glm-5.2","message":{"text":"study the app"},"response":[{"kind":"thinking","value":"let me look"},{"value":"Here is the plan"},{"kind":"toolInvocationSerialized","toolId":"copilot_readFile","invocationMessage":{"value":"Reading style.css"}},{"kind":"undoStop"}]}]}"#,
        ]
        .join("\n");
        let s = replay(&log);
        let requests = s.get("requests").and_then(Value::as_array).unwrap();
        assert_eq!(requests.len(), 1);

        // Rebuild via read_session by writing to a temp file.
        let dir = std::env::temp_dir();
        let f = dir.join("copilot_read_test.jsonl");
        std::fs::write(&f, &log).unwrap();
        let ir = read_session(&f, "a", Some(r"P:\afterpic".to_string())).unwrap();
        let _ = std::fs::remove_file(&f);

        assert_eq!(ir.title.as_deref(), Some("study the app"));
        assert_eq!(ir.timeline.len(), 2); // user + assistant
        let asst = &ir.timeline[1];
        assert_eq!(asst.role, Role::Assistant);
        assert_eq!(asst.model.as_deref(), Some("glm-5.2"));
        // thinking + markdown + tool-as-text = 3 blocks; undoStop dropped.
        assert_eq!(asst.blocks.len(), 3);
        assert!(matches!(asst.blocks[0], Block::Thinking { .. }));
        assert!(ir.losses.iter().any(|l| l.code == LossCode::ToolPairingIncomplete));
        assert!(ir.losses.iter().any(|l| l.code == LossCode::UnknownRecord));
    }
}
