//! WormDB `mem_*` client for session chunk storage and semantic query.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{PortalError, Result};
use crate::search::sidecar::WormWireClient;
use crate::search::{score_with_recency, SearchHit, SearchMatchKind, SearchQuery};

pub const MEMORY_NAMESPACE: &str = "portal-sessions";

#[derive(Debug, Clone)]
pub struct MemHit {
    pub id: String,
    pub score: f32,
    pub ts: i64,
    pub doc: Option<String>,
    pub meta: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemStats {
    pub namespace: String,
    pub vectors: u64,
    pub doc_keys: u64,
    #[serde(default)]
    pub dimensions: u64,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default)]
    pub hnsw: Option<Value>,
}

pub struct MemClient {
    port: u16,
}

impl MemClient {
    pub fn connect(port: u16) -> Result<Self> {
        // Probe STATUS once.
        let mut c = WormWireClient::connect(port)?;
        let _ = c.status()?;
        Ok(Self { port })
    }

    fn client(&self) -> Result<WormWireClient> {
        WormWireClient::connect(self.port)
    }

    pub fn init(&self, embedder_id: &str, metric: &str) -> Result<()> {
        // Defensive: WormDB rejects `:`, spaces, etc. Callers should already
        // sanitize; do it here so a bad path never reaches the sidecar.
        let embedder_id = crate::search::sanitize_embedder_id(embedder_id);
        let mut c = self.client()?;
        let _ = c.exec(
            "mem_init",
            &[
                MEMORY_NAMESPACE.as_bytes(),
                embedder_id.as_bytes(),
                metric.as_bytes(),
            ],
        )?;
        Ok(())
    }

