//! Lexical content index in app-local `search.db` (FTS5).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::dto::SessionSummary;
use crate::error::{PortalError, Result};
use crate::search::{SearchHit, SearchMatchKind, SearchQuery};

pub struct SearchStore {
    path: PathBuf,
}

impl SearchStore {
    pub fn open(app_data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(app_data_dir)?;
        let path = app_data_dir.join("search.db");
        let store = Self { path };
        store.with_conn(|conn| {
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA busy_timeout=3000;
                 CREATE TABLE IF NOT EXISTS session_docs (
                     agent_id TEXT NOT NULL,
                     native_id TEXT NOT NULL,
                     store_path TEXT NOT NULL,
                     project_key TEXT NOT NULL,
                     project_label TEXT NOT NULL,
                     cwd_normalized TEXT,
                     title TEXT,
                     revision TEXT NOT NULL,
                     maybe_live INTEGER NOT NULL DEFAULT 0,
                     body TEXT NOT NULL,
                     updated_at INTEGER NOT NULL,
                     PRIMARY KEY (agent_id, native_id)
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS session_fts USING fts5(
                     body,
                     title,
                     project_label,
                     agent_id UNINDEXED,
                     native_id UNINDEXED,
                     content='session_docs',
                     content_rowid='rowid'
                 );
                 CREATE TABLE IF NOT EXISTS fts_meta (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS semantic_state (
                     agent_id TEXT NOT NULL,
                     native_id TEXT NOT NULL,
                     revision TEXT NOT NULL,
                     chunk_ids_json TEXT NOT NULL,
                     embedder_id TEXT NOT NULL,
                     updated_at INTEGER NOT NULL,
                     PRIMARY KEY (agent_id, native_id)
                 );",
            )
            .map_err(sql_err)?;
            Ok(())
        })?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = Connection::open(&self.path).map_err(sql_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(sql_err)?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
        f(&conn)
    }

    /// Revision currently indexed for this session, if any.
    pub fn indexed_revision(&self, agent_id: &str, native_id: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT revision FROM session_docs WHERE agent_id=?1 AND native_id=?2",
                params![agent_id, native_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)
        })
    }

