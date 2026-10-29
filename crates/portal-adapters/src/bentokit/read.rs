use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use portal_core::adapter::SessionLocator;
use portal_core::dto::{Installation, ProjectRef, SessionSummary};
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, TokenUsage, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::normalize_cwd;

use super::ID;

fn query_error(error: rusqlite::Error) -> PortalError {
    PortalError::Other(format!("BentoKit query: {error}"))
}

fn open_ro(path: &Path) -> Result<Connection> {
    let conn =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(query_error)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(query_error)?;
    // No immutable=1: sessions in a running Core's WAL must be visible.
    Ok(conn)
}

fn safe_component(value: &str) -> bool {
    !value.is_empty() && !matches!(value, "." | "..") && !value.contains(['/', '\\', ':'])
}

pub(super) fn split_id(id: &str) -> Result<(&str, &str)> {
    id.split_once('/')
        .filter(|(agent, session)| safe_component(agent) && safe_component(session))
        .ok_or_else(|| {
            PortalError::Other("BentoKit session id must be <agent>/<conversation>".into())
        })
}

fn db_path(root: &Path, agent: &str) -> Result<PathBuf> {
    let path = root
        .join("data/agents")
        .join(agent)
        .join("conversations.db");
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root.canonicalize()?) {
        return Err(PortalError::Other(
            "BentoKit database is outside the data directory".into(),
        ));
    }
    Ok(canonical)
}

fn profiles(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let entries = match std::fs::read_dir(root.join("data/agents")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let agent = entry.file_name().to_string_lossy().into_owned();
        if safe_component(&agent) && entry.path().join("conversations.db").is_file() {
            out.push((agent.clone(), db_path(root, &agent)?));
        }
    }
    out.sort();
    Ok(out)
}

#[derive(Clone)]
struct Project {
    name: String,
    cwd: Option<String>,
}

fn projects(root: &Path) -> Result<HashMap<String, Project>> {
    let path = root.join("data/projects.db");
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    if !path.canonicalize()?.starts_with(root.canonicalize()?) {
        return Err(PortalError::Other(
            "BentoKit projects database is outside the data directory".into(),
        ));
    }
    let conn = open_ro(&path)?;
    let mut stmt = conn
        .prepare("SELECT id, name, source_type, uri FROM projects")
        .map_err(query_error)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(query_error)?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, name, source, uri) = row.map_err(query_error)?;
        // SSH and Git URIs are not local working directories.
        let cwd = (source == "local").then(|| local_path(&uri)).flatten();
        out.insert(id, Project { name, cwd });
    }
    Ok(out)
}

fn local_path(uri: &str) -> Option<String> {
    if let Some(path) = uri.strip_prefix("file:///") {
        let mut bytes = Vec::new();
        let mut chars = path.as_bytes().iter().copied();
        while let Some(c) = chars.next() {
            bytes.push(if c == b'%' {
                let hi = (chars.next()? as char).to_digit(16)?;
                let lo = (chars.next()? as char).to_digit(16)?;
                (hi * 16 + lo) as u8
            } else {
                c
            });
        }
        let path = String::from_utf8(bytes).ok()?;
        return Some(if path.as_bytes().get(1) == Some(&b':') {
            path
        } else {
            format!("/{path}")
        });
    }
    // Path::is_absolute on Unix does not recognize Windows drive paths.
    (Path::new(uri).is_absolute() || uri.as_bytes().get(1) == Some(&b':')).then(|| uri.to_string())
}

fn project_ref(
    root: &Path,
    agent: &str,
    project_id: Option<&str>,
    index: &HashMap<String, Project>,
) -> ProjectRef {
    if let Some(id) = project_id {
        let project = index.get(id);
        let cwd = project.and_then(|p| p.cwd.clone());
        return ProjectRef {
            key: cwd
                .as_deref()
                .map(normalize_cwd)
                .unwrap_or_else(|| format!("bentokit/{agent}/project/{id}")),
            cwd,
            label: project
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("BentoKit {agent} · {id}")),
        };
    }
    let cwd = root
        .join("agents")
        .join(agent)
        .join("workspace")
        .display()
        .to_string();
    ProjectRef {
        key: normalize_cwd(&cwd),
        cwd: Some(cwd),
        label: format!("BentoKit · {agent}"),
    }
}

fn parse_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

