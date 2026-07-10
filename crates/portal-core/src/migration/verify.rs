//! Read-back verification: after writing a session into the target store, the
//! target adapter re-reads it and we compare *normalized* content streams.
//!
//! Normalization is deliberately blind to everything a conversion may
//! legitimately change — thinking blocks, compaction, meta records, turn
//! grouping, role reclassification of tool results, and the opaque tool-call
//! id strings (each store mints its own). What it compares is the content that
//! must survive: the ordered stream of user/assistant text, tool calls
//! (by name + arguments), and tool results (by output). Any mismatch here is a
//! real defect and fails the migration. Structural pairing is enforced
//! separately by `CanonicalSession::validate`.

use crate::ir::{tool_args_text, tool_output_text, Block, CanonicalSession, Role};
use crate::migration::types::{VerifyGrade, VerifyReport};

#[derive(Debug, PartialEq, Eq, Clone)]
enum NormBlock {
    /// user/assistant text, hashed. Role collapses to the human/machine axis.
    Text(bool, u64), // is_assistant, hash
    /// a tool call, identified by name + arguments (never the opaque id)
    Call(u64),
    /// a tool result, identified by its output text + error flag
    Result(u64),
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.trim().hash(&mut h);
    h.finish()
}

fn normalize(session: &CanonicalSession, drop_calls: &[String]) -> Vec<NormBlock> {
    let mut out = Vec::new();
    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        let is_assistant = turn.role == Role::Assistant;
        for block in &turn.blocks {
            match block {
                Block::Text { text } if !text.trim().is_empty() => {
                    out.push(NormBlock::Text(is_assistant, hash(text)));
                }
                Block::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    if drop_calls.contains(call_id) {
                        continue;
                    }
                    out.push(NormBlock::Call(hash(&format!(
                        "{name}\u{1}{}",
                        tool_args_text(arguments)
                    ))));
                }
                Block::ToolResult {
                    output, is_error, ..
                } => {
                    out.push(NormBlock::Result(hash(&format!(
                        "{}\u{1}{is_error}",
                        tool_output_text(output)
                    ))));
                }
                _ => {}
            }
        }
    }
    out
}

pub fn compare(source: &CanonicalSession, written: &CanonicalSession) -> VerifyReport {
    // Unanswered calls are legitimately dropped by writers (they'd break tool
    // pairing in the target), so exclude them from the source side too.
    let dropped = source.unanswered_tool_calls();
    let a = normalize(source, &dropped);
    let b = normalize(written, &[]);

    let mut diffs = Vec::new();
    if a.len() != b.len() {
        diffs.push(format!(
            "content block count differs: source {} vs written {}",
            a.len(),
            b.len()
        ));
    }
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if x != y {
            diffs.push(format!("content block {i} differs after migration"));
            if diffs.len() >= 6 {
                diffs.push("… further differences suppressed".to_string());
                break;
            }
        }
    }

    VerifyReport {
        grade: if diffs.is_empty() {
            VerifyGrade::Exact
        } else {
            VerifyGrade::Failed
        },
        compared_blocks: a.len() as u32,
        diffs,
    }
}
