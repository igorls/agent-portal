//! Generate a handoff brief from a real local session — deterministic, and
//! (if reachable) enriched by local Ollama — and print both.
//!
//!   cargo run -p portal-adapters --example brief_smoke <claude-session-uuid>

use std::sync::Arc;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::migration::{brief, ollama};

fn main() {
    let env = HostEnv::from_system();
    let adapters = portal_adapters::builtin_adapters();
    let claude: Arc<dyn AgentAdapter> = adapters
        .iter()
        .find(|a| a.id() == "claude-code")
        .unwrap()
        .clone();
    let inst = claude.detect(&env).expect("claude not detected");

    let Some(id) = std::env::args().nth(1) else {
        let mut all: Vec<_> = claude
            .snapshot(&inst)
            .unwrap()
            .into_iter()
            .flat_map(|(_, s)| s)
            .filter(|s| !s.maybe_live && s.message_count.unwrap_or(0) > 5)
            .collect();
        all.sort_by_key(|s| s.size_bytes);
        println!("small cold candidates:");
        for s in all.iter().take(8) {
            println!(
                "  {} | {}",
                s.native_id,
                s.title.as_deref().unwrap_or("(untitled)")
            );
        }
        return;
    };

    let session = claude
        .read_session(
            &inst,
            &SessionLocator {
                native_id: id,
                store_path: None,
            },
        )
        .expect("read session");
    let facts = brief::extract_facts(&session);
    let deterministic = brief::render(&facts);

    println!("========== DETERMINISTIC BRIEF ==========\n");
    println!("{deterministic}");

    let status = ollama::status(ollama::DEFAULT_BASE_URL);
    println!("\n========== OLLAMA ==========");
    println!(
        "available={} default={} present={}",
        status.available, status.default_model, status.default_present
    );
    if status.default_present {
        println!("\nenriching with {} …\n", status.default_model);
        match ollama::enrich(&status.base_url, &status.default_model, &deterministic) {
            Some(enriched) => {
                println!("========== ENRICHED BRIEF ==========\n");
                println!("{enriched}");
            }
            None => println!("(enrichment failed — deterministic brief stands)"),
        }
    }
}