/// Board enumeration reads headers and indexed counts, never message bodies.
pub(super) fn snapshot(inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
    let root = Path::new(&inst.store_root);
    let index = projects(root)?;
    let mut grouped: BTreeMap<String, (ProjectRef, Vec<SessionSummary>)> = BTreeMap::new();
    for (agent, path) in profiles(root)? {
        let conn = open_ro(&path)?;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.title, c.project_id, c.created_at, c.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id AND m.is_deleted = 0),
                    (SELECT CASE WHEN json_valid(m.metadata) THEN json_extract(m.metadata, '$.modelInfo.modelName') END
                     FROM messages m WHERE m.conversation_id = c.id AND m.is_deleted = 0 AND m.role = 'assistant'
                     ORDER BY m.timestamp DESC, COALESCE(m.seq, m.rowid) DESC LIMIT 1)
             FROM conversations c WHERE c.is_deleted = 0 AND c.agent_id = ?1"
        ).map_err(query_error)?;
        let rows = stmt
            .query_map([&agent], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, u32>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(query_error)?;
        for row in rows {
            let (id, title, project_id, created, updated, count, model) =
                row.map_err(query_error)?;
            let project = project_ref(root, &agent, project_id.as_deref(), &index);
            let updated_at = parse_ts(&updated);
            let summary = SessionSummary {
                agent_id: ID.into(),
                native_id: format!("{agent}/{id}"),
                project_key: project.key.clone(),
                title,
                cwd: project.cwd.clone(),
                git_branch: None,
                model,
                created_at: parse_ts(&created),
                updated_at,
                message_count: Some(count),
                message_count_exact: true,
                // The shared DB size is not a session's size.
                size_bytes: 0,
                store_path: path.display().to_string(),
                maybe_live: updated_at.is_some_and(|ts| {
                    (0..120).contains(&Utc::now().signed_duration_since(ts).num_seconds())
                }),
            };
            grouped
                .entry(project.key.clone())
                .or_insert_with(|| (project, Vec::new()))
                .1
                .push(summary);
        }
    }
    Ok(grouped.into_values().collect())
}

