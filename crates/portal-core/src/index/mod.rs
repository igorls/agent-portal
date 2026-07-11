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
}
