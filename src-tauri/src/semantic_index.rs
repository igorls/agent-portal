//! Background WormDB `mem_*` indexer (settings-driven embedder).

use std::sync::Arc;
use std::time::{Duration, Instant};

use portal_core::adapter::SessionLocator;
use portal_core::dto::SessionSummary;
use portal_core::index::session_revision;
use portal_core::ir::Role;
use portal_core::search::{
    build_meta_json, chunk_doc_id, chunk_for_embed, embedder_from_settings, Embedder,
    ExtractOptions, MemClient, MEMORY_NAMESPACE,
};
use portal_core::settings::AppSettings;
use serde::Serialize;

use crate::state::AppState;

const WARMUP_SECS: u64 = 8;
/// While work remains, re-enter quickly (batch embeds are the real throttle).
const FAST_INTERVAL_SECS: u64 = 1;
const IDLE_INTERVAL_SECS: u64 = 45;
/// Sessions per pass — higher now that embeds are batched per session.
const BATCH: usize = 8;
const MAX_READS: usize = 8;
/// Cap text sent to Ollama so embeds stay snappy.
const MAX_EMBED_CHARS: usize = 4000;
/// Texts per `/api/embed` request (Ollama multi-input).
const EMBED_BATCH_SIZE: usize = 16;
/// Max chunks per session for discovery (title + users first from extract).
const MAX_CHUNKS_PER_SESSION: usize = 12;
const META_EMBEDDER_KEY: &str = "semantic_embedder_id";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticProgress {
    pub active: bool,
    pub current: Option<String>,
    pub current_project: Option<String>,
    pub sessions_done_pass: u32,
    pub sessions_pending: u32,
    pub last_error: Option<String>,
    pub embedder_id: Option<String>,
    /// User paused embedding (search still works on what is already indexed).
    pub paused: bool,
    /// Estimated seconds remaining for the pending queue (None until rate is known).
    pub eta_secs: Option<u32>,
    /// Recent throughput used for ETA (sessions per minute).
    pub rate_sessions_per_min: Option<f32>,
    /// Sessions successfully indexed in the current rate window (since resume/reset).
    pub sessions_done_window: u32,
}

/// Rolling throughput for ETA: sessions completed since window start.
#[derive(Debug, Default)]
struct RateTracker {
    started: Option<Instant>,
    completed: u32,
    /// After pause, reset window on next successful embed so ETA is not frozen stale.
    need_reset: bool,
}

impl RateTracker {
    fn on_resume_or_reset(&mut self) {
        self.started = None;
        self.completed = 0;
        self.need_reset = false;
    }

    fn on_pause(&mut self) {
        self.need_reset = true;
    }

    fn record_success(&mut self) {
        if self.need_reset {
            self.on_resume_or_reset();
        }
        if self.started.is_none() {
            self.started = Some(Instant::now());
        }
        self.completed = self.completed.saturating_add(1);
    }

    /// Returns (eta_secs, sessions_per_min) once we have enough signal.
    fn estimate(&self, pending: u32) -> Option<(u32, f32)> {
        let start = self.started?;
        let elapsed = start.elapsed().as_secs_f32();
        // Need a few sessions and a few seconds so Ollama cold-start does not skew ETA.
        if self.completed < 3 || elapsed < 5.0 {
            return None;
        }
        let per_sec = self.completed as f32 / elapsed;
        if per_sec <= f32::EPSILON {
            return None;
        }
        let eta = (pending as f32 / per_sec).ceil().max(0.0) as u32;
        // Cap display runaway (e.g. 99h) so UI stays sane.
        let eta = eta.min(99 * 3600);
        let per_min = per_sec * 60.0;
        Some((eta, per_min))
    }

    fn apply_to_progress(&self, p: &mut SemanticProgress, pending: u32) {
        p.sessions_done_window = self.completed;
        p.sessions_pending = pending;
        match self.estimate(pending) {
            Some((eta, rate)) => {
                p.eta_secs = Some(eta);
                p.rate_sessions_per_min = Some(rate);
            }
            None => {
                // Keep last ETA while warming up if we already had one mid-run.
                if self.completed == 0 || pending == 0 {
                    p.eta_secs = None;
                    p.rate_sessions_per_min = None;
                }
            }
        }
        if pending == 0 {
            p.eta_secs = None;
            p.rate_sessions_per_min = None;
        }
    }
}