pub(super) fn read_session(
    inst: &Installation,
    locator: &SessionLocator,
) -> Result<CanonicalSession> {
    let (agent, id) = split_id(&locator.native_id)?;
    let root = Path::new(&inst.store_root);
    // Ignore the optional path hint: the qualified id determines the only
    // allowed database, so an IPC caller cannot redirect this reader.
    let path = db_path(root, agent)?;
    let conn = open_ro(&path)?;
    let tx = conn.unchecked_transaction().map_err(query_error)?;
    let (title, project_id, metadata) = tx
        .query_row(
            "SELECT title, project_id, metadata FROM conversations
         WHERE id = ?1 AND agent_id = ?2 AND is_deleted = 0",
            [id, agent],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(query_error)?;
    let project = project_ref(root, agent, project_id.as_deref(), &projects(root)?);
    let cwd = project.cwd.unwrap_or_default();
    let mut stmt = tx
        .prepare(
            "SELECT id, role, content, timestamp, metadata FROM messages
         WHERE conversation_id = ?1 AND is_deleted = 0
         ORDER BY COALESCE(CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.historySequence') END,2147483647), timestamp ASC, COALESCE(seq, rowid) ASC",
        )
        .map_err(query_error)?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(query_error)?;
    let mut timeline = Vec::new();
    let mut usage = UsageTotals::default();
    let mut losses = vec![LossNote {
        code: LossCode::ContentSkipped,
        detail: "BentoKit profile configuration, ownership, permissions, and runtime state are not transferred; tool arguments are display traces".into(),
        turn_id: None,
    }];
    for row in rows {
        let (id, role, content, timestamp, metadata) = row.map_err(query_error)?;
        let meta = parse_metadata(metadata.as_deref(), &path)?;
        if meta.get("importedSourceMessage").is_some()
            || meta.get("importedHistoryBlocks").is_some()
        {
            let hash = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&json!([role, content, timestamp]))
                        .map_err(|e| PortalError::Other(e.to_string()))?
                )
            );
            if meta["historyOperationalHash"].as_str() != Some(&hash) {
                loss(&mut losses,&id,LossCode::ContentSkipped,"Imported message was edited or its stored representation differs; current stored content is authoritative and historical provenance is retained separately");
                timeline.push(Turn {
                    id,
                    parent_id: timeline.last().map(|t: &Turn| t.id.clone()),
                    role: if role == "assistant" {
                        Role::Assistant
                    } else {
                        Role::User
                    },
                    timestamp: parse_ts(&timestamp),
                    model: None,
                    is_meta: false,
                    blocks: vec![Block::Text { text: content }],
                    usage: None,
                    raw: Some(meta),
                });
                continue;
            }
        }
        if let Some(original) = meta.get("importedSourceMessage") {
            let turn: Turn = serde_json::from_value(original.clone()).map_err(|e| {
                PortalError::Other(format!("Invalid historical source message: {e}"))
            })?;
            if let Some(u) = turn.usage {
                usage.input_tokens += u.input_tokens;
                usage.output_tokens += u.output_tokens;
                usage.known = true;
            }
            timeline.push(turn);
            continue;
        }
        let role = match role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "system" => Role::System,
            "tool" => Role::Tool,
            other => {
                return Err(PortalError::Other(format!(
                    "unknown BentoKit message role: {other}"
                )))
            }
        };
        let token_usage = meta
            .get("tokenUsage")
            .filter(|u| u["input"].is_u64() || u["output"].is_u64())
            .map(|u| TokenUsage {
                input_tokens: u["input"].as_u64().unwrap_or(0),
                output_tokens: u["output"].as_u64().unwrap_or(0),
                ..Default::default()
            });
        if let Some(u) = token_usage {
            usage.input_tokens += u.input_tokens;
            usage.output_tokens += u.output_tokens;
            usage.known = true;
        }
        let blocks = message_blocks(&id, &content, &meta, &mut losses);
        if role == Role::Tool {
            loss(
                &mut losses,
                &id,
                LossCode::ToolPairingIncomplete,
                "Standalone tool message has no provider call id; preserved as text",
            );
        }
        timeline.push(Turn {
            parent_id: timeline.last().map(|t: &Turn| t.id.clone()),
            id,
            role,
            timestamp: parse_ts(&timestamp),
            model: meta["modelInfo"]["modelName"].as_str().map(str::to_string),
            is_meta: role == Role::System,
            blocks,
            usage: token_usage,
            raw: Some(json!({"content": content, "metadata": meta})),
        });
    }
    let conversation_meta = parse_metadata(metadata.as_deref(), &path)?;
    if let Some(metadata) = metadata {
        // Parse even if unused: malformed source metadata must not disappear
        // behind an apparently successful migration.
        parse_metadata(Some(&metadata), &path)?;
    }
    let source_session = &conversation_meta["historyImport"]["sourceSession"];
    if let Some(archived) = source_session["archivedTurns"].as_array() {
        for entry in archived {
            let turn: Turn = serde_json::from_value(entry["turn"].clone())
                .map_err(|e| PortalError::Other(e.to_string()))?;
            let index = entry["index"]
                .as_u64()
                .ok_or_else(|| PortalError::Other("Invalid archived turn order".into()))?
                as usize;
            timeline.insert(index.min(timeline.len()), turn);
        }
    }
    if let Some(source_losses) = source_session.get("losses") {
        losses.extend(
            serde_json::from_value::<Vec<LossNote>>(source_losses.clone())
                .map_err(|e| PortalError::Other(e.to_string()))?,
        );
    }
    let attachments = source_session
        .get("attachments")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|e| PortalError::Other(format!("Invalid historical attachment references: {e}")))?
        .unwrap_or_default();
    if let Some(source_usage) = source_session.get("usage") {
        usage = serde_json::from_value(source_usage.clone())
            .map_err(|e| PortalError::Other(e.to_string()))?;
    }
    if !usage.known {
        losses.push(LossNote {
            code: LossCode::UsageUnavailable,
            detail: "No BentoKit token usage records".into(),
            turn_id: None,
        });
    }
    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: locator.native_id.clone(),
            agent_id: ID.into(),
            store_path: path.display().to_string(),
            agent_version: inst.version.clone(),
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&cwd),
            cwd,
            git_branch: None,
            project_label: project.label,
        },
        title,
        timeline,
        attachments,
        usage,
        losses,
        fidelity: Fidelity::Partial,
    })
}

fn parse_metadata(raw: Option<&str>, path: &Path) -> Result<Value> {
    raw.map(serde_json::from_str)
        .transpose()
        .map(|v| v.unwrap_or(Value::Null))
        .map_err(|e| PortalError::Parse {
            path: path.display().to_string(),
            detail: format!("BentoKit metadata: {e}"),
        })
}

fn loss(losses: &mut Vec<LossNote>, id: &str, code: LossCode, detail: &str) {
    losses.push(LossNote {
        code,
        detail: detail.into(),
        turn_id: Some(id.into()),
    });
}

