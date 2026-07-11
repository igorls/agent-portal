//! Antigravity `steps` protobuf -> IR (best-effort).
//!
//! The step model is rich and undocumented; v1 preserves all readable content:
//! user text, assistant text, and tool activity (as text — tool call/result
//! pairing isn't reliably recoverable). Fidelity is Partial, which is honest
//! and plenty for a read/brief source.

use std::path::Path;

use chrono::Utc;
use rusqlite::{Connection, OpenFlags};

use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::proto::Msg;
use super::ID;

pub fn read_session(db_path: &Path, id: &str, cwd: Option<String>) -> Result<CanonicalSession> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| PortalError::Other(format!("open antigravity db: {e}")))?;
    conn.busy_timeout(std::time::Duration::from_secs(3)).ok();

    let mut stmt = conn
        .prepare("SELECT idx, step_type, step_payload FROM steps ORDER BY idx ASC")
        .map_err(|e| PortalError::Other(format!("antigravity steps: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })
        .map_err(|e| PortalError::Other(format!("antigravity steps: {e}")))?;

    let mut timeline = Vec::new();
    let mut tool_steps = 0usize;
    let mut skipped = 0usize;

    for row in rows {
        let (idx, _step_type, payload) =
            row.map_err(|e| PortalError::Other(format!("antigravity step row: {e}")))?;
        let Some(payload) = payload else {
            continue;
        };
        match extract(&payload) {
            Some((role, text, is_tool)) => {
                if is_tool {
                    tool_steps += 1;
                }
                timeline.push(Turn {
                    id: format!("s{idx}"),
                    parent_id: None,
                    role,
                    timestamp: None,
                    model: None,
                    is_meta: false,
                    blocks: vec![Block::Text { text }],
                    usage: None,
                    raw: None,
                });
            }
            None => skipped += 1,
        }
    }

    let cwd = cwd.unwrap_or_default();
    let title = timeline
        .iter()
        .find(|t| t.role == Role::User)
        .and_then(|t| t.blocks.first())
        .and_then(|b| match b {
            Block::Text { text } => Some(truncate(text, 80)),
            _ => None,
        });

    let mut losses = vec![LossNote {
        code: LossCode::ToolPairingIncomplete,
        detail: format!(
            "{tool_steps} tool step(s) preserved as text (Antigravity's step model isn't fully decoded)"
        ),
        turn_id: None,
    }];
    if skipped > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!("{skipped} step(s) had no extractable text"),
            turn_id: None,
        });
    }

    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: id.to_string(),
            agent_id: ID.to_string(),
            store_path: db_path.display().to_string(),
            agent_version: None,
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&cwd),
            project_label: label_from_cwd(&cwd),
            cwd,
            git_branch: None,
        },
        title,
        timeline,
        attachments: Vec::new(),
        usage: UsageTotals::default(),
        losses,
        fidelity: Fidelity::Partial,
    })
}

/// Pull (role, text, is_tool) out of a step payload. Returns None if there's
/// nothing human-readable in it.
fn extract(payload: &[u8]) -> Option<(Role, String, bool)> {
    let m = Msg::decode(payload);

    // Tool step: #5 -> #4 -> { #2 name, #3 args }
    if let Some(inner) = m.msg(5).map(|s| s.msg(4).unwrap_or_default()) {
        if let Some(name) = inner.str(2) {
            if !name.is_empty() && name.len() < 60 {
                let args = inner.str(3).map(|a| truncate(&a, 240)).unwrap_or_default();
                let text = if args.is_empty() {
                    format!("⚙ {name}")
                } else {
                    format!("⚙ {name}: {args}")
                };
                return Some((Role::Assistant, text, true));
            }
        }
    }

    // User / task step: #19 -> #2 | #3
    if let Some(u) = m.msg(19) {
        if let Some(t) = u.str(2).or_else(|| u.str(3)) {
            if is_texty(&t) {
                return Some((Role::User, t, false));
            }
        }
    }

    // Assistant step: #20 -> #3 | #2
    if let Some(a) = m.msg(20) {
        if let Some(t) = a.str(3).or_else(|| a.str(2)) {
            if is_texty(&t) {
                return Some((Role::Assistant, t, false));
            }
        }
    }

    // Fallback: the longest natural-language string anywhere in the step.
    let mut strings = Vec::new();
    m.strings(&mut strings);
    strings
        .into_iter()
        .filter(|s| is_texty(s))
        .max_by_key(|s| s.len())
        .map(|t| (Role::Assistant, t, false))
}

fn is_texty(s: &str) -> bool {
    let t = s.trim();
    t.len() >= 20
        && t.contains(' ')
        && !t.starts_with('{')
        && !t.starts_with('[')
        && !t.starts_with("file:")
        && !t.starts_with("http")
        && !(t.len() == 36 && t.as_bytes().get(8) == Some(&b'-'))
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    let mut out: String = t.chars().take(max).collect();
    if t.chars().count() > max {
        out.push('…');
    }
    out
}