    /// Upsert one session body into FTS.
    pub fn upsert_session(
        &self,
        summary: &SessionSummary,
        project_key: &str,
        project_label: &str,
        cwd_normalized: Option<&str>,
        revision: &str,
        body: &str,
    ) -> Result<()> {
        let title = summary.title.clone().unwrap_or_default();
        let updated_at = summary
            .updated_at
            .map(|t| t.timestamp_millis())
            .unwrap_or(0);
        let maybe_live = if summary.maybe_live { 1i64 } else { 0 };
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            // Remove old FTS row if present (external-content FTS needs manual sync).
            let old_rowid: Option<i64> = tx
                .query_row(
                    "SELECT rowid FROM session_docs WHERE agent_id=?1 AND native_id=?2",
                    params![summary.agent_id, summary.native_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            if let Some(rowid) = old_rowid {
                tx.execute(
                    "INSERT INTO session_fts(session_fts, rowid, body, title, project_label, agent_id, native_id)
                     VALUES('delete', ?1, '', '', '', '', '')",
                    params![rowid],
                )
                .map_err(sql_err)?;
            }
            tx.execute(
                "INSERT INTO session_docs (
                    agent_id, native_id, store_path, project_key, project_label,
                    cwd_normalized, title, revision, maybe_live, body, updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT(agent_id, native_id) DO UPDATE SET
                    store_path=excluded.store_path,
                    project_key=excluded.project_key,
                    project_label=excluded.project_label,
                    cwd_normalized=excluded.cwd_normalized,
                    title=excluded.title,
                    revision=excluded.revision,
                    maybe_live=excluded.maybe_live,
                    body=excluded.body,
                    updated_at=excluded.updated_at",
                params![
                    summary.agent_id,
                    summary.native_id,
                    summary.store_path,
                    project_key,
                    project_label,
                    cwd_normalized,
                    title,
                    revision,
                    maybe_live,
                    body,
                    updated_at,
                ],
            )
            .map_err(sql_err)?;
            let rowid: i64 = tx
                .query_row(
                    "SELECT rowid FROM session_docs WHERE agent_id=?1 AND native_id=?2",
                    params![summary.agent_id, summary.native_id],
                    |r| r.get(0),
                )
                .map_err(sql_err)?;
            tx.execute(
                "INSERT INTO session_fts(rowid, body, title, project_label, agent_id, native_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    rowid,
                    body,
                    title,
                    project_label,
                    summary.agent_id,
                    summary.native_id
                ],
            )
            .map_err(sql_err)?;
            tx.commit().map_err(sql_err)?;
            Ok(())
        })
    }

    pub fn delete_session(&self, agent_id: &str, native_id: &str) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            let rowid: Option<i64> = tx
                .query_row(
                    "SELECT rowid FROM session_docs WHERE agent_id=?1 AND native_id=?2",
                    params![agent_id, native_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            if let Some(rowid) = rowid {
                tx.execute(
                    "INSERT INTO session_fts(session_fts, rowid, body, title, project_label, agent_id, native_id)
                     VALUES('delete', ?1, '', '', '', '', '')",
                    params![rowid],
                )
                .map_err(sql_err)?;
                tx.execute(
                    "DELETE FROM session_docs WHERE rowid=?1",
                    params![rowid],
                )
                .map_err(sql_err)?;
            }
            tx.commit().map_err(sql_err)?;
            Ok(())
        })
    }

    pub fn count_docs(&self) -> Result<u32> {
        self.with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM session_docs", [], |r| r.get(0))
                .map_err(sql_err)
        })
    }

    pub fn semantic_revision(&self, agent_id: &str, native_id: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT revision FROM semantic_state WHERE agent_id=?1 AND native_id=?2",
                params![agent_id, native_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)
        })
    }

    pub fn semantic_chunk_ids(&self, agent_id: &str, native_id: &str) -> Result<Vec<String>> {
        self.with_conn(|conn| {
            let json: Option<String> = conn
                .query_row(
                    "SELECT chunk_ids_json FROM semantic_state WHERE agent_id=?1 AND native_id=?2",
                    params![agent_id, native_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            Ok(json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default())
        })
    }

    pub fn upsert_semantic_state(
        &self,
        agent_id: &str,
        native_id: &str,
        revision: &str,
        chunk_ids: &[String],
        embedder_id: &str,
    ) -> Result<()> {
        let chunk_ids_json =
            serde_json::to_string(chunk_ids).map_err(|e| PortalError::Other(e.to_string()))?;
        let now = chrono::Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO semantic_state (agent_id,native_id,revision,chunk_ids_json,embedder_id,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(agent_id,native_id) DO UPDATE SET
                    revision=excluded.revision,
                    chunk_ids_json=excluded.chunk_ids_json,
                    embedder_id=excluded.embedder_id,
                    updated_at=excluded.updated_at",
                params![agent_id, native_id, revision, chunk_ids_json, embedder_id, now],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    pub fn delete_semantic_state(&self, agent_id: &str, native_id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM semantic_state WHERE agent_id=?1 AND native_id=?2",
                params![agent_id, native_id],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    pub fn count_semantic_sessions(&self) -> Result<u32> {
        self.with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM semantic_state", [], |r| r.get(0))
                .map_err(sql_err)
        })
    }

    pub fn all_semantic_keys(&self) -> Result<Vec<(String, String)>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT agent_id, native_id FROM semantic_state")
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(sql_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(sql_err)?);
            }
            Ok(out)
        })
    }

    pub fn clear_semantic_state(&self) -> Result<u32> {
        self.with_conn(|conn| {
            let n = conn
                .execute("DELETE FROM semantic_state", [])
                .map_err(sql_err)?;
            Ok(n as u32)
        })
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM fts_meta WHERE key=?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)
        })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO fts_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    /// FTS5 search → SearchHits (session-level).
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        let q = query.q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let fts_q = build_fts_query(q);
        if fts_q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = query.limit.max(1).min(100) as i64;
        let agent_filter = if query.agent_ids.is_empty() {
            None
        } else {
            Some(query.agent_ids.clone())
        };

        self.with_conn(|conn| {
            let sql = "
                SELECT d.agent_id, d.native_id, d.store_path, d.title, d.project_label,
                       d.project_key, d.cwd_normalized, d.maybe_live, d.updated_at,
                       snippet(session_fts, 0, '«', '»', '…', 12) AS snip,
                       bm25(session_fts) AS rank
                FROM session_fts
                JOIN session_docs d ON d.rowid = session_fts.rowid
                WHERE session_fts MATCH ?1
                ORDER BY rank
                LIMIT ?2
            ";
            let mut stmt = conn.prepare(sql).map_err(sql_err)?;
            let rows = stmt
                .query_map(params![fts_q, limit * 4], |r| {
                    Ok(FtsRow {
                        agent_id: r.get(0)?,
                        native_id: r.get(1)?,
                        store_path: r.get(2)?,
                        title: r.get(3)?,
                        project_label: r.get(4)?,
                        project_key: r.get(5)?,
                        cwd_normalized: r.get(6)?,
                        maybe_live: r.get::<_, i64>(7)? != 0,
                        updated_at_ms: r.get(8)?,
                        snippet: r.get(9)?,
                        bm25: r.get(10)?,
                    })
                })
                .map_err(sql_err)?;

            let mut hits = Vec::new();
            for row in rows {
                let row = row.map_err(sql_err)?;
                if let Some(ref agents) = agent_filter {
                    if !agents.iter().any(|a| a == &row.agent_id) {
                        continue;
                    }
                }
                if !crate::search::project_matches_filters(
                    &row.project_key,
                    &row.project_label,
                    row.cwd_normalized.as_deref(),
                    query.project_id.as_deref(),
                    query.project.as_deref(),
                ) {
                    continue;
                }
                // bm25: lower is better; map into ~8..30 then blend recency.
                let relevance = bm25_to_score(row.bm25);
                let score = crate::search::score_with_recency(
                    relevance,
                    row.updated_at_ms,
                    32.0,
                    row.maybe_live,
                );
                let title = if row.title.is_empty() {
                    None
                } else {
                    Some(row.title)
                };
                let updated_at = if row.updated_at_ms > 0 {
                    chrono::DateTime::from_timestamp_millis(row.updated_at_ms)
                } else {
                    None
                };
                hits.push(SearchHit {
                    agent_id: row.agent_id,
                    native_id: row.native_id,
                    store_path: row.store_path,
                    title,
                    project_label: row.project_label,
                    project_key: row.project_key,
                    cwd_normalized: row.cwd_normalized,
                    updated_at,
                    match_kind: SearchMatchKind::Fts,
                    score,
                    snippet: Some(row.snippet),
                    maybe_live: row.maybe_live,
                    content_incomplete: row.maybe_live,
                });
            }
            // Re-rank after recency blend (SQL only ordered by bm25).
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.updated_at.cmp(&a.updated_at))
            });
            hits.truncate(limit as usize);
            Ok(hits)
        })
    }

    /// All (agent, native) keys currently in the index (for orphan cleanup).
    pub fn all_keys(&self) -> Result<Vec<(String, String)>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT agent_id, native_id FROM session_docs")
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(sql_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(sql_err)?);
            }
            Ok(out)
        })
    }
}

