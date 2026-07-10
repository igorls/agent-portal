//! Dev smoke: assemble the real board from this machine's stores and print a
//! compact summary. `cargo run -p portal-adapters --example board_smoke --release`

use portal_core::adapter::HostEnv;
use portal_core::registry::AgentRegistry;

fn main() {
    let env = HostEnv::from_system();
    let registry = AgentRegistry::new(portal_adapters::builtin_adapters());

    let started = std::time::Instant::now();
    let board = registry.board(&env);
    let elapsed = started.elapsed();

    for lane in &board.lanes {
        let agent = &lane.agent;
        match &agent.installation {
            Some(inst) => println!(
                "\n=== {} [{}] cli={} version={}\n    store={}",
                agent.display_name,
                agent.id,
                inst.cli_path.as_deref().unwrap_or("-"),
                inst.version.as_deref().unwrap_or("?"),
                inst.store_root
            ),
            None => {
                println!("\n=== {} [{}] NOT DETECTED", agent.display_name, agent.id);
                continue;
            }
        }
        if let Some(err) = &lane.error {
            println!("    LANE ERROR: {err}");
        }
        let total: usize = lane.projects.iter().map(|p| p.sessions.len()).sum();
        println!("    {} projects, {} sessions", lane.projects.len(), total);
        for project in lane.projects.iter().take(4) {
            println!(
                "    - {} ({}) [{} sessions]",
                project.label,
                project.cwd_normalized.as_deref().unwrap_or("?"),
                project.sessions.len()
            );
            for s in project.sessions.iter().take(2) {
                println!(
                    "        · {} | {} | msgs={}{} | {} | model={}",
                    &s.native_id[..8.min(s.native_id.len())],
                    s.title.as_deref().unwrap_or("(untitled)"),
                    s.message_count
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".into()),
                    if s.message_count_exact { "" } else { "~" },
                    s.updated_at
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "?".into()),
                    s.model.as_deref().unwrap_or("?")
                );
            }
        }
    }

    println!("\nboard assembled in {elapsed:?}");
}
