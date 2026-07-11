use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Smoke DTO: proves the Rust -> ts-rs -> Angular typed IPC pipeline end to end.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub app_version: String,
    pub adapters_registered: u32,
}

/// How an agent persists sessions on disk. Drives which adapter machinery applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    JsonlPerSession,
    DirPerSession,
    Sqlite,
    Protobuf,
    CloudApi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SupportLevel {
    Full,
    Partial,
    None,
}

/// Static, declarative description of what an adapter can do.
/// The feasibility matrix and all UI gating derive from this.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub store_kind: StoreKind,
    pub read: SupportLevel,
    pub write_native: SupportLevel,
    pub watch: bool,
    pub launch_resume: bool,
    /// Can start a fresh session seeded with a prompt (enables brief-mode as a
    /// migration target).
    pub launch_new: bool,
    /// Approximate default context window (tokens) of this agent when it resumes
    /// a native session. Used only as a soft advisory: a session estimated well
    /// above this may not load, and the wizard nudges toward a brief. `None`
    /// when the window is too model-dependent to guess (e.g. OpenCode). It is a
    /// conservative default — larger-context models (e.g. Claude Sonnet 1M) fit
    /// more.
    pub context_tokens: Option<u32>,
    /// Human confidence in the native writer: "High" | "Medium" | "Experimental".
    pub write_confidence: Option<String>,
    /// Agent versions this adapter has fixtures/verification for, e.g. "2.0–2.1.x".
    pub version_range_tested: String,
    pub notes: Vec<String>,
}

/// Result of detecting an agent on this machine.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub cli_path: Option<String>,
    pub version: Option<String>,
    pub store_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AgentDescriptor {
    pub id: String,
    pub display_name: String,
    pub capabilities: Capabilities,
    pub installation: Option<Installation>,
}

/// An agent-native project/workspace grouping (directory slug, cwd hash, …).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRef {
    /// Agent-native key (e.g. Claude's dir slug `P--agent-portal`).
    pub key: String,
    /// Real workspace path when recoverable. Never derived by inverting slugs.
    pub cwd: Option<String>,
    pub label: String,
}

/// Cheap card-level metadata for one session. Produced by head/tail peeking
/// only — building one of these must never require parsing a whole transcript.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub agent_id: String,
    pub native_id: String,
    pub project_key: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub message_count: Option<u32>,
    /// False when message_count is an extrapolation from newline sampling.
    pub message_count_exact: bool,
    /// Serialized as a JSON number over IPC; sessions never approach 2^53.
    #[ts(type = "number")]
    pub size_bytes: u64,
    pub store_path: String,
    /// Heuristic: the owning agent may currently be appending to this session.
    pub maybe_live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGroup {
    pub key: String,
    pub label: String,
    /// Forward-slash, drive-letter-lowercased path used to align the same
    /// project across lanes of different agents.
    pub cwd_normalized: Option<String>,
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Lane {
    pub agent: AgentDescriptor,
    pub projects: Vec<ProjectGroup>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BoardSnapshot {
    pub lanes: Vec<Lane>,
    pub feasibility: Vec<PairFeasibility>,
    pub generated_at: DateTime<Utc>,
}

/// Precomputed once per board: for every ordered (source, target) pair of
/// detected agents, which migration modes are possible. Drives drag-drop
/// gating (droppable if either is true) and the wizard's mode choice.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PairFeasibility {
    pub source: String,
    pub target: String,
    /// Convert to a resumable session with history.
    pub native: bool,
    /// Write a handoff brief and launch a fresh session.
    pub brief: bool,
    /// Confidence surfaced from the target adapter (High/Medium/Experimental).
    pub write_confidence: Option<String>,
}

/// Snapshot of the background session-naming worker, surfaced in the Activity
/// view so the local-LLM titling can be watched and understood.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct NamingReport {
    /// A local Ollama server is reachable.
    pub ollama_available: bool,
    /// The model configured for naming.
    pub model: String,
    /// That model is installed on the server. Naming stays idle without it.
    pub model_present: bool,
    /// Length of the recency window (hours) the worker prioritizes.
    pub window_hours: u32,
    /// Counts for sessions touched within the recency window — the worker's
    /// actual queue, and what the panel leads with.
    pub recent: NamingCounts,
    /// Counts across the whole session library — context, not an active backlog.
    pub overall: NamingCounts,
    /// Live state of the worker (running now, timing of passes).
    pub progress: NamingProgress,
    /// Generated titles for sessions still on the board, newest first.
    pub entries: Vec<NamingEntry>,
}

/// Named / stale / pending breakdown of a set of sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct NamingCounts {
    /// Sessions in this set.
    pub total: u32,
    /// With an up-to-date generated title.
    pub named: u32,
    /// With a title that went stale (the session changed since).
    pub stale: u32,
    /// Never named yet.
    pub pending: u32,
}

/// Live state of the background naming worker. Distinguishes "actively naming
/// right now" from "queued work, but the worker is asleep between passes" —
/// the two look identical from counts alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct NamingProgress {
    /// A pass is running right now (the model is being called).
    pub active: bool,
    /// Label of the session being named this instant, when active.
    pub current: Option<String>,
    /// Project of the session being named this instant, when active.
    pub current_project: Option<String>,
    /// Sessions named so far in the current (or most recent) pass.
    pub batch_done: u32,
    /// Sessions the current (or most recent) pass set out to name.
    pub batch_total: u32,
    /// When the last pass finished.
    pub last_pass_at: Option<DateTime<Utc>>,
    /// When the next pass is scheduled to start.
    pub next_pass_at: Option<DateTime<Utc>>,
}

/// One generated session title.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct NamingEntry {
    pub agent_id: String,
    pub native_id: String,
    /// Project (workspace) the session belongs to.
    pub project: String,
    pub title: String,
    /// The title matches the session's current revision (else it is stale).
    pub current: bool,
    /// When the worker last wrote this title.
    pub updated_at: DateTime<Utc>,
}
