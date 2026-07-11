//! App-local index cache.
//!
//! Enumerating every agent's store (peeking thousands of Claude transcripts,
//! walking Codex's date tree, probing `<cli> --version`) takes seconds. The
//! board shouldn't pay that on every open, so the last computed [`BoardSnapshot`]
//! is cached in a small SQLite file in the app's own data dir. The UI shows the
//! cache instantly, then refreshes in the background.
//!
//! This DB is the portal's alone — it never touches an agent's store.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::dto::BoardSnapshot;
use crate::error::{PortalError, Result};

pub struct IndexStore {
    path: PathBuf,
}

impl IndexStore {
    /// Open (creating if needed) the index at `<app_data_dir>/index.db`.
    pub fn open(app_data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(app_data_dir)?;
        let path = app_data_dir.join("index.db");
        let store = Self { path };
        store.with_conn(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS board_cache (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     snapshot_json TEXT NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS generated_titles (
                     agent_id TEXT NOT NULL,
                     native_id TEXT NOT NULL,
                     title TEXT NOT NULL,
                     source_revision TEXT NOT NULL,
                     updated_at INTEGER NOT NULL,
                     PRIMARY KEY (agent_id, native_id)
                 );",
            )
            .map_err(sql_err)
        })?;
        Ok(store)
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = Connection::open(&self.path).map_err(sql_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(sql_err)?;
        f(&conn)
    }

    /// The last cached board, or `None` on a cold start / unreadable cache.
    pub fn cached_board(&self) -> Option<BoardSnapshot> {
        self.with_conn(|conn| {
            let json: Option<String> = conn
                .query_row(
                    "SELECT snapshot_json FROM board_cache WHERE id = 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            Ok(json)
        })
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
    }

    /// Replace the cached board.
    pub fn store_board(&self, board: &BoardSnapshot) -> Result<()> {
        let json = serde_json::to_string(board).map_err(|e| PortalError::Other(e.to_string()))?;
        let ts = board.generated_at.timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO board_cache (id, snapshot_json, updated_at) VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET snapshot_json = ?1, updated_at = ?2",
                rusqlite::params![json, ts],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    pub fn upsert_generated_title(
        &self,
        agent_id: &str,
        native_id: &str,
        title: &str,
        source_revision: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("INSERT INTO generated_titles (agent_id,native_id,title,source_revision,updated_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(agent_id,native_id) DO UPDATE SET title=?3,source_revision=?4,updated_at=?5",
                rusqlite::params![agent_id,native_id,title,source_revision,chrono::Utc::now().timestamp_millis()]).map_err(sql_err)?;
            Ok(())
        })
    }

    /// 0 = never named, 1 = stale, 2 = current.
    pub fn title_refresh_priority(
        &self,
        agent_id: &str,
        native_id: &str,
        source_revision: &str,
    ) -> u8 {
        self.with_conn(|conn| {
            let revision: Option<String> = conn
                .query_row(
                    "SELECT source_revision FROM generated_titles WHERE agent_id=?1 AND native_id=?2",
                    rusqlite::params![agent_id, native_id],
                    |r| r.get(0),
                )
                .ok();
            Ok(match revision { None => 0, Some(r) if r != source_revision => 1, Some(_) => 2 })
        })
        .unwrap_or(0)
    }

    /// Every generated title on record, for monitoring the naming worker.
    pub fn all_generated_titles(&self) -> Result<Vec<StoredTitle>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT agent_id,native_id,title,source_revision,updated_at FROM generated_titles",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(StoredTitle {
                        agent_id: r.get(0)?,
                        native_id: r.get(1)?,
                        title: r.get(2)?,
                        source_revision: r.get(3)?,
                        updated_at: r.get(4)?,
                    })
                })
                .map_err(sql_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(sql_err)?);
            }
            Ok(out)
        })
    }

    pub fn apply_generated_titles(&self, board: &mut BoardSnapshot) -> Result<()> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT title,source_revision FROM generated_titles WHERE agent_id=?1 AND native_id=?2").map_err(sql_err)?;
            for lane in &mut board.lanes { for project in &mut lane.projects { for session in &mut project.sessions {
                let row: Option<(String,String)> = stmt.query_row(rusqlite::params![session.agent_id,session.native_id], |r| Ok((r.get(0)?,r.get(1)?))).ok();
                if let Some((title,_)) = row.filter(|(_,revision)| *revision == session_revision(session)) { session.title = Some(title); }
            }}}
            Ok(())
        })
    }
}

