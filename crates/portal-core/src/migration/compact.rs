//! Canonical-session compaction used by compacted native migrations.

use crate::ir::{Block, CanonicalSession, Role, Turn};

/// Replace the historical prefix with one canonical compaction turn while
/// retaining the most recent `keep_turns` verbatim.
pub fn compact_session(
    mut session: CanonicalSession,
    keep_turns: usize,
    summary: String,
) -> CanonicalSession {
    if session.timeline.len() <= keep_turns {
        return session;
    }
    let split = split_index(&session, keep_turns);
    let mut tail = session.timeline.split_off(split);
    let boundary = Turn {
        id: format!("portal-compaction-{}", uuid::Uuid::now_v7()),
        parent_id: None,
        // User text is the common portable representation: Codex deliberately
        // drops system turns, while both native writers preserve user text.
        role: Role::User,
        timestamp: Some(chrono::Utc::now()),
        model: None,
        is_meta: false,
        blocks: vec![Block::Text {
            text: format!("[Agent Portal compacted earlier history]\n\n{summary}"),
        }],
        usage: None,
        raw: None,
    };
    session.timeline = vec![boundary];
    session.timeline.append(&mut tail);
    session
}

/// Resolve the actual user boundary once so the summary prefix and retained
/// tail cannot overlap.
pub fn split_index(session: &CanonicalSession, keep_turns: usize) -> usize {
    let mut split = session.timeline.len().saturating_sub(keep_turns.max(1));
    while split > 0 && session.timeline[split].role != Role::User {
        split -= 1;
    }
    split
}

/// A conservative local fallback that captures the old prefix without model
/// dependency. It is intentionally extractive so it cannot invent facts.
pub fn deterministic_summary(session: &CanonicalSession, prefix_turns: usize) -> String {
    let mut out = String::from("Earlier session history (compacted by Agent Portal):\n");
    for turn in session
        .timeline
        .iter()
        .take(prefix_turns)
        .filter(|t| !t.is_meta)
    {
        for block in &turn.blocks {
            let text = match block {
                Block::Text { text } => text.clone(),
                Block::ToolCall {
                    name, arguments, ..
                } => {
                    format!("tool {name}: {}", crate::ir::tool_args_text(arguments))
                }
                Block::ToolResult {
                    output, is_error, ..
                } => format!(
                    "tool result{}: {}",
                    if *is_error { " error" } else { "" },
                    crate::ir::tool_output_text(output)
                ),
                Block::Compaction { summary } => format!("prior compaction: {summary}"),
                Block::Thinking { .. } | Block::Meta { .. } => continue,
            };
            let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !clean.is_empty() {
                let excerpt: String = clean.chars().take(600).collect();
                let line = format!("- {:?}: {excerpt}\n", turn.role);
                if out.len() + line.len() > 24_000 {
                    out.push_str(
                        "- [remaining earlier activity omitted from deterministic fallback]\n",
                    );
                    return out;
                }
                out.push_str(&line);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::ir::{
        CanonicalSession, Fidelity, Role, SessionIdentity, Turn, UsageTotals, Workspace, IR_VERSION,
    };

    fn session(turns: usize) -> CanonicalSession {
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
                cwd: "P:\\repo".into(),
                cwd_normalized: "p:/repo".into(),
                git_branch: None,
                project_label: "repo".into(),
            },
            title: Some("old title".into()),
            timeline: (0..turns)
                .map(|i| Turn {
                    id: format!("t{i}"),
                    parent_id: None,
                    role: if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    timestamp: None,
                    model: None,
                    is_meta: false,
                    blocks: vec![crate::ir::Block::Text {
                        text: format!("turn {i}"),
                    }],
                    usage: None,
                    raw: None,
                })
                .collect(),
            attachments: vec![],
            usage: UsageTotals::default(),
            losses: vec![],
            fidelity: Fidelity::Full,
        }
    }

    #[test]
    fn compaction_replaces_old_history_and_keeps_recent_tail() {
        let compacted = super::compact_session(session(12), 4, "summary of earlier work".into());
        assert_eq!(compacted.timeline.len(), 5);
        assert!(
            matches!(&compacted.timeline[0].blocks[0], crate::ir::Block::Text { text } if text.contains("summary of earlier work"))
        );
        assert_eq!(compacted.timeline[1].id, "t8");
        assert_eq!(compacted.timeline[4].id, "t11");
        assert!(compacted.validate().is_empty());
    }
}
