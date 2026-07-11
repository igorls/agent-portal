//! Antigravity enumeration index: conversation_id -> {title, workspace}.
//!
//! Two plaintext sources feed it — the IDE's `agyhub_summaries_proto.pb`
//! (a protobuf) and the CLI's `conversation_summaries.db` (SQLite). Both carry
//! title + workspace uri + timestamp per conversation. Legacy conversations
//! with no summary fall back to file mtime + an id-derived title.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use portal_core::util::paths::file_uri_to_path;

use super::proto::Msg;

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub title: Option<String>,
    pub workspace: Option<String>,
    pub updated_ms: Option<i64>,
    pub step_count: Option<u32>,
    /// A subagent conversation (has a parent / an agent role) — hidden from the
    /// board like Claude's sidechains.
    pub is_subagent: bool,
}

pub type Index = HashMap<String, Summary>;

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.as_bytes()[8] == b'-' && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Build the index from all available summary sources under `~/.gemini`.
pub fn build_index(gemini: &Path) -> Index {
    let mut index = Index::new();
    parse_ide_pb(
        &gemini.join("antigravity").join("agyhub_summaries_proto.pb"),
        &mut index,
    );
    parse_cli_db(
        &gemini
            .join("antigravity-cli")
            .join("conversation_summaries.db"),
        &mut index,
    );
    index
}

/// The IDE protobuf: a flat list of conversation entries. Each entry
/// (a submessage) carries a uuid id, a title, and a `file:///` workspace uri;
/// subagents additionally carry an agent role name. Field numbers vary, so we
/// extract by shape: within each entry submessage, the uuid string is the id,
/// the first `file:///` is the workspace, and the first plain non-uri/non-uuid
/// string is the title.
fn parse_ide_pb(path: &Path, index: &mut Index) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let top = Msg::decode(&bytes);
    for (_, val) in &top.fields {
        let entry = val.as_msg();
        if entry.fields.is_empty() {
            continue;
        }
        let mut strings = Vec::new();
        entry.strings(&mut strings);
        let id = strings.iter().find(|s| is_uuid(s)).cloned();
        let Some(id) = id else { continue };
        let workspace = strings
            .iter()
            .find(|s| s.starts_with("file:///"))
            .and_then(|u| file_uri_to_path(u));
        // title: first plain string that isn't a uuid, uri, url, or a lone keyword
        let title = strings
            .iter()
            .find(|s| {
                !is_uuid(s)
                    && !s.starts_with("file:")
                    && !s.starts_with("http")
                    && s.contains(' ')
                    && s.len() > 4
            })
            .cloned();
        // an agent role like "research"/"browser" near the top marks a subagent
        let is_subagent = strings.iter().take(4).any(|s| {
            matches!(
                s.as_str(),
                "research" | "browser" | "planner" | "coder" | "reviewer"
            )
        });
        index.entry(id).or_insert(Summary {
            title,
            workspace,
            updated_ms: None,
            step_count: None,
            is_subagent,
        });
    }
}

fn parse_cli_db(path: &Path, index: &mut Index) {
    if !path.is_file() {
        return;
    }
    let Ok(conn) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return;
    };
    conn.busy_timeout(std::time::Duration::from_secs(2)).ok();
    let Ok(mut stmt) = conn.prepare(
        "SELECT conversation_id, title, workspace_uris, last_modified_time, step_count, parent_conversation_id, nesting_depth FROM conversation_summaries",
    ) else {
        return;
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<i64>>(3).unwrap_or(None),
            r.get::<_, Option<i64>>(4).unwrap_or(None),
            r.get::<_, Option<String>>(5).unwrap_or(None),
            r.get::<_, Option<i64>>(6).unwrap_or(None),
        ))
    });
    let Ok(rows) = rows else { return };
    for row in rows.flatten() {
        let (id, title, ws, modified, steps, parent, depth) = row;
        // workspace_uris may be a JSON array of uris; take the first file:/// one
        let workspace = ws
            .as_deref()
            .and_then(first_workspace_uri)
            .and_then(|u| file_uri_to_path(&u));
        index.insert(
            id,
            Summary {
                title: title.filter(|t| !t.is_empty()),
                workspace,
                updated_ms: modified,
                step_count: steps.map(|s| s as u32),
                is_subagent: parent.is_some() || depth.unwrap_or(0) > 0,
            },
        );
    }
}

fn first_workspace_uri(raw: &str) -> Option<String> {
    // raw may be `["file:///p:/x"]` or a bare `file:///p:/x`
    if let Some(start) = raw.find("file:///") {
        let tail = &raw[start..];
        let end = tail.find(['"', ']', ',']).unwrap_or(tail.len());
        return Some(tail[..end].to_string());
    }
    None
}