pub fn start(state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("session-semantic-index".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(WARMUP_SECS));
            let mut last_embedder_id = String::new();
            let mut rate = RateTracker::default();
            let mut was_paused = false;
            loop {
                // Pause: do not embed; still respond to resume kick quickly.
                if state.semantic_paused() {
                    if !was_paused {
                        rate.on_pause();
                        was_paused = true;
                    }
                    let pending = count_pending(&state);
                    state.set_semantic_progress(|p| {
                        p.active = false;
                        p.paused = true;
                        p.current = Some("paused".into());
                        p.sessions_pending = pending;
                        // Freeze last ETA while paused (still useful context).
                        rate.apply_to_progress(p, pending);
                    });
                    sleep_interruptible(&state, IDLE_INTERVAL_SECS);
                    continue;
                }
                if was_paused {
                    rate.on_resume_or_reset();
                    was_paused = false;
                }

                let settings = state.settings.load();
                let embedder = embedder_from_settings(&settings);
                let emb_id = embedder.id().to_string();

                // Auto-migrate when settings embedder changes (even if paused was off).
                if emb_id != last_embedder_id {
                    if let Err(e) = force_reset(&state, embedder.as_ref()) {
                        eprintln!("semantic embedder change: {e}");
                        state.set_semantic_progress(|p| {
                            p.active = false;
                            p.last_error = Some(e);
                        });
                        std::thread::sleep(Duration::from_secs(IDLE_INTERVAL_SECS));
                        continue;
                    }
                    last_embedder_id = emb_id.clone();
                    rate.on_resume_or_reset();
                }

                if let Err(e) = ensure_namespace(&state, embedder.as_ref()) {
                    eprintln!("semantic mem_init: {e}");
                    state.set_semantic_progress(|p| {
                        p.active = false;
                        p.last_error = Some(e);
                        p.embedder_id = Some(emb_id.clone());
                    });
                    std::thread::sleep(Duration::from_secs(IDLE_INTERVAL_SECS));
                    continue;
                }

                let pending = run_pass(&state, embedder.as_ref(), &mut rate);
                state.set_semantic_progress(|p| {
                    p.paused = state.semantic_paused();
                    p.active = pending > 0 && !p.paused;
                    if pending == 0 && !p.paused {
                        p.current = None;
                        p.current_project = None;
                        rate.on_resume_or_reset();
                    }
                    p.embedder_id = Some(emb_id.clone());
                    rate.apply_to_progress(p, pending);
                    // Keep last_error if set during pass; clear when idle healthy.
                    if pending == 0 && p.last_error.is_none() {
                        p.last_error = None;
                    }
                });
                // Interruptible idle so "Clear & reindex" / resume is not stuck for 45s.
                let interval = if pending > 0 {
                    FAST_INTERVAL_SECS
                } else {
                    IDLE_INTERVAL_SECS
                };
                sleep_interruptible(&state, interval);
            }
        })
        .expect("spawn semantic indexer");
}

