//! Dev smoke: fully parse recent REAL sessions from each installed agent into
//! the canonical IR and print structure stats + validation results.

use portal_core::adapter::{HostEnv, SessionLocator};
use portal_core::ir::{Block, Role};
use portal_core::registry::AgentRegistry;

fn main() {
    let env = HostEnv::from_system();
    let registry = AgentRegistry::new(portal_adapters::builtin_adapters());

    let only = std::env::args().nth(1);
    for adapter in registry.adapters() {
        if let Some(only) = &only {
            if adapter.id() != only {
                continue;
            }
        }
        let Some(inst) = adapter.detect(&env) else {
            continue;
        };
        println!("\n=== {} ===", adapter.display_name());

        // Take the 3 most recently updated sessions across the store.
        let mut sessions: Vec<_> = adapter
            .snapshot(&inst)
            .map(|snap| snap.into_iter().flat_map(|(_, s)| s).collect())
            .unwrap_or_default();
        sessions
            .sort_by_key(|s: &portal_core::dto::SessionSummary| std::cmp::Reverse(s.updated_at));

        for summary in sessions.iter().take(3) {
            let started = std::time::Instant::now();
            let result = adapter.read_session(
                &inst,
                &SessionLocator {
                    native_id: summary.native_id.clone(),
                    store_path: Some(summary.store_path.clone().into()),
                },
            );
            match result {
                Ok(session) => {
                    let issues = session.validate();
                    let mut text = 0;
                    let mut thinking = 0;
                    let mut calls = 0;
                    let mut results = 0;
                    let mut meta_blocks = 0;
                    for turn in &session.timeline {
                        for block in &turn.blocks {
                            match block {
                                Block::Text { .. } => text += 1,
                                Block::Thinking { .. } => thinking += 1,
                                Block::ToolCall { .. } => calls += 1,
                                Block::ToolResult { .. } => results += 1,
                                _ => meta_blocks += 1,
                            }
                        }
                    }
                    let users = session
                        .timeline
                        .iter()
                        .filter(|t| t.role == Role::User && !t.is_meta)
                        .count();
                    println!(
                        "  {} | {} turns ({} user) | blocks: {}txt {}think {}call {}result {}meta | {} losses | unanswered_calls={} | validate: {} | {:?}",
                        &summary.native_id[..8],
                        session.timeline.len(),
                        users,
                        text,
                        thinking,
                        calls,
                        results,
                        meta_blocks,
                        session.losses.len(),
                        session.unanswered_tool_calls().len(),
                        if issues.is_empty() {
                            "OK".to_string()
                        } else {
                            format!("{} ISSUES: {}", issues.len(), issues.join("; "))
                        },
                        started.elapsed(),
                    );
                    for loss in &session.losses {
                        println!("      loss[{:?}]: {}", loss.code, loss.detail);
                    }
                }
                Err(e) => println!("  {} | READ FAILED: {e}", &summary.native_id[..8]),
            }
        }
    }
}