    /// Drop the whole memory namespace (docs + vectors + HNSW). Used on embedder change.
    pub fn drop_namespace(&self) -> Result<String> {
        let mut c = self.client()?;
        let bytes = c.exec("mem_drop", &[MEMORY_NAMESPACE.as_bytes()])?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Reset vector index and switch embedder id while attempting to keep docs.
    /// Prefer [`drop_namespace`] when changing dimensions.
    pub fn reset_index(&self, new_embedder_id: &str) -> Result<String> {
        let new_embedder_id = crate::search::sanitize_embedder_id(new_embedder_id);
        let mut c = self.client()?;
        let bytes = c.exec(
            "mem_reset_index",
            &[MEMORY_NAMESPACE.as_bytes(), new_embedder_id.as_bytes()],
        )?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn stats(&self) -> Result<MemStats> {
        let mut c = self.client()?;
        let bytes = c.exec("mem_stats", &[MEMORY_NAMESPACE.as_bytes()])?;
        let s = String::from_utf8_lossy(&bytes);
        serde_json::from_str(&s)
            .map_err(|e| PortalError::Other(format!("mem_stats parse: {e}: {s}")))
    }

    pub fn add(
        &self,
        doc_id: &str,
        text: &str,
        embedding: &[u8],
        meta_json: &str,
        embedder_id: &str,
    ) -> Result<()> {
        let embedder_id = crate::search::sanitize_embedder_id(embedder_id);
        let mut c = self.client()?;
        // mem_add <ns> <doc_id> <text> <embedding> [meta] [worm] [embedder_id]
        // worm "0" = non-WORM (reindexable)
        let _ = c.exec(
            "mem_add",
            &[
                MEMORY_NAMESPACE.as_bytes(),
                doc_id.as_bytes(),
                text.as_bytes(),
                embedding,
                meta_json.as_bytes(),
                b"0",
                embedder_id.as_bytes(),
            ],
        )?;
        Ok(())
    }

    /// Best-effort delete of a memory doc + vector keys for a chunk id.
    pub fn delete_chunk(&self, doc_id: &str) -> Result<()> {
        let mut c = self.client()?;
        let mem_key = format!("mem:{MEMORY_NAMESPACE}:{doc_id}");
        let meta_key = format!("mem:{MEMORY_NAMESPACE}:{doc_id}:meta");
        let vec_key = format!("vec:mem:{MEMORY_NAMESPACE}:{doc_id}");
        let bq_key = format!("bq:vec:mem:{MEMORY_NAMESPACE}:{doc_id}");
        for key in [mem_key, meta_key, vec_key, bq_key] {
            let _ = c.del(key.as_bytes());
        }
        Ok(())
    }

    pub fn query(
        &self,
        embedding: &[u8],
        k: u32,
        lambda: f32,
        min_score: f32,
        snippet_chars: u32,
        filter: Option<&str>,
    ) -> Result<Vec<MemHit>> {
        let mut c = self.client()?;
        let k_s = k.to_string();
        let lambda_s = lambda.to_string();
        let min_s = min_score.to_string();
        let snip_s = snippet_chars.to_string();
        let filter_owned = filter.map(str::to_string);
        let mut args: Vec<&[u8]> = vec![
            MEMORY_NAMESPACE.as_bytes(),
            embedding,
            k_s.as_bytes(),
            lambda_s.as_bytes(),
            min_s.as_bytes(),
            snip_s.as_bytes(),
        ];
        if let Some(ref f) = filter_owned {
            args.push(f.as_bytes());
        }
        let bytes = c.exec("mem_query", &args)?;
        let s = String::from_utf8_lossy(&bytes);
        if s.trim().is_empty() {
            return Ok(Vec::new());
        }
        let raw: Vec<Value> = serde_json::from_str(&s)
            .map_err(|e| PortalError::Other(format!("mem_query parse: {e}: {s}")))?;
        let mut out = Vec::new();
        for v in raw {
            let id = v
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let score = v.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            let ts = v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0);
            let doc = v.get("doc").and_then(|x| x.as_str()).map(str::to_string);
            let meta = v.get("meta").cloned().unwrap_or(Value::Null);
            out.push(MemHit {
                id,
                score,
                ts,
                doc,
                meta,
            });
        }
        Ok(out)
    }
}

/// Build chunk doc id: `{agent}/{native}/c{ix:04}`.
pub fn chunk_doc_id(agent_id: &str, native_id: &str, chunk_ix: u32) -> String {
    format!("{agent_id}/{native_id}/c{chunk_ix:04}")
}

pub fn parse_chunk_doc_id(id: &str) -> Option<(String, String, u32)> {
    let (rest, cpart) = id.rsplit_once('/')?;
    let ix: u32 = cpart.strip_prefix('c')?.parse().ok()?;
    let (agent, native) = rest.split_once('/')?;
    if agent.is_empty() || native.is_empty() {
        return None;
    }
    Some((agent.to_string(), native.to_string(), ix))
}

/// Convert mem hits → session-level SearchHits (collapse chunks).
pub fn mem_hits_to_search_hits(hits: Vec<MemHit>, query: &SearchQuery) -> Vec<SearchHit> {
    use std::collections::HashMap;
    let mut best: HashMap<(String, String), SearchHit> = HashMap::new();

    for h in hits {
        let (agent_id, native_id, _ix) = match parse_chunk_doc_id(&h.id) {
            Some(x) => x,
            None => {
                // Fall back to meta fields.
                let agent = h
                    .meta
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let native = h
                    .meta
                    .get("native_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if agent.is_empty() || native.is_empty() {
                    continue;
                }
                (agent, native, 0u32)
            }
        };

        let project_label = h
            .meta
            .get("project")
            .or_else(|| h.meta.get("project_label"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let project_key = h
            .meta
            .get("project_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cwd = h
            .meta
            .get("cwd_normalized")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let store_path = h
            .meta
            .get("store_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let title = h
            .meta
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let updated_ms = h
            .meta
            .get("updated_at_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or(h.ts);
        let maybe_live = h
            .meta
            .get("maybe_live")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Optional project filters (when meta incomplete, skip filter fail-open for that hit).
        if !crate::search::project_matches_filters(
            &project_key,
            &project_label,
            cwd.as_deref(),
            query.project_id.as_deref(),
            query.project.as_deref(),
        ) {
            continue;
        }
        if !query.agent_ids.is_empty() && !query.agent_ids.iter().any(|a| a == &agent_id) {
            continue;
        }

        // Cosine-ish ~0..1. Do **not** floor weak hits — that made pasta-noise
        // look like real sessions after recency boost. Goal is session discovery.
        let cosine = h.score.clamp(0.0, 1.0);
        if cosine < 0.18 {
            continue;
        }
        let relevance = cosine * 55.0; // stronger weight on topic match vs recency
        let score = score_with_recency(relevance, updated_ms, 12.0, maybe_live);
        let snippet = h.doc.as_ref().and_then(|d| {
            let cleaned = crate::search::clean_snippet(d, 160);
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            }
        });
        let updated_at = if updated_ms > 0 {
            chrono::DateTime::from_timestamp_millis(updated_ms)
        } else {
            None
        };

        let hit = SearchHit {
            agent_id: agent_id.clone(),
            native_id: native_id.clone(),
            store_path,
            title,
            project_label,
            project_key,
            cwd_normalized: cwd,
            updated_at,
            match_kind: SearchMatchKind::Semantic,
            score,
            snippet,
            maybe_live,
            content_incomplete: maybe_live,
        };
        let key = (agent_id, native_id);
        match best.get_mut(&key) {
            None => {
                best.insert(key, hit);
            }
            Some(existing) => {
                if hit.score > existing.score {
                    *existing = hit;
                } else if existing.snippet.is_none() {
                    existing.snippet = hit.snippet;
                }
            }
        }
    }

    let mut out: Vec<_> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    out
}

#[allow(clippy::too_many_arguments)]
pub fn build_meta_json(
    agent_id: &str,
    native_id: &str,
    project_key: &str,
    project_label: &str,
    cwd_normalized: Option<&str>,
    store_path: &str,
    title: Option<&str>,
    role: &str,
    turn_id: &str,
    chunk_ix: u32,
    updated_at_ms: i64,
    session_revision: &str,
    maybe_live: bool,
) -> String {
    json!({
        "agent_id": agent_id,
        "native_id": native_id,
        "project_key": project_key,
        "project": project_label,
        "cwd_normalized": cwd_normalized,
        "store_path": store_path,
        "title": title,
        "role": role,
        "turn_id": turn_id,
        "chunk_ix": chunk_ix,
        "updated_at_ms": updated_at_ms,
        "session_revision": session_revision,
        "maybe_live": maybe_live,
    })
    .to_string()
}

pub fn mem_filter_from_query(query: &SearchQuery) -> Option<String> {
    // Only exact agent_id at the WormDB layer. Free-text project filters are
    // applied in mem_hits_to_search_hits (substring). Pushing project="foo" into
    // mem_query failed when labels did not match exactly and disabled semantic.
    if query.agent_ids.len() == 1 {
        Some(format!("agent_id=\"{}\"", query.agent_ids[0]))
    } else {
        None
    }
}
