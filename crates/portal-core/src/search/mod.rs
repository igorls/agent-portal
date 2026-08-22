//! Global session search: metadata (instant), FTS (progressive), semantic sidecar (later).
//!
//! See `docs/global-session-search.md` (rev5, sidecar-first).

mod embed;
mod embedder_factory;
mod extract;
mod fts;
mod memory;
mod merge;
mod meta;
mod sidecar;

pub use embed::{
    sanitize_embedder_id, Embedder, HashEmbedder, OllamaEmbedder, HASH_EMBEDDER_ID, HASH_EMBED_DIM,
};
pub use embedder_factory::embedder_from_settings;
pub use extract::{
    chunk_for_embed, clean_for_embed, clean_snippet, extract_indexable_text, Chunk, ExtractOptions,
};
pub use fts::{build_fts_query, SearchStore};
pub use memory::{
    build_meta_json, chunk_doc_id, mem_filter_from_query, mem_hits_to_search_hits,
    parse_chunk_doc_id, MemClient, MemStats, MEMORY_NAMESPACE,
};
pub use merge::merge_hits;
pub use meta::search_metadata;
pub use sidecar::{resolve_wormdb_binary, WormWireClient, WormdbSidecar};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::dto::{BoardSnapshot, SessionSummary};

/// How a hit matched the query (richest kind wins when collapsing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SearchMatchKind {
    Recent,
    Title,
    Project,
    Path,
    Model,
    Id,
    Fts,
    Semantic,
}

impl SearchMatchKind {
    pub fn rank_priority(self) -> u8 {
        match self {
            Self::Title => 8,
            Self::Project => 7,
            Self::Path => 6,
            Self::Model => 5,
            Self::Id => 4,
            Self::Fts => 3,
            Self::Semantic => 2,
            Self::Recent => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub q: String,
    /// Max sessions to return (default 30).
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Restrict to these agent ids when non-empty.
    #[serde(default)]
    pub agent_ids: Vec<String>,
    /// Loose substring filter on project label / cwd (MCP free-text).
    #[serde(default)]
    pub project: Option<String>,
    /// Exact project identity: matches `cwd_normalized` or native `project_key`
    /// (board selection id). Prefer this for "this project only" scoping.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Include FTS content matches (no-op until FTS indexer is live).
    #[serde(default = "default_true")]
    pub include_content: bool,
    /// Include semantic matches (no-op until sidecar + embedder are live).
    #[serde(default)]
    pub include_semantic: bool,
}

fn default_limit() -> u32 {
    30
}
fn default_true() -> bool {
    true
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            q: String::new(),
            limit: 30,
            agent_ids: Vec::new(),
            project: None,
            project_id: None,
            include_content: true,
            include_semantic: false,
        }
    }
}

