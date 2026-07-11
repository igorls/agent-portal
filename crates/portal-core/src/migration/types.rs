use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ir::LossNote;

/// A launchable command, platform-agnostic. The launcher decides how to open
/// a terminal around it; the UI shows it for copy-paste recovery.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
}

impl CommandSpec {
    /// Human-readable single line (double-quoted args). For display and for
    /// POSIX shells.
    pub fn display(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        }));
        parts.join(" ")
    }

    /// PowerShell command line with single-quoted args (safe for prompts that
    /// contain spaces, backslashes, and paths). Passed as `pwsh -Command`.
    pub fn pwsh_line(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(
            self.args
                .iter()
                .map(|a| format!("'{}'", a.replace('\'', "''"))),
        );
        parts.join(" ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// Title to register in the target's index/metadata where supported.
    pub title: Option<String>,
}

/// What a migration actually does. Native converts to a resumable session;
/// Brief writes a handoff document and starts a fresh target session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MigrationKind {
    Native,
    Brief,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A file this migration created; undo deletes it.
    File,
    /// A line appended to a shared index file; undo restores the backup.
    IndexAppendLine,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WrittenArtifact {
    pub kind: ArtifactKind,
    pub path: String,
    pub backup: Option<String>,
    /// Content fingerprint at write time (File artifacts). Undo compares
    /// against it to detect whether the target agent has since continued the
    /// session, in which case the file is left alone.
    #[serde(default)]
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WrittenSession {
    pub native_id: String,
    pub primary_path: String,
    pub artifacts: Vec<WrittenArtifact>,
}

/// What a writer predicts it would produce, without touching disk.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WritePlan {
    pub predicted_losses: Vec<LossNote>,
    pub target_path_hint: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BlockCensus {
    pub text: u32,
    pub thinking: u32,
    pub tool_calls: u32,
    pub tool_results: u32,
    pub compaction: u32,
    pub meta: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DryRunReport {
    pub plan_id: String,
    pub kind: MigrationKind,
    pub source_agent: String,
    pub source_native_id: String,
    pub source_title: Option<String>,
    pub target_agent: String,
    pub cwd: String,
    pub turn_count: u32,
    pub census: BlockCensus,
    pub predicted_losses: Vec<LossNote>,
    pub unanswered_tool_calls: u32,
    /// Rough token estimate of the session's transferable content (chars/4).
    pub estimated_tokens: u32,
    /// The target's approximate default context window, when known — for the UI
    /// to show alongside the estimate.
    pub target_context_tokens: Option<u32>,
    pub warnings: Vec<String>,
    pub target_path_hint: String,
    pub resume_preview: String,
    /// Brief mode only: the exact handoff text that will be written.
    pub brief_preview: Option<String>,
    /// Brief mode only: whether the preview was polished by a local model.
    pub brief_enhanced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum VerifyGrade {
    Exact,
    Equivalent,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub grade: VerifyGrade,
    pub compared_blocks: u32,
    pub diffs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MigrationResult {
    pub migration_id: String,
    pub kind: MigrationKind,
    pub target_agent: String,
    /// Native: the new session id. Brief: the handoff document path.
    pub target_native_id: String,
    pub target_path: String,
    /// Present for native migrations (read-back verification); None for briefs.
    pub verify: Option<VerifyReport>,
    pub resume_command: CommandSpec,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct UndoReport {
    pub migration_id: String,
    pub removed: Vec<String>,
    /// Artifacts left in place, with why (e.g. the agent continued the session).
    pub skipped: Vec<String>,
}
