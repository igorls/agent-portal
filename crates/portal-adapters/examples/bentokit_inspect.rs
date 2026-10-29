//! Read-only compatibility check; prints counts, never transcript contents.
//! cargo run -p portal-adapters --example bentokit_inspect -- [data-directory]

use portal_adapters::bentokit::BentoKitAdapter;
use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut env = HostEnv::from_system();
    if let Some(root) = std::env::args_os().nth(1) {
        env.store_overrides.insert("bentokit".into(), root.into());
    }
    let adapter = BentoKitAdapter;
    let inst = adapter.detect(&env).ok_or("BentoKit not detected")?;
    let snapshot = adapter.snapshot(&inst)?;
    let mut sessions = 0;
    let mut turns = 0;
    let mut tools = 0;
    for (_, summaries) in &snapshot {
        for summary in summaries {
            let session = adapter.read_session(
                &inst,
                &SessionLocator {
                    native_id: summary.native_id.clone(),
                    store_path: Some(summary.store_path.clone().into()),
                },
            )?;
            let issues = session.validate();
            if !issues.is_empty() {
                return Err(format!("BentoKit IR validation failed: {}", issues.join("; ")).into());
            }
            sessions += 1;
            turns += session.timeline.len();
            tools += session
                .timeline
                .iter()
                .flat_map(|t| &t.blocks)
                .filter(|b| matches!(b, portal_core::ir::Block::ToolCall { .. }))
                .count();
        }
    }
    println!("BentoKit read check passed: {} project groups, {sessions} sessions, {turns} turns, {tools} tools", snapshot.len());
    Ok(())
}