fn tool_blocks(
    id: &str,
    index: usize,
    tool: &Value,
    blocks: &mut Vec<Block>,
    losses: &mut Vec<LossNote>,
) {
    let Some(name) = tool["name"].as_str() else {
        blocks.push(Block::Meta {
            source_kind: "bentokit/unknown-tool".into(),
            raw: tool.clone(),
        });
        loss(
            losses,
            id,
            LossCode::UnknownRecord,
            "Tool trace has no name; preserved as metadata",
        );
        return;
    };
    let call_id = format!("{id}:tool:{index}");
    blocks.push(Block::ToolCall {
        call_id: call_id.clone(),
        name: name.into(),
        arguments: tool.get("args").cloned().unwrap_or_else(|| json!({})),
    });
    if tool.get("output").is_some()
        || tool.get("error").is_some()
        || matches!(tool["result"].as_str(), Some("success" | "error"))
    {
        blocks.push(Block::ToolResult {
            call_id,
            output: tool
                .get("output")
                .or_else(|| tool.get("error"))
                .cloned()
                .unwrap_or(Value::Null),
            is_error: tool["result"] == "error"
                || tool["error"].as_str().is_some_and(|s| !s.is_empty()),
        });
    }
}

fn message_blocks(id: &str, content: &str, meta: &Value, losses: &mut Vec<LossNote>) -> Vec<Block> {
    if let Some(history) = meta.get("importedHistoryBlocks") {
        return match serde_json::from_value::<Vec<Block>>(history.clone()) {
            Ok(blocks) => blocks,
            Err(_) => {
                loss(
                    losses,
                    id,
                    LossCode::UnknownRecord,
                    "Invalid imported history blocks; using stored context text",
                );
                vec![Block::Text {
                    text: content.into(),
                }]
            }
        };
    }
    let mut blocks = Vec::new();
    if let Some(text) = meta["reasoning"].as_str().filter(|s| !s.is_empty()) {
        blocks.push(Block::Thinking {
            text: text.into(),
            encrypted: false,
        });
    }
    if meta["subtype"] == "compaction" {
        blocks.push(Block::Compaction {
            summary: content.into(),
        });
        return blocks;
    }
    let tools = meta["toolCalls"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let segments = meta["segments"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let texts: Vec<&str> = segments
        .iter()
        .filter(|s| s["kind"] == "text")
        .filter_map(|s| s["text"].as_str())
        .collect();
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    // An edited message can retain old segments. In that case the stored
    // content wins rather than exporting stale text or duplicating the answer.
    let ordered = !segments.is_empty()
        && (content.trim().is_empty()
            || texts
                .last()
                .is_some_and(|text| text.trim() == content.trim())
            || squash(&texts.join(" ")) == squash(content));
    let mut seen = HashSet::new();
    if ordered {
        for segment in segments {
            match segment["kind"].as_str() {
                Some("text") if segment["text"].is_string() => {
                    blocks.push(Block::Text {
                        text: segment["text"].as_str().unwrap().into(),
                    });
                }
                Some("tool")
                    if segment["index"]
                        .as_u64()
                        .is_some_and(|i| i < tools.len() as u64) =>
                {
                    let index = segment["index"].as_u64().unwrap() as usize;
                    if seen.insert(index) {
                        tool_blocks(id, index, &tools[index], &mut blocks, losses);
                    }
                }
                _ => {
                    blocks.push(Block::Meta {
                        source_kind: "bentokit/unknown-segment".into(),
                        raw: segment.clone(),
                    });
                    loss(
                        losses,
                        id,
                        LossCode::UnknownRecord,
                        "Unknown or invalid ordered turn segment",
                    );
                }
            }
        }
    }
    for (index, tool) in tools.iter().enumerate() {
        if !seen.contains(&index) {
            tool_blocks(id, index, tool, &mut blocks, losses);
            loss(
                losses,
                id,
                LossCode::ContentSkipped,
                "Tool/text emission order unavailable; unplaced tools precede the stored reply",
            );
        }
    }
    if !ordered && !content.is_empty() {
        blocks.push(Block::Text {
            text: content.into(),
        });
    }
    for key in ["images", "files", "attachments", "council"] {
        if let Some(value) = meta.get(key).filter(|v| !v.is_null()) {
            blocks.push(Block::Meta {
                source_kind: format!("bentokit/{key}"),
                raw: value.clone(),
            });
            loss(
                losses,
                id,
                LossCode::ContentSkipped,
                &format!(
                    "BentoKit {key} retained as metadata; not converted to native target content"
                ),
            );
        }
    }
    blocks
}

pub(super) fn destination_cwd(
    inst: &Installation,
    destination: &portal_core::migration::types::MigrationDestination,
) -> Result<String> {
    let root = Path::new(&inst.store_root);
    let project = project_ref(
        root,
        &destination.profile,
        destination.project_id.as_deref(),
        &projects(root)?,
    );
    // A fresh profile may not have made its default workspace yet; the data
    // directory remains a valid terminal launch directory in that case.
    Ok(project
        .cwd
        .filter(|cwd| Path::new(cwd).is_dir())
        .unwrap_or_else(|| inst.store_root.clone()))
}
