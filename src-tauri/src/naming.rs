//! Background, local-only session naming.

use std::sync::Arc;
use std::time::Duration;

use portal_core::adapter::SessionLocator;
use portal_core::ir::{tool_args_text, Block};
use portal_core::migration::ollama;
use tauri::Emitter;

use crate::state::AppState;

pub fn start(state: Arc<AppState>, app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("session-naming".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(8));
            loop {
                run_pass(&state);
                let _ = app.emit("titles-updated", ());
                std::thread::sleep(Duration::from_secs(300));
            }
        })
        .expect("spawn session naming worker");
}

fn run_pass(state: &AppState) {
    let status = ollama::status(ollama::DEFAULT_BASE_URL);
    if !status.default_present {
        return;
    }
    let board = state.registry.board(&state.env);
    let mut candidates = board
        .lanes
        .iter()
        .flat_map(|l| l.projects.iter())
        .flat_map(|p| p.sessions.iter())
        .map(|s| {
            (
                state.index.title_refresh_priority(
                    &s.agent_id,
                    &s.native_id,
                    &portal_core::index::session_revision(s),
                ),
                s,
            )
        })
        .filter(|(priority, _)| *priority < 2)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, s)| std::cmp::Reverse(s.updated_at));
    let mut never_named = candidates.iter().filter(|(p, _)| *p == 0).map(|(_, s)| *s);
    let mut stale = candidates.iter().filter(|(p, _)| *p == 1).map(|(_, s)| *s);
    let mut batch = Vec::with_capacity(4);
    batch.extend(stale.by_ref().take(2));
    batch.extend(never_named.by_ref().take(2));
    batch.extend(stale.chain(never_named).take(4 - batch.len()));
    for summary in batch {
        let Some(adapter) = state.adapter(&summary.agent_id) else {
            continue;
        };
        let Some(inst) = state.installation(&summary.agent_id) else {
            continue;
        };
        let Ok(session) = adapter.read_session(
            &inst,
            &SessionLocator {
                native_id: summary.native_id.clone(),
                store_path: Some(summary.store_path.clone().into()),
            },
        ) else {
            continue;
        };
        let activity = recent_activity(&session);
        if let Some(title) =
            ollama::title(ollama::DEFAULT_BASE_URL, ollama::DEFAULT_MODEL, &activity)
        {
            let _ = state.index.upsert_generated_title(
                &summary.agent_id,
                &summary.native_id,
                &title,
                &portal_core::index::session_revision(summary),
            );
        }
    }
    let mut refreshed = state.registry.board(&state.env);
    let _ = state.index.apply_generated_titles(&mut refreshed);
    let _ = state.index.store_board(&refreshed);
}

fn recent_activity(session: &portal_core::ir::CanonicalSession) -> String {
    let mut parts = Vec::new();
    let mut turns: Vec<_> = session
        .timeline
        .iter()
        .rev()
        .filter(|t| !t.is_meta)
        .take(12)
        .collect();
    turns.reverse();
    for turn in turns {
        for block in &turn.blocks {
            let text = match block {
                Block::Text { text } => text.clone(),
                Block::ToolCall {
                    name, arguments, ..
                } => format!("tool {name}: {}", tool_args_text(arguments)),
                Block::ToolResult {
                    output, is_error, ..
                } => format!(
                    "tool result{}: {}",
                    if *is_error { " error" } else { "" },
                    portal_core::ir::tool_output_text(output)
                ),
                Block::Compaction { summary } => format!("prior summary: {summary}"),
                _ => continue,
            };
            parts.push(format!(
                "{:?}: {}",
                turn.role,
                text.chars().take(700).collect::<String>()
            ));
        }
    }
    let joined = parts.join("\n");
    joined
        .chars()
        .rev()
        .take(6000)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn recent_activity_prefers_tail() {
        use portal_core::ir::*;
        let mut session = CanonicalSession {
            ir_version: IR_VERSION,
            identity: SessionIdentity {
                portal_id: "p".into(),
                native_id: "n".into(),
                agent_id: "a".into(),
                store_path: "s".into(),
                agent_version: None,
                read_at: chrono::Utc::now(),
            },
            workspace: Workspace {
                cwd: String::new(),
                cwd_normalized: String::new(),
                git_branch: None,
                project_label: String::new(),
            },
            title: None,
            timeline: vec![],
            attachments: vec![],
            usage: UsageTotals::default(),
            losses: vec![],
            fidelity: Fidelity::Full,
        };
        for i in 0..20 {
            session.timeline.push(portal_core::ir::Turn {
                id: i.to_string(),
                parent_id: None,
                role: portal_core::ir::Role::User,
                timestamp: None,
                model: None,
                is_meta: false,
                blocks: vec![portal_core::ir::Block::Text {
                    text: format!("work-{i}"),
                }],
                usage: None,
                raw: None,
            });
        }
        let text = super::recent_activity(&session);
        assert!(!text.contains("work-0"));
        assert!(text.contains("work-19"));
    }
}
