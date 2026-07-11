//! OpenCode adapter (SQLite store).
//!
//! Unlike the JSONL agents, OpenCode keeps everything in a Drizzle SQLite
//! database (`~/.local/share/opencode/opencode.db`): a `session` row, `message`
//! rows (with a JSON `data` blob), and `part` rows (JSON `data`, one row per
//! content block). Two quirks the IR has to absorb:
//!   - a `tool` part carries the call AND its result together
//!     (`{tool, callID, state:{input, output, status}}`), so one part becomes a
//!     ToolCall + a ToolResult block;
//!   - `reasoning` parts keep readable text (not just an encrypted blob).
//!
//! v1 is read + brief-target. Native write would go through OpenCode's own
//! `opencode import` (it owns id generation, schema, and FKs) rather than raw
//! row inserts into the user's live DB — deferred.

mod read;

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::{PortalError, Result};
use portal_core::ir::CanonicalSession;
use portal_core::migration::types::CommandSpec;
use portal_core::util::paths::{cli_version, find_cli};

pub const ID: &str = "opencode";

pub struct OpenCodeAdapter;

impl AgentAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::Sqlite,
            read: SupportLevel::Full,
            write_native: SupportLevel::None, // deferred to `opencode import`
            watch: true,
            launch_resume: true,
            launch_new: true,
            write_confidence: None,
            version_range_tested: "1.17.x".to_string(),
            notes: vec![
                "Sessions live in a SQLite DB (opencode.db)".to_string(),
                "A migration target via handoff brief; native import is planned".to_string(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let default_db = env
            .home
            .join(".local")
            .join("share")
            .join("opencode")
            .join("opencode.db");
        let db = env.store_root(ID, default_db);
        let cli = find_cli(&env.path_dirs, "opencode");
        if !db.is_file() && cli.is_none() {
            return None;
        }
        let version = cli.as_deref().and_then(|c| cli_version(c, "--version"));
        Some(Installation {
            cli_path: cli.map(|p| p.display().to_string()),
            version,
            store_root: db.display().to_string(),
        })
    }

    fn list_projects(&self, inst: &Installation) -> Result<Vec<ProjectRef>> {
        Ok(self
            .snapshot(inst)?
            .into_iter()
            .map(|(project, _)| project)
            .collect())
    }

    fn list_sessions(
        &self,
        inst: &Installation,
        project: &ProjectRef,
    ) -> Result<Vec<SessionSummary>> {
        Ok(self
            .snapshot(inst)?
            .into_iter()
            .find(|(p, _)| p.key == project.key)
            .map(|(_, sessions)| sessions)
            .unwrap_or_default())
    }

    fn read_session(
        &self,
        inst: &Installation,
        locator: &SessionLocator,
    ) -> Result<CanonicalSession> {
        let conn = open_ro(&inst.store_root)?;
        read::read_session(&conn, &locator.native_id)
    }

    fn snapshot(&self, inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
        let conn = open_ro(&inst.store_root)?;
        read::snapshot(&conn)
    }

    fn resume_command(
        &self,
        _inst: &Installation,
        _native_id: &str,
        cwd: &str,
    ) -> Result<CommandSpec> {
        // OpenCode has no session-specific CLI resume; open the TUI in the
        // project (its picker lists recent sessions).
        Ok(CommandSpec {
            program: "opencode".to_string(),
            args: vec![cwd.to_string()],
            cwd: cwd.to_string(),
        })
    }

    fn new_session_command(
        &self,
        _inst: &Installation,
        cwd: &str,
        initial_prompt: &str,
    ) -> Result<CommandSpec> {
        Ok(CommandSpec {
            program: "opencode".to_string(),
            args: vec!["run".to_string(), initial_prompt.to_string()],
            cwd: cwd.to_string(),
        })
    }
}

/// Open the store read-only. Plain READ_ONLY (not `immutable`) so we see data
/// in the WAL too — OpenCode writes in WAL mode, and an immutable open would
/// read a stale snapshot that misses recent sessions and still shows deleted
/// ones. READ_ONLY can never mutate the DB, and WAL permits concurrent readers
/// so a running OpenCode doesn't block us.
pub(crate) fn open_ro(store_root: &str) -> Result<Connection> {
    let path = PathBuf::from(store_root);
    if !path.is_file() {
        return Err(PortalError::Other(format!(
            "opencode.db not found at {store_root}"
        )));
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| PortalError::Other(format!("opening opencode.db: {e}")))?;
    // Busy-wait briefly rather than erroring if OpenCode is mid-commit.
    let _ = conn.busy_timeout(std::time::Duration::from_millis(2000));
    Ok(conn)
}
