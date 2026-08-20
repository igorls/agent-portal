//! Merge and collapse hits from meta / FTS / semantic layers.

use crate::search::{SearchHit, SearchMatchKind};
use std::collections::HashMap;

/// Merge layers, collapse to one hit per (agent_id, native_id), keep top `limit`.
pub fn merge_hits(
    meta: Vec<SearchHit>,
    fts: Vec<SearchHit>,
    semantic: Vec<SearchHit>,
    limit: usize,
) -> Vec<SearchHit> {
    let mut best: HashMap<(String, String), SearchHit> = HashMap::new();

    for hit in meta.into_iter().chain(fts).chain(semantic) {
        let key = (hit.agent_id.clone(), hit.native_id.clone());
        match best.get_mut(&key) {
            None => {
                best.insert(key, hit);
            }
            Some(existing) => {
                *existing = prefer(existing.clone(), hit);
            }
        }
    }

    let mut out: Vec<SearchHit> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| {
                b.match_kind
                    .rank_priority()
                    .cmp(&a.match_kind.rank_priority())
            })
    });
    out.truncate(limit);
    out
}

fn prefer(a: SearchHit, b: SearchHit) -> SearchHit {
    // Higher score wins; tie-break on match kind richness; keep best snippet.
    let a_better = a.score > b.score
        || (a.score == b.score && a.match_kind.rank_priority() >= b.match_kind.rank_priority());
    if a_better {
        let mut out = a;
        if out.snippet.is_none() {
            out.snippet = b.snippet.clone();
        }
        if b.match_kind == SearchMatchKind::Fts || b.match_kind == SearchMatchKind::Semantic {
            // Promote kind if we only had weak meta but content matched too —
            // only when scores are close and content kind is "richer" for UX.
            if b.score >= out.score * 0.9
                && b.match_kind.rank_priority() > out.match_kind.rank_priority()
                && matches!(
                    out.match_kind,
                    SearchMatchKind::Recent | SearchMatchKind::Id | SearchMatchKind::Model
                )
            {
                out.match_kind = b.match_kind;
                if b.snippet.is_some() {
                    out.snippet = b.snippet;
                }
            }
        }
        out
    } else {
        let mut out = b;
        if out.snippet.is_none() {
            out.snippet = a.snippet;
        }
        out
    }
}
