//! Background FTS content indexer (independent of Ollama).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use portal_core::adapter::SessionLocator;
use portal_core::dto::SessionSummary;
use portal_core::index::session_revision;
use portal_core::search::{extract_indexable_text, ExtractOptions};

use crate::state::AppState;

const WARMUP_SECS: u64 = 5;
const FAST_INTERVAL_SECS: u64 = 12;
const IDLE_INTERVAL_SECS: u64 = 90;
const BATCH: usize = 3;
/// Soft shared budget with naming: keep full reads modest per pass.
const MAX_READS_PER_PASS: usize = 4;

pub fn start(state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("session-fts-index".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(WARMUP_SECS));
            // Bring WormDB sidecar up early so STATUS can report HNSW-capable backend.
            // Semantic mem_* indexing waits on embeddings (next).
            if let Err(e) = state.ensure_wormdb() {
                eprintln!("wormdb sidecar not started: {e}");
            } else if let Some(port) = state.wormdb.as_ref().and_then(|s| s.port()) {
                eprintln!("wormdb sidecar ready on 127.0.0.1:{port}");
            }
            loop {
                let pending = run_pass(&state);
                let interval = if pending > 0 {
                    FAST_INTERVAL_SECS
                } else {
                    IDLE_INTERVAL_SECS
                };
                std::thread::sleep(Duration::from_secs(interval));
            }
        })
        .expect("spawn fts indexer");
}

/// One indexing pass. Returns sessions still needing (re)index in inventory.
fn run_pass(state: &AppState) -> u32 {
    let Some(store) = state.search.as_ref() else {
        return 0;
    };
    let Some(board) = state.hot_board() else {
        return 0;
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    for lane in &board.lanes {
        let Some(adapter) = state.adapter(&lane.agent.id) else {
            continue;
        };
        if adapter.capabilities().read == portal_core::dto::SupportLevel::None {
            continue;
        }
        for project in &lane.projects {
            for s in &project.sessions {
                let rev = session_revision(s);
                let need = match store.indexed_revision(&s.agent_id, &s.native_id) {
                    Ok(None) => true,
                    Ok(Some(r)) if r != rev => {
                        // Live: only first index; reindex when settled.
                        !s.maybe_live
                    }
                    Ok(Some(_)) => false,
                    Err(_) => true,
                };
                if need {
                    candidates.push(Candidate {
                        summary: s.clone(),
                        project_key: project.key.clone(),
                        project_label: project.label.clone(),
                        cwd_normalized: project.cwd_normalized.clone(),
                        revision: rev,
                        updated_ms: s.updated_at.map(|t| t.timestamp_millis()).unwrap_or(0),
                    });
                }
            }
        }
    }

    // Newest first, then never-indexed (empty revision check already done).
    candidates.sort_by_key(|b| std::cmp::Reverse(b.updated_ms));
    let pending_total = candidates.len() as u32;

    let mut reads = 0usize;
    for c in candidates.into_iter().take(BATCH.max(MAX_READS_PER_PASS)) {
        if reads >= MAX_READS_PER_PASS {
            break;
        }
        if index_one(state, store, &c) {
            reads += 1;
        }
    }

    // Orphan cleanup occasionally (cheap).
    if Utc::now().timestamp() % 7 == 0 {
        cleanup_orphans(state, store, &board);
    }

    pending_total.saturating_sub(reads as u32)
}

struct Candidate {
    summary: SessionSummary,
    project_key: String,
    project_label: String,
    cwd_normalized: Option<String>,
    revision: String,
    updated_ms: i64,
}

fn index_one(state: &AppState, store: &portal_core::search::SearchStore, c: &Candidate) -> bool {
    let Some(adapter) = state.adapter(&c.summary.agent_id) else {
        return false;
    };
    let Some(inst) = state.installation(&c.summary.agent_id) else {
        return false;
    };
    let Ok(session) = adapter.read_session(
        &inst,
        &SessionLocator {
            native_id: c.summary.native_id.clone(),
            store_path: Some(c.summary.store_path.clone().into()),
        },
    ) else {
        return false;
    };
    let body = extract_indexable_text(&session, &ExtractOptions::default());
    // Still index empty bodies so we don't retry forever.
    if let Err(e) = store.upsert_session(
        &c.summary,
        &c.project_key,
        &c.project_label,
        c.cwd_normalized.as_deref(),
        &c.revision,
        &body,
    ) {
        eprintln!("fts index upsert failed: {e}");
        return false;
    }
    true
}

fn cleanup_orphans(
    _state: &AppState,
    store: &portal_core::search::SearchStore,
    board: &portal_core::dto::BoardSnapshot,
) {
    let Ok(keys) = store.all_keys() else {
        return;
    };
    let mut live = std::collections::HashSet::new();
    for lane in &board.lanes {
        for project in &lane.projects {
            for s in &project.sessions {
                live.insert((s.agent_id.clone(), s.native_id.clone()));
            }
        }
    }
    for (agent, native) in keys {
        if !live.contains(&(agent.clone(), native.clone())) {
            let _ = store.delete_session(&agent, &native);
        }
    }
}
