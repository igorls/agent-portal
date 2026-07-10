//! REAL end-to-end reverse migration: Codex session (this machine's store) ->
//! native Claude Code session, verified by read-back. Prints the resume cmd.
//!
//!   cargo run -p portal-adapters --example migrate_x2c            # candidates
//!   cargo run -p portal-adapters --example migrate_x2c <uuid>     # migrate one

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

    let Some(session_id) = std::env::args().nth(1) else {
        let mut all: Vec<_> = codex
            .snapshot(&codex_inst)
            .expect("snapshot")
            .into_iter()
            .flat_map(|(_, s)| s)
            .filter(|s| !s.maybe_live && s.message_count.unwrap_or(0) > 3)
            .collect();
        all.sort_by_key(|s| s.size_bytes);
        println!("smallest cold Codex sessions:");
        for s in all.iter().take(10) {
            println!(
                "  {} | {:>7} bytes | {} | {}",
                s.native_id,
                s.size_bytes,
                s.cwd.as_deref().unwrap_or("?"),
                s.title.as_deref().unwrap_or("(untitled)")
            );
        }
        return;
    };

    println!("planning Codex→Claude migration of {session_id} ...");
    let planned = engine::plan_native(
        &codex,
        &codex_inst,
        &claude,
        &claude_inst,
        &SessionLocator {
            native_id: session_id,
            store_path: None,
        },
    )
    .expect("plan failed");

    println!("  title: {:?}", planned.report.source_title);
    println!("  cwd: {}", planned.report.cwd);
    println!(
        "  turns: {} | census {:?}",
        planned.report.turn_count, planned.report.census
    );
    for l in &planned.report.predicted_losses {
        println!("  predicted loss [{:?}]: {}", l.code, l.detail);
    }

    let ledger = Ledger::new(std::env::temp_dir().join("agent-portal-smoke"));
    let result = engine::execute(&planned, &claude, &claude_inst, &ledger).expect("execute failed");

    println!("\nMIGRATED OK");
    if let Some(v) = &result.verify {
        println!("  verify: {:?} over {} blocks", v.grade, v.compared_blocks);
    }
    println!("  target file: {}", result.target_path);
    println!("  new claude session id: {}", result.target_native_id);
    println!("  resume: {}", result.resume_command.display());
}
