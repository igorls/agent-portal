//! Background, local-only session naming.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use portal_core::adapter::SessionLocator;
use portal_core::ir::{tool_args_text, Block};
use portal_core::migration::ollama;
use tauri::Emitter;

use crate::state::AppState;

/// Sessions touched within this window are the focus: they get named first,
/// and while any remain unnamed the worker runs at the fast cadence.
pub const RECENT_WINDOW_HOURS: i64 = 24;
/// Cadence while there is recent (last-24h) work still to name.
const FAST_INTERVAL_SECS: i64 = 30;
/// Cadence once the recent window is fully named — just a slow heartbeat that
/// catches older sessions and edits drifting in.
const IDLE_INTERVAL_SECS: i64 = 300;
const WARMUP_SECS: i64 = 8;
const BATCH: usize = 4;

pub fn start(state: Arc<AppState>, app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("session-naming".into())
        .spawn(move || {
            let first = Utc::now() + chrono::Duration::seconds(WARMUP_SECS);
            emit_progress(&state, &app, |p| p.next_pass_at = Some(first));
            std::thread::sleep(Duration::from_secs(WARMUP_SECS as u64));
            loop {
                let recent_pending = run_pass(&state, &app);
                // Fast while the last 24h still has unnamed work; relax otherwise.
                let interval = if recent_pending > 0 {
                    FAST_INTERVAL_SECS
                } else {
                    IDLE_INTERVAL_SECS
                };
                let next = Utc::now() + chrono::Duration::seconds(interval);
                emit_progress(&state, &app, |p| {
                    p.active = false;
                    p.current = None;
                    p.next_pass_at = Some(next);
                });
                let _ = app.emit("titles-updated", ());
                std::thread::sleep(Duration::from_secs(interval as u64));
            }
        })
        .expect("spawn session naming worker");
}

/// Mutate the shared worker progress and push it to the UI in one step.
fn emit_progress(
    state: &AppState,
    app: &tauri::AppHandle,
    f: impl FnOnce(&mut portal_core::dto::NamingProgress),
) {
    let snapshot = state.update_naming_progress(f);
    let _ = app.emit("naming-progress", snapshot);
}

/// Run one naming pass. Returns the number of sessions in the recency window
/// that still lack a current title — the loop uses it to choose its cadence.
fn run_pass(state: &AppState, app: &tauri::AppHandle) -> u32 {
    let settings = state.settings.load();
    let status = ollama::status(&settings.ollama_host);
    if !status.models.iter().any(|m| m == &settings.ollama_model) {
        // No model → nothing to run; leave the worker marked idle.
        emit_progress(state, app, |p| {
            p.active = false;
            p.current = None;
            p.current_project = None;
        });
        return 0;
    }
    let recent_cutoff = Utc::now() - chrono::Duration::hours(RECENT_WINDOW_HOURS);
    let board = state.registry.board(&state.env);
    let mut candidates = board
        .lanes
        .iter()
        .flat_map(|l| l.projects.iter())
        // Carry each session's project label so the UI can show where the work is.
        .flat_map(|p| p.sessions.iter().map(move |s| (p.label.as_str(), s)))
        .map(|(project, s)| {
            (
                state.index.title_refresh_priority(
                    &s.agent_id,
                    &s.native_id,
                    &portal_core::index::session_revision(s),
                ),
                project,
                s,
            )
        })
        .filter(|(priority, _, _)| *priority < 2)
        .collect::<Vec<_>>();
    // Recency-first: the most recently touched sessions are named before older
    // ones, so the last 24h always drains first.
    candidates.sort_by_key(|(_, _, s)| std::cmp::Reverse(s.updated_at));
    let batch: Vec<_> = candidates
        .iter()
        .map(|(_, project, s)| (project.to_string(), *s))
        .take(BATCH)
        .collect();

    let batch_total = batch.len() as u32;
    emit_progress(state, app, |p| {
        p.active = batch_total > 0;
        p.current = None;
        p.current_project = None;
        p.batch_done = 0;
        p.batch_total = batch_total;
    });

    for (project, summary) in batch {
        // Show what is being named this instant: its existing title, else a
        // short id, plus its project. This is the "what's going on" the UI shows.
        let label = summary.title.clone().unwrap_or_else(|| {
            let id: String = summary.native_id.chars().take(8).collect();
            format!("{} · {id}", summary.agent_id)
        });
        emit_progress(state, app, |p| {
            p.current = Some(label);
            p.current_project = Some(project.clone());
        });

        let named = (|| {
            let adapter = state.adapter(&summary.agent_id)?;
            let inst = state.installation(&summary.agent_id)?;
            let session = adapter
                .read_session(
                    &inst,
                    &SessionLocator {
                        native_id: summary.native_id.clone(),
                        store_path: Some(summary.store_path.clone().into()),
                    },
                )
                .ok()?;
            let activity = recent_activity(&session);
            let title = ollama::title(&settings.ollama_host, &settings.ollama_model, &activity)?;
            state
                .index
                .upsert_generated_title(
                    &summary.agent_id,
                    &summary.native_id,
                    &title,
                    &portal_core::index::session_revision(summary),
                )
                .ok()
        })();
        let _ = named;

        emit_progress(state, app, |p| {
            p.batch_done += 1;
            p.current = None;
            p.current_project = None;
        });
    }

    let mut refreshed = state.registry.board(&state.env);
    let _ = state.index.apply_generated_titles(&mut refreshed);
    let _ = state.index.store_board(&refreshed);

    // Recount recent-window sessions still needing a title, off the freshly
    // stored board, to drive both the cadence and the UI badge.
    let recent_pending = refreshed
        .lanes
        .iter()
        .flat_map(|l| l.projects.iter())
        .flat_map(|p| p.sessions.iter())
        .filter(|s| s.updated_at.is_some_and(|t| t >= recent_cutoff))
        .filter(|s| {
            state.index.title_refresh_priority(
                &s.agent_id,
                &s.native_id,
                &portal_core::index::session_revision(s),
            ) < 2
        })
        .count() as u32;

    emit_progress(state, app, |p| {
        p.active = false;
        p.current = None;
        p.current_project = None;
        p.last_pass_at = Some(Utc::now());
    });
    recent_pending
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