struct FtsRow {
    agent_id: String,
    native_id: String,
    store_path: String,
    title: String,
    project_label: String,
    project_key: String,
    cwd_normalized: Option<String>,
    maybe_live: bool,
    updated_at_ms: i64,
    snippet: String,
    bm25: f64,
}

fn bm25_to_score(bm25: f64) -> f32 {
    // SQLite bm25: more negative ≈ better. Clamp to 0..30.
    let strength = (-bm25).clamp(0.0, 20.0) as f32;
    (strength * 1.5).clamp(8.0, 30.0)
}

/// Build a safe FTS5 MATCH query: AND of quoted tokens, strip specials.
pub fn build_fts_query(raw: &str) -> String {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .filter_map(|t| {
            let cleaned: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                // Quote token; escape internal quotes by doubling.
                let esc = cleaned.replace('"', "\"\"");
                Some(format!("\"{esc}\""))
            }
        })
        .collect();
    tokens.join(" ")
}

fn sql_err(e: rusqlite::Error) -> PortalError {
    PortalError::Other(format!("search.db: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::SessionSummary;
    use chrono::Utc;

    fn summary(id: &str, title: &str) -> SessionSummary {
        SessionSummary {
            agent_id: "claude-code".into(),
            native_id: id.into(),
            project_key: "p".into(),
            title: Some(title.into()),
            cwd: Some("c:/work".into()),
            git_branch: None,
            model: None,
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
            message_count: Some(1),
            message_count_exact: true,
            size_bytes: 10,
            store_path: format!("/tmp/{id}"),
            maybe_live: false,
        }
    }

    #[test]
    fn fts_finds_body_token() {
        let dir = std::env::temp_dir().join(format!(
            "portal-fts-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = SearchStore::open(&dir).unwrap();
        let s = summary("s1", "My session");
        store
            .upsert_session(
                &s,
                "p",
                "agent-portal",
                Some("c:/work/agent-portal"),
                "1:2:3",
                "we need to fix the WormDB sidecar vector search pipeline",
            )
            .unwrap();
        let hits = store
            .search(&SearchQuery {
                q: "WormDB sidecar".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].native_id, "s1");
        assert_eq!(hits[0].match_kind, SearchMatchKind::Fts);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_fts_strips_specials() {
        assert_eq!(build_fts_query("foo bar"), "\"foo\" \"bar\"");
        assert_eq!(build_fts_query("a:b *c*"), "\"ab\" \"c\"");
    }
}
