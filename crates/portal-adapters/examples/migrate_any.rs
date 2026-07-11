//! Generic REAL migration between any two installed agents, verified by
//! read-back. Prints the resume command.
//!
//!   cargo run -p portal-adapters --example migrate_any <src> <dst>        # candidates
//!   cargo run -p portal-adapters --example migrate_any <src> <dst> <id>   # migrate one

use std::sync::Arc;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::migration::engine;
use portal_core::migration::ledger::Ledger;

fn main() {
    let mut args = std::env::args().skip(1);
    let (src, dst) = match (args.next(), args.next()) {
        (Some(s), Some(d)) => (s, d),
        _ => {
            eprintln!("usage: migrate_any <src-agent> <dst-agent> [session-id]");
            return;
        }
    };
    let session_id = args.next();

    let env = HostEnv::from_system();
    let adapters = portal_adapters::builtin_adapters();
    let find = |id: &str| -> Arc<dyn AgentAdapter> {
        adapters
            .iter()
            .find(|a| a.id() == id)
            .cloned()
            .unwrap_or_else(|| {
                panic!("unknown agent '{id}'");
            })
    };
    let source = find(&src);
    let target = find(&dst);
    let source_inst = source.detect(&env).expect("source not detected");
    let target_inst = target.detect(&env).expect("target not detected");

    let Some(id) = session_id else {
        let mut all: Vec<_> = source
            .snapshot(&source_inst)
            .expect("snapshot")
            .into_iter()
            .flat_map(|(_, s)| s)
            .filter(|s| !s.maybe_live && s.message_count.unwrap_or(0) > 3)
            .collect();
        all.sort_by_key(|s| s.message_count.unwrap_or(0));
        println!("smallest {src} sessions:");
        for s in all.iter().take(10) {
            println!(
                "  {} | {} msgs | {} | {}",
                s.native_id,
                s.message_count.unwrap_or(0),
                s.cwd.as_deref().unwrap_or("?"),
                s.title.as_deref().unwrap_or("(untitled)")
            );
        }
        return;
    };

    println!("planning {src} -> {dst} of {id} ...");
    let planned = engine::plan_native(
        &source,
        &source_inst,
        &target,
        &target_inst,
        &SessionLocator {
            native_id: id,
            store_path: None,
        },
    )
    .expect("plan");
    println!("  title: {:?}", planned.report.source_title);
    println!(
        "  cwd: {} | turns: {}",
        planned.report.cwd, planned.report.turn_count
    );
    println!("  census: {:?}", planned.report.census);
    for l in &planned.report.predicted_losses {
        println!("  loss [{:?}]: {}", l.code, l.detail);
    }

    let ledger = Ledger::new(std::env::temp_dir().join("agent-portal-smoke"));
    let result = engine::execute(&planned, &target, &target_inst, &ledger).expect("execute");
    println!("\nMIGRATED OK");
    if let Some(v) = &result.verify {
        println!("  verify: {:?} over {} blocks", v.grade, v.compared_blocks);
    }
    println!("  target: {}", result.target_path);
    println!("  new id: {}", result.target_native_id);
    println!("  resume: {}", result.resume_command.display());
}
