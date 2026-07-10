//! Append-only migration ledger. JSONL for now (one file in the app data
//! dir); moves into the SQLite index when that lands. Every migration records
//! exactly which artifacts it created so undo is always a bounded delete.

use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::Result;
use crate::migration::types::{VerifyGrade, WrittenArtifact};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    pub id: String,
    pub at: DateTime<Utc>,
    pub source_agent: String,
    pub source_native_id: String,
    pub source_path: String,
    pub target_agent: String,
    pub target_native_id: String,
    pub artifacts: Vec<WrittenArtifact>,
    pub verify_grade: VerifyGrade,
    pub undone: bool,
}

pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self {
            path: app_data_dir.join("migrations.jsonl"),
        }
    }

    pub fn append(&self, entry: &LedgerEntry) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut line = serde_json::to_string(entry)
            .map_err(|e| crate::error::PortalError::Other(e.to_string()))?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<LedgerEntry>> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Ok(Vec::new());
        };
        Ok(raw
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<Option<LedgerEntry>> {
        Ok(self.list()?.into_iter().find(|e| e.id == id))
    }

    /// Flip an entry's `undone` flag. The ledger is append-only in spirit but
    /// small, so a full rewrite is fine.
    pub fn mark_undone(&self, id: &str) -> Result<()> {
        let mut entries = self.list()?;
        for entry in &mut entries {
            if entry.id == id {
                entry.undone = true;
            }
        }
        let mut body = String::new();
        for entry in &entries {
            body.push_str(
                &serde_json::to_string(entry)
                    .map_err(|e| crate::error::PortalError::Other(e.to_string()))?,
            );
            body.push('\n');
        }
        crate::util::paths::atomic_write(&self.path, body.as_bytes())?;
        Ok(())
    }
}