/// Whether a project group matches optional `project_id` (exact) and/or
/// `project` (substring on label/cwd).
pub fn project_matches_filters(
    project_key: &str,
    project_label: &str,
    cwd_normalized: Option<&str>,
    project_id: Option<&str>,
    project_substr: Option<&str>,
) -> bool {
    if let Some(id) = project_id.map(str::trim).filter(|s| !s.is_empty()) {
        let id_l = id.to_lowercase();
        let key_ok = project_key.eq_ignore_ascii_case(id);
        let cwd_ok = cwd_normalized
            .map(|c| c.eq_ignore_ascii_case(id) || c.to_lowercase() == id_l)
            .unwrap_or(false);
        // Also accept exact label match for convenience.
        let label_ok = project_label.eq_ignore_ascii_case(id);
        if !(key_ok || cwd_ok || label_ok) {
            return false;
        }
    }
    if let Some(sub) = project_substr.map(str::trim).filter(|s| !s.is_empty()) {
        let sub_l = sub.to_lowercase();
        let label_l = project_label.to_lowercase();
        let cwd_l = cwd_normalized.unwrap_or("").to_lowercase();
        if !label_l.contains(&sub_l)
            && !cwd_l.contains(&sub_l)
            && !project_key.to_lowercase().contains(&sub_l)
        {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub agent_id: String,
    pub native_id: String,
    pub store_path: String,
    pub title: Option<String>,
    pub project_label: String,
    pub project_key: String,
    pub cwd_normalized: Option<String>,
    /// Last activity on the session (when known). Used for display + ranking.
    pub updated_at: Option<DateTime<Utc>>,
    pub match_kind: SearchMatchKind,
    pub score: f32,
    pub snippet: Option<String>,
    pub maybe_live: bool,
    /// True when content index may lag (live session or not yet indexed).
    pub content_incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub query: String,
    /// Whether FTS layer contributed (false until indexer wired).
    pub content_available: bool,
    /// Whether semantic layer contributed.
    pub semantic_available: bool,
    /// Wall time for the search; always a safe JS number over IPC.
    #[ts(type = "number")]
    pub took_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexStatus {
    /// Sessions with a body in search.db.
    pub content_docs: u32,
    /// Sessions visible on the hot board.
    pub board_sessions: u32,
    pub content_enabled: bool,
    pub semantic_enabled: bool,
    pub semantic_chunks: u32,
    pub hnsw_live: bool,
    /// Semantic worker is actively embedding.
    #[serde(default)]
    pub semantic_active: bool,
    /// Session currently being embedded (title or id).
    #[serde(default)]
    pub semantic_current: Option<String>,
    /// Sessions still needing a semantic pass.
    #[serde(default)]
    pub semantic_pending: u32,
    /// Last embedder/index error (surface in Settings).
    #[serde(default)]
    pub semantic_error: Option<String>,
    /// Active embedder id (hash-… or ollama/…).
    #[serde(default)]
    pub semantic_embedder: Option<String>,
    /// User paused the background semantic indexer (resume to continue).
    #[serde(default)]
    pub semantic_paused: bool,
    /// Estimated seconds to finish the pending semantic queue (None until rate known).
    #[serde(default)]
    pub semantic_eta_secs: Option<u32>,
    /// Throughput used for the ETA (sessions/minute), if known.
    #[serde(default)]
    pub semantic_rate_per_min: Option<f32>,
    /// Sessions completed in the current rate window (since resume / reindex).
    #[serde(default)]
    pub semantic_done_window: u32,
}

/// Run hybrid search against an in-memory board + optional FTS/semantic hits.
pub fn search_sessions(
    board: &BoardSnapshot,
    query: &SearchQuery,
    fts_hits: Vec<SearchHit>,
    semantic_hits: Vec<SearchHit>,
) -> SearchResponse {
    let started = std::time::Instant::now();
    let q = query.q.trim();
    let limit = query.limit.clamp(1, 100) as usize;
    let content_available = !fts_hits.is_empty() || query.include_content;
    let semantic_available = !semantic_hits.is_empty() || query.include_semantic;

    let meta = if q.is_empty() {
        meta::recent_sessions(board, query, limit)
    } else {
        search_metadata(board, query)
    };

    let fts = if query.include_content {
        fts_hits
    } else {
        Vec::new()
    };
    let sem = if query.include_semantic {
        semantic_hits
    } else {
        Vec::new()
    };

    let mut hits = merge_hits(meta, fts, sem, limit);
    hits.truncate(limit);

    SearchResponse {
        hits,
        query: q.to_string(),
        content_available,
        semantic_available,
        took_ms: started.elapsed().as_millis() as u64,
    }
}

/// Convenience: meta + FTS from [`SearchStore`] when present.
pub fn search_sessions_with_store(
    board: &BoardSnapshot,
    query: &SearchQuery,
    store: Option<&SearchStore>,
) -> SearchResponse {
    search_sessions_full(board, query, store, None, None)
}

/// Full hybrid search: meta + FTS + optional WormDB semantic.
pub fn search_sessions_full(
    board: &BoardSnapshot,
    query: &SearchQuery,
    store: Option<&SearchStore>,
    wormdb_port: Option<u16>,
    embedder: Option<&dyn Embedder>,
) -> SearchResponse {
    let fts_hits = if query.include_content && !query.q.trim().is_empty() {
        store.and_then(|s| s.search(query).ok()).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut semantic_hits = Vec::new();
    let mut semantic_available = false;
    if query.include_semantic && !query.q.trim().is_empty() {
        if let (Some(port), Some(emb)) = (wormdb_port, embedder) {
            match semantic_search(port, emb, query) {
                Ok(hits) => {
                    semantic_available = true;
                    semantic_hits = hits;
                }
                Err(e) => {
                    eprintln!("semantic search failed: {e}");
                }
            }
        }
    }

    let mut resp = search_sessions(board, query, fts_hits, semantic_hits);
    if let Some(s) = store {
        resp.content_available = s.count_docs().unwrap_or(0) > 0;
    } else {
        resp.content_available = false;
    }
    resp.semantic_available = semantic_available;
    resp
}

fn semantic_search(
    port: u16,
    embedder: &dyn Embedder,
    query: &SearchQuery,
) -> crate::error::Result<Vec<SearchHit>> {
    let emb = embedder.embed(&query.q)?;
    let client = MemClient::connect(port)?;
    let filter = mem_filter_from_query(query);
    let k = (query.limit.clamp(1, 100) * 4).min(80);
    // Session discovery: prefer fewer stronger neighbors over a soft flood.
    // min_score is cosine-ish (WormDB); 0.18 drops pure noise while keeping paraphrases.
    let hits = client.query(
        &emb,
        k,
        0.15, // light temporal decay — topic match > recency for "find the session"
        0.18,
        220,
        filter.as_deref(),
    )?;
    let mut session_hits = mem_hits_to_search_hits(hits, query);
    session_hits.truncate(query.limit.clamp(1, 100) as usize);
    Ok(session_hits)
}

/// Build a hit skeleton from a summary + project context.
pub fn hit_from_summary(
    summary: &SessionSummary,
    project_label: &str,
    project_key: &str,
    cwd_normalized: Option<&str>,
    match_kind: SearchMatchKind,
    score: f32,
    snippet: Option<String>,
) -> SearchHit {
    SearchHit {
        agent_id: summary.agent_id.clone(),
        native_id: summary.native_id.clone(),
        store_path: summary.store_path.clone(),
        title: summary.title.clone(),
        project_label: project_label.to_string(),
        project_key: project_key.to_string(),
        cwd_normalized: cwd_normalized.map(str::to_string),
        updated_at: summary.updated_at,
        match_kind,
        score,
        snippet,
        maybe_live: summary.maybe_live,
        content_incomplete: summary.maybe_live,
    }
}

/// Recency in `0.0..1.0` (floor 0.05). Half-life ≈ 3 days so recent coding
/// sessions dominate over month-old matches with the same text relevance.
pub fn recency_factor(updated_ms: i64) -> f32 {
    if updated_ms <= 0 {
        return 0.05;
    }
    let now = Utc::now().timestamp_millis();
    let age_h = ((now - updated_ms).max(0) as f32) / 3_600_000.0;
    (-age_h / 72.0).exp().clamp(0.05, 1.0)
}

/// Blend text/meta relevance with recency.
///
/// `max_boost` is added when the session is brand-new (typically 25–45).
/// Live sessions get a tiny haircut so they don't dominate solely by churn.
pub fn score_with_recency(
    relevance: f32,
    updated_ms: i64,
    max_boost: f32,
    maybe_live: bool,
) -> f32 {
    let r = recency_factor(updated_ms);
    // Multiplicative tilt: older hits keep most of their relevance but never
    // fully outrank a solid recent match of the same kind.
    let mut score = relevance * (0.62 + 0.38 * r) + r * max_boost;
    if maybe_live {
        score *= 0.98;
    }
    score
}

/// Pure recency score for empty-query "recent" mode (0..100 scale).
pub fn recency_rank_score(updated_ms: i64) -> f32 {
    recency_factor(updated_ms) * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{
        AgentDescriptor, Capabilities, Installation, Lane, ProjectGroup, SessionSummary, StoreKind,
        SupportLevel,
    };
    use chrono::Utc;

    fn caps() -> Capabilities {
        Capabilities {
            store_kind: StoreKind::JsonlPerSession,
            read: SupportLevel::Full,
            write_native: SupportLevel::None,
            watch: false,
            launch_resume: false,
            launch_new: false,
            context_tokens: None,
            write_confidence: None,
            version_range_tested: "test".into(),
            notes: vec![],
        }
    }

    fn summary(agent: &str, id: &str, title: &str, path: &str) -> SessionSummary {
        SessionSummary {
            agent_id: agent.into(),
            native_id: id.into(),
            project_key: "proj".into(),
            title: Some(title.into()),
            cwd: Some(path.into()),
            git_branch: None,
            model: Some("gpt-test".into()),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
            message_count: Some(3),
            message_count_exact: true,
            size_bytes: 100,
            store_path: format!("/tmp/{id}.jsonl"),
            maybe_live: false,
        }
    }

    fn board() -> BoardSnapshot {
        BoardSnapshot {
            lanes: vec![Lane {
                agent: AgentDescriptor {
                    id: "claude-code".into(),
                    display_name: "Claude Code".into(),
                    capabilities: caps(),
                    installation: Some(Installation {
                        cli_path: None,
                        version: Some("1".into()),
                        store_root: "/tmp".into(),
                    }),
                },
                projects: vec![ProjectGroup {
                    key: "proj".into(),
                    label: "agent-portal".into(),
                    cwd_normalized: Some("c:/work/agent-portal".into()),
                    sessions: vec![
                        summary(
                            "claude-code",
                            "s1",
                            "Fix WormDB sidecar search",
                            "c:/work/agent-portal",
                        ),
                        summary(
                            "claude-code",
                            "s2",
                            "Unrelated UI polish",
                            "c:/work/agent-portal",
                        ),
                    ],
                }],
                error: None,
            }],
            feasibility: vec![],
            generated_at: Utc::now(),
        }
    }

    #[test]
    fn title_match_outranks_empty() {
        let b = board();
        let resp = search_sessions(
            &b,
            &SearchQuery {
                q: "WormDB".into(),
                ..Default::default()
            },
            vec![],
            vec![],
        );
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].native_id, "s1");
        assert_eq!(resp.hits[0].match_kind, SearchMatchKind::Title);
        assert!(resp.hits[0].score >= 80.0);
        assert!(resp.hits[0].updated_at.is_some());
    }

    #[test]
    fn recent_session_outranks_stale_same_title_match() {
        use chrono::Duration;
        let mut b = board();
        // Two title hits; older one would win on pure string match if recency ignored.
        b.lanes[0].projects[0].sessions[0].title = Some("Fix search ranking".into());
        b.lanes[0].projects[0].sessions[0].updated_at = Some(Utc::now() - Duration::days(30));
        b.lanes[0].projects[0].sessions[1].title = Some("Fix search ranking later".into());
        b.lanes[0].projects[0].sessions[1].updated_at = Some(Utc::now());
        let resp = search_sessions(
            &b,
            &SearchQuery {
                q: "search ranking".into(),
                ..Default::default()
            },
            vec![],
            vec![],
        );
        assert!(resp.hits.len() >= 2);
        assert_eq!(
            resp.hits[0].native_id, "s2",
            "fresher title hit should rank first"
        );
    }

    #[test]
    fn empty_query_returns_recent() {
        let b = board();
        let resp = search_sessions(&b, &SearchQuery::default(), vec![], vec![]);
        assert!(!resp.hits.is_empty());
        assert!(resp
            .hits
            .iter()
            .all(|h| h.match_kind == SearchMatchKind::Recent));
    }

    #[test]
    fn agent_filter_restricts() {
        let b = board();
        let resp = search_sessions(
            &b,
            &SearchQuery {
                q: "WormDB".into(),
                agent_ids: vec!["codex".into()],
                ..Default::default()
            },
            vec![],
            vec![],
        );
        assert!(resp.hits.is_empty());
    }

    #[test]
    fn project_id_scopes_exact() {
        let b = board();
        let resp = search_sessions(
            &b,
            &SearchQuery {
                q: "".into(),
                project_id: Some("c:/work/agent-portal".into()),
                limit: 50,
                ..Default::default()
            },
            vec![],
            vec![],
        );
        assert!(!resp.hits.is_empty());
        assert!(resp
            .hits
            .iter()
            .all(|h| h.cwd_normalized.as_deref() == Some("c:/work/agent-portal")));

        let none = search_sessions(
            &b,
            &SearchQuery {
                q: "".into(),
                project_id: Some("c:/other".into()),
                ..Default::default()
            },
            vec![],
            vec![],
        );
        assert!(none.hits.is_empty());
    }

    #[test]
    fn project_substring_scopes_label() {
        let b = board();
        let resp = search_sessions(
            &b,
            &SearchQuery {
                q: "WormDB".into(),
                project: Some("agent-portal".into()),
                ..Default::default()
            },
            vec![],
            vec![],
        );
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].project_label, "agent-portal");
    }
}
