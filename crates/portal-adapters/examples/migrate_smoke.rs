//! REAL end-to-end migration: Claude Code session (this machine's store) ->
//! native Codex rollout, verified by read-back. Prints the resume command.
//!
//! Usage:
//!   cargo run -p portal-adapters --example migrate_smoke            # list candidates
//!   cargo run -p portal-adapters --example migrate_smoke <uuid>     # migrate one

use std::sync::Arc;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::migration::engine;
use portal_core::migration::ledger::Ledger;

fn main() {
    let env = HostEnv::from_system();
    let adapters = portal_adapters::builtin_adapters();
    let claude: Arc<dyn AgentAdapter> = adapters
        .iter()
        .find(|a| a.id() == "claude-code")
        .unwrap()
        .clone();
    let codex: Arc<dyn AgentAdapter> = adapters.iter().find(|a| a.id() == "codex").unwrap().clone();

    let claude_inst = claude.detect(&env).expect("claude not detected");
    let codex_inst = codex.detect(&env).expect("codex not detected");

    let arg = std::env::args().nth(1);
    let Some(session_id) = arg else {
        // List small, cold candidates.
        let mut all: Vec<_> = claude
            .snapshot(&claude_inst)
            .expect("snapshot")
            .into_iter()
            .flat_map(|(_, s)| s)
            .filter(|s| !s.maybe_live && s.message_count.unwrap_or(0) > 3)
            .collect();
        all.sort_by_key(|s| s.size_bytes);
        println!("smallest cold Claude sessions:");
        for s in all.iter().take(10) {
            println!(
                "  {} | {:>7} bytes | {} | {}",
                s.native_id,
                s.size_bytes,
                s.updated_at
                    .map(|t| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                s.title.as_deref().unwrap_or("(untitled)")
            );
        }
        return;
    };

    println!("planning migration of {session_id} ...");
    let planned = engine::plan_native(
        &claude,
        &claude_inst,
        &codex,
        &codex_inst,
        &SessionLocator {
            native_id: session_id,
            store_path: None,
        },
    )
    .expect("plan failed");

    println!("  title: {:?}", planned.report.source_title);
    println!("  cwd: {}", planned.report.cwd);
    println!("  turns: {}", planned.report.turn_count);
    println!("  census: {:?}", planned.report.census);
    println!(
        "  unanswered tool calls: {}",
        planned.report.unanswered_tool_calls
    );
    for l in &planned.report.predicted_losses {
        println!("  predicted loss [{:?}]: {}", l.code, l.detail);
    }
    for w in &planned.report.warnings {
        println!("  WARNING: {w}");
    }

    let ledger = Ledger::new(std::env::temp_dir().join("agent-portal-smoke"));
    let result = engine::execute(&planned, &codex, &codex_inst, &ledger).expect("execute failed");

    println!("\nMIGRATED OK");
    if let Some(v) = &result.verify {
        println!("  verify: {:?} over {} blocks", v.grade, v.compared_blocks);
    }
    println!("  target rollout: {}", result.target_path);
    println!("  new codex session id: {}", result.target_native_id);
    println!("  resume: {}", result.resume_command.display());
}