/// A row of the `generated_titles` table.
pub struct StoredTitle {
    pub agent_id: String,
    pub native_id: String,
    pub title: String,
    pub source_revision: String,
    /// Unix milliseconds when the title was written.
    pub updated_at: i64,
}

pub fn session_revision(session: &crate::dto::SessionSummary) -> String {
    format!(
        "{}:{}:{}",
        session.size_bytes,
        session
            .updated_at
            .map(|t| t.timestamp_millis())
            .unwrap_or_default(),
        session.message_count.unwrap_or_default()
    )
}

fn sql_err(e: rusqlite::Error) -> PortalError {
    PortalError::Other(format!("index cache: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::BoardSnapshot;

    #[test]
    fn round_trips_a_board_snapshot() {
        let dir = std::env::temp_dir().join(format!("portal-idx-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let store = IndexStore::open(&dir).unwrap();
        assert!(store.cached_board().is_none(), "cold cache is empty");

        let board = BoardSnapshot {
            lanes: vec![],
            feasibility: vec![],
            generated_at: chrono::Utc::now(),
        };
        store.store_board(&board).unwrap();

        // A second store over the same file reads the cache (persistence).
        let reopened = IndexStore::open(&dir).unwrap();
        assert!(reopened.cached_board().is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generated_titles_overlay_board_without_touching_native_titles() {
        let dir = std::env::temp_dir().join(format!("portal-title-{}", uuid::Uuid::now_v7()));
        let store = IndexStore::open(&dir).unwrap();
        store
            .upsert_generated_title("codex", "s1", "Fix migration verification", "42:0:0")
            .unwrap();
        let mut board = BoardSnapshot {
            lanes: vec![crate::dto::Lane {
                agent: crate::dto::AgentDescriptor {
                    id: "codex".into(),
                    display_name: "Codex".into(),
                    capabilities: crate::dto::Capabilities {
                        store_kind: crate::dto::StoreKind::JsonlPerSession,
                        read: crate::dto::SupportLevel::Full,
                        write_native: crate::dto::SupportLevel::Full,
                        watch: false,
                        launch_resume: true,
                        launch_new: true,
                        context_tokens: None,
                        write_confidence: None,
                        version_range_tested: String::new(),
                        notes: vec![],
                    },
                    installation: None,
                },
                projects: vec![crate::dto::ProjectGroup {
                    key: "p".into(),
                    label: "p".into(),
                    cwd_normalized: None,
                    sessions: vec![crate::dto::SessionSummary {
                        agent_id: "codex".into(),
                        native_id: "s1".into(),
                        project_key: "p".into(),
                        title: Some("initial prompt".into()),
                        cwd: None,
                        git_branch: None,
                        model: None,
                        created_at: None,
                        updated_at: None,
                        message_count: None,
                        message_count_exact: false,
                        size_bytes: 42,
                        store_path: "x".into(),
                        maybe_live: false,
                    }],
                }],
                error: None,
            }],
            feasibility: vec![],
            generated_at: chrono::Utc::now(),
        };
        store.apply_generated_titles(&mut board).unwrap();
        assert_eq!(
            board.lanes[0].projects[0].sessions[0].title.as_deref(),
            Some("Fix migration verification")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn all_generated_titles_lists_every_row() {
        let dir = std::env::temp_dir().join(format!("portal-titles-{}", uuid::Uuid::now_v7()));
        let store = IndexStore::open(&dir).unwrap();
        assert!(store.all_generated_titles().unwrap().is_empty());

        store
            .upsert_generated_title("codex", "s1", "First title", "1:0:0")
            .unwrap();
        store
            .upsert_generated_title("claude-code", "s2", "Second title", "2:0:0")
            .unwrap();
        // Re-titling the same session updates in place, not appends.
        store
            .upsert_generated_title("codex", "s1", "First title, refreshed", "3:0:0")
            .unwrap();

        let rows = store.all_generated_titles().unwrap();
        assert_eq!(rows.len(), 2);
        let s1 = rows.iter().find(|t| t.native_id == "s1").unwrap();
        assert_eq!(s1.title, "First title, refreshed");
        assert_eq!(s1.source_revision, "3:0:0");
        std::fs::remove_dir_all(dir).ok();
    }
}