fn sleep_interruptible(state: &AppState, secs: u64) {
    for _ in 0..(secs * 2) {
        if state.take_semantic_kick() {
            return;
        }
        // Wake sooner when user toggles pause/resume.
        if state.semantic_paused() {
            std::thread::sleep(Duration::from_millis(250));
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Always clear WormDB memory namespace + local semantic_state, then re-init.
/// Used on embedder change **and** explicit "Clear & reindex" (must not no-op).
pub fn reset_semantic_index(state: &AppState, settings: &AppSettings) -> Result<(), String> {
    let embedder = embedder_from_settings(settings);
    force_reset(state, embedder.as_ref())?;
    // Reindex implies we want work to run — unpause.
    state.set_semantic_paused(false);
    state.set_semantic_progress(|p| {
        p.active = true;
        p.paused = false;
        p.current = Some("reindex queued…".into());
        p.sessions_done_pass = 0;
        p.sessions_pending = 0;
        p.last_error = None;
        p.embedder_id = Some(embedder.id().to_string());
        p.eta_secs = None;
        p.rate_sessions_per_min = None;
        p.sessions_done_window = 0;
    });
    // Wake the worker if it is in a long idle sleep.
    state.kick_semantic_indexer();
    Ok(())
}

fn force_reset(state: &AppState, embedder: &dyn Embedder) -> Result<(), String> {
    let new_id = embedder.id();
    eprintln!("semantic: force reset → embedder {new_id} (clear WormDB ns + semantic_state)");

    // Cold start can take 10–30s while WormDB replays WAL + rebuilds HNSW.
    state.set_semantic_progress(|p| {
        p.active = true;
        p.current = Some("starting WormDB sidecar (WAL replay may take a while)…".into());
        p.last_error = None;
    });
    let port = state.ensure_wormdb().map_err(|e| {
        format!(
            "WormDB sidecar failed to start: {e}. \
             If the process crashed (Windows 0xC0000005), check that wormdb.exe \
             is next to the app, allow it in antivirus, and prefer the NSIS \
             setup if the MSI install is incomplete. \
             Sidecar stderr is under the app data wormdb/ folder. \
             Override with PORTAL_WORMDB only for local debugging."
        )
    })?;
    let client = MemClient::connect(port)
        .map_err(|e| format!("WormDB listening on :{port} but STATUS/connect failed: {e}"))?;
    match client.drop_namespace() {
        Ok(msg) => eprintln!("semantic mem_drop: {msg}"),
        Err(e) => eprintln!("semantic mem_drop failed (continuing): {e}"),
    }
    client
        .init(new_id, "cosine")
        .map_err(|e| format!("mem_init after reset: {e}"))?;

    if let Some(store) = state.search.as_ref() {
        let n = store.clear_semantic_state().unwrap_or(0);
        eprintln!("semantic_state cleared ({n} rows)");
        let _ = store.set_meta(META_EMBEDDER_KEY, new_id);
    }
    Ok(())
}

fn ensure_namespace(state: &AppState, embedder: &dyn Embedder) -> Result<(), String> {
    let port = state.ensure_wormdb().map_err(|e| e.to_string())?;
    let client = MemClient::connect(port).map_err(|e| e.to_string())?;
    client
        .init(embedder.id(), "cosine")
        .map_err(|e| e.to_string())?;
    if let Some(store) = state.search.as_ref() {
        let _ = store.set_meta(META_EMBEDDER_KEY, embedder.id());
    }
    if let Ok(st) = client.stats() {
        eprintln!(
            "semantic ns {MEMORY_NAMESPACE}: vectors={} docs={} dim={} embedder={}",
            st.vectors,
            st.doc_keys,
            st.dimensions,
            embedder.id()
        );
    }
    Ok(())
}

fn run_pass(state: &AppState, embedder: &dyn Embedder, rate: &mut RateTracker) -> u32 {
    if state.semantic_paused() {
        return count_pending(state);
    }
    let Some(store) = state.search.as_ref() else {
        return 0;
    };
    let Some(board) = state.hot_board() else {
        return 0;
    };
    let Ok(port) = state.ensure_wormdb() else {
        return 0;
    };
    let Ok(client) = MemClient::connect(port) else {
        return 0;
    };

    let mut candidates = collect_candidates(state, store, &board);
    candidates.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    let pending_total = candidates.len() as u32;

    state.set_semantic_progress(|p| {
        p.sessions_done_pass = 0;
        p.embedder_id = Some(embedder.id().to_string());
        p.paused = false;
        rate.apply_to_progress(p, pending_total);
    });

    let mut reads = 0usize;
    let mut remaining = pending_total;
    for c in candidates.into_iter().take(BATCH.max(MAX_READS)) {
        if state.semantic_paused() {
            break;
        }
        if reads >= MAX_READS {
            break;
        }
        let label = c
            .summary
            .title
            .clone()
            .unwrap_or_else(|| c.summary.native_id.clone());
        state.set_semantic_progress(|p| {
            p.active = true;
            p.current = Some(label.clone());
            p.current_project = Some(c.project_label.clone());
            rate.apply_to_progress(p, remaining);
        });
        match index_one(state, store, &client, embedder, &c) {
            Ok(true) => {
                reads += 1;
                remaining = remaining.saturating_sub(1);
                rate.record_success();
                state.set_semantic_progress(|p| {
                    p.sessions_done_pass += 1;
                    p.last_error = None;
                    rate.apply_to_progress(p, remaining);
                });
            }
            Ok(false) => {
                // Transient skip (missing install, etc.)
            }
            Err(e) => {
                eprintln!("semantic index {}: {e}", c.summary.native_id);
                state.set_semantic_progress(|p| {
                    p.last_error = Some(e);
                });
                // Don't spin forever on hard embed failures this pass.
                break;
            }
        }
    }
    remaining
}

fn count_pending(state: &AppState) -> u32 {
    let Some(store) = state.search.as_ref() else {
        return 0;
    };
    let Some(board) = state.hot_board() else {
        return 0;
    };
    collect_candidates(state, store, &board).len() as u32
}

fn collect_candidates(
    state: &AppState,
    store: &portal_core::search::SearchStore,
    board: &portal_core::dto::BoardSnapshot,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for lane in &board.lanes {
        if state.adapter(&lane.agent.id).is_none() {
            continue;
        }
        for project in &lane.projects {
            for s in &project.sessions {
                let rev = session_revision(s);
                let need = match store.semantic_revision(&s.agent_id, &s.native_id) {
                    Ok(None) => true,
                    Ok(Some(r)) if r != rev => !s.maybe_live,
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
    candidates
}

struct Candidate {
    summary: SessionSummary,
    project_key: String,
    project_label: String,
    cwd_normalized: Option<String>,
    revision: String,
    updated_ms: i64,
}

fn index_one(
    state: &AppState,
    store: &portal_core::search::SearchStore,
    client: &MemClient,
    embedder: &dyn Embedder,
    c: &Candidate,
) -> Result<bool, String> {
    let Some(adapter) = state.adapter(&c.summary.agent_id) else {
        return Ok(false);
    };
    let Some(inst) = state.installation(&c.summary.agent_id) else {
        return Ok(false);
    };
    let session = adapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: c.summary.native_id.clone(),
                store_path: Some(c.summary.store_path.clone().into()),
            },
        )
        .map_err(|e| format!("read_session: {e}"))?;

    if let Ok(old) = store.semantic_chunk_ids(&c.summary.agent_id, &c.summary.native_id) {
        for id in old {
            let _ = client.delete_chunk(&id);
        }
    }

    // Topic-finding extract: user intent + cleaned prose, not tool dumps.
    let mut chunks = chunk_for_embed(&session, &ExtractOptions::for_semantic());
    chunks.retain(|ch| ch.text.trim().len() >= 8);
    if chunks.len() > MAX_CHUNKS_PER_SESSION {
        chunks.truncate(MAX_CHUNKS_PER_SESSION);
    }

    let title = c.summary.title.as_deref();
    let updated_ms = c.updated_ms;
    let mut chunk_ids = Vec::with_capacity(chunks.len());

    // Prepare texts, then embed in batches (one Ollama round-trip per batch).
    let prepared: Vec<(String, String, portal_core::search::Chunk)> = chunks
        .into_iter()
        .map(|ch| {
            let text = truncate_chars(&ch.text, MAX_EMBED_CHARS).to_string();
            let doc_id = chunk_doc_id(&c.summary.agent_id, &c.summary.native_id, ch.chunk_ix);
            (doc_id, text, ch)
        })
        .collect();

    for batch in prepared.chunks(EMBED_BATCH_SIZE) {
        if state.semantic_paused() {
            // Leave semantic_state unchanged so this session is retried; orphan
            // vectors from a partial pass are cleaned on the next successful index.
            return Ok(false);
        }
        let texts: Vec<String> = batch.iter().map(|(_, t, _)| t.clone()).collect();
        let embeddings = embedder
            .embed_batch(&texts)
            .map_err(|e| format!("embed_batch ({}): {e}", embedder.id()))?;
        if embeddings.len() != batch.len() {
            return Err(format!(
                "embed_batch size mismatch: got {} want {}",
                embeddings.len(),
                batch.len()
            ));
        }
        for ((doc_id, text, ch), emb) in batch.iter().zip(embeddings.into_iter()) {
            let role = match ch.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
                Role::Tool => "tool",
            };
            let meta = build_meta_json(
                &c.summary.agent_id,
                &c.summary.native_id,
                &c.project_key,
                &c.project_label,
                c.cwd_normalized.as_deref(),
                &c.summary.store_path,
                title,
                role,
                &ch.turn_id,
                ch.chunk_ix,
                updated_ms,
                &c.revision,
                c.summary.maybe_live,
            );
            client
                .add(doc_id, text, &emb, &meta, embedder.id())
                .map_err(|e| format!("mem_add {doc_id}: {e}"))?;
            chunk_ids.push(doc_id.clone());
        }
    }

    // Empty sessions: still record revision so we don't retry forever.
    store
        .upsert_semantic_state(
            &c.summary.agent_id,
            &c.summary.native_id,
            &c.revision,
            &chunk_ids,
            embedder.id(),
        )
        .map_err(|e| format!("semantic_state: {e}"))?;
    Ok(true)
}

fn truncate_chars(s: &str, max: usize) -> &str {
    if s.chars().count() <= max {
        return s;
    }
    let mut end = 0;
    for (i, (idx, _)) in s.char_indices().enumerate() {
        if i == max {
            end = idx;
            break;
        }
    }
    if end == 0 {
        s
    } else {
        &s[..end]
    }
}
