//! Instant metadata search over an in-memory [`BoardSnapshot`].

use crate::dto::BoardSnapshot;
use crate::search::{
    hit_from_summary, project_matches_filters, recency_rank_score, score_with_recency, SearchHit,
    SearchMatchKind, SearchQuery,
};

/// Recent sessions when the query is empty (palette open).
pub fn recent_sessions(board: &BoardSnapshot, query: &SearchQuery, limit: usize) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = Vec::new();
    for_each_session(board, query, |s, label, key, cwd| {
        let ms = s.updated_at.map(|t| t.timestamp_millis()).unwrap_or(0);
        let score = recency_rank_score(ms);
        hits.push(hit_from_summary(
            s,
            label,
            key,
            cwd,
            SearchMatchKind::Recent,
            score,
            None,
        ));
    });
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    hits.truncate(limit);
    hits
}

/// Substring / token match on title, project, path, model, native id.
pub fn search_metadata(board: &BoardSnapshot, query: &SearchQuery) -> Vec<SearchHit> {
    let q = query.q.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let q_lower = q.to_lowercase();
    let mut hits = Vec::new();

    for_each_session(board, query, |s, label, key, cwd| {
        let (kind, relevance, snippet) = match_fields(s, label, cwd, &q_lower, q);
        if relevance <= 0.0 {
            return;
        }
        // Title matches can pick up more recency boost than weak id/model hits.
        let max_boost = match kind {
            SearchMatchKind::Title => 42.0,
            SearchMatchKind::Project | SearchMatchKind::Path => 36.0,
            _ => 28.0,
        };
        let ms = s.updated_at.map(|t| t.timestamp_millis()).unwrap_or(0);
        let score = score_with_recency(relevance, ms, max_boost, s.maybe_live);
        hits.push(hit_from_summary(s, label, key, cwd, kind, score, snippet));
    });

    hits
}

fn for_each_session<'a>(
    board: &'a BoardSnapshot,
    query: &SearchQuery,
    mut f: impl FnMut(&'a crate::dto::SessionSummary, &'a str, &'a str, Option<&'a str>),
) {
    let agent_filter: Option<std::collections::HashSet<&str>> = if query.agent_ids.is_empty() {
        None
    } else {
        Some(query.agent_ids.iter().map(String::as_str).collect())
    };
    let project_id = query.project_id.as_deref();
    let project_substr = query.project.as_deref();

    for lane in &board.lanes {
        if let Some(ref af) = agent_filter {
            if !af.contains(lane.agent.id.as_str()) {
                continue;
            }
        }
        for project in &lane.projects {
            if !project_matches_filters(
                &project.key,
                &project.label,
                project.cwd_normalized.as_deref(),
                project_id,
                project_substr,
            ) {
                continue;
            }
            let cwd = project.cwd_normalized.as_deref();
            for s in &project.sessions {
                f(s, &project.label, &project.key, cwd);
            }
        }
    }
}

fn match_fields(
    s: &crate::dto::SessionSummary,
    project_label: &str,
    cwd: Option<&str>,
    q_lower: &str,
    q_raw: &str,
) -> (SearchMatchKind, f32, Option<String>) {
    // Title exact / substring
    if let Some(title) = s.title.as_deref() {
        let t = title.trim();
        if !t.is_empty() {
            let tl = t.to_lowercase();
            if tl == q_lower {
                return (SearchMatchKind::Title, 100.0, Some(t.to_string()));
            }
            if tl.contains(q_lower) {
                return (SearchMatchKind::Title, 80.0, Some(t.to_string()));
            }
        }
    }

    // Project label
    let pl = project_label.to_lowercase();
    if pl.contains(q_lower) {
        return (
            SearchMatchKind::Project,
            60.0,
            Some(project_label.to_string()),
        );
    }

    // Path (cwd_normalized or summary.cwd)
    let path = cwd.or(s.cwd.as_deref()).unwrap_or("").to_lowercase();
    if !path.is_empty() && path.contains(q_lower) {
        return (
            SearchMatchKind::Path,
            50.0,
            Some(cwd.or(s.cwd.as_deref()).unwrap_or("").to_string()),
        );
    }

    // Model
    if let Some(model) = s.model.as_deref() {
        if model.to_lowercase().contains(q_lower) {
            return (SearchMatchKind::Model, 40.0, Some(model.to_string()));
        }
    }

    // Native id
    if s.native_id.to_lowercase().contains(q_lower) || s.native_id.eq_ignore_ascii_case(q_raw) {
        return (SearchMatchKind::Id, 40.0, Some(s.native_id.clone()));
    }

    (SearchMatchKind::Title, 0.0, None)
}
