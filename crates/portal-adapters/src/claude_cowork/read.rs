use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use portal_core::adapter::SessionLocator;
use portal_core::dto::{Installation, ProjectRef, SessionSummary};
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, TokenUsage, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::cache;
use super::ID;

#[derive(Debug, Clone)]
struct SessionRecord {
    summary: SessionSummary,
    remote: bool,
}

pub fn snapshot(inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
    let records = records(inst)?;
    let mut projects: BTreeMap<String, (ProjectRef, Vec<SessionSummary>)> = BTreeMap::new();
    for record in records {
        let cwd = record.summary.cwd.clone();
        let key = record.summary.project_key.clone();
        let label = cwd.as_deref().map(label_from_cwd).unwrap_or_else(|| {
            if record.remote {
                "Remote Cowork".into()
            } else {
                "Local Cowork".into()
            }
        });
        projects
            .entry(key.clone())
            .or_insert_with(|| (ProjectRef { key, cwd, label }, Vec::new()))
            .1
            .push(record.summary);
    }
    let mut out = projects.into_values().collect::<Vec<_>>();
    for (_, sessions) in &mut out {
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    }
    out.sort_by_key(|(_, sessions)| {
        std::cmp::Reverse(sessions.first().and_then(|session| session.updated_at))
    });
    Ok(out)
}

pub fn read_session(inst: &Installation, locator: &SessionLocator) -> Result<CanonicalSession> {
    let root = PathBuf::from(&inst.store_root);
    if locator.native_id.starts_with("cse_") {
        read_remote(inst, &root, locator)
    } else {
        read_local(inst, &root, locator)
    }
}

fn records(inst: &Installation) -> Result<Vec<SessionRecord>> {
    let root = PathBuf::from(&inst.store_root);
    let entries = cache::relevant_entries(&root);
    let grants = remote_folder_grants(&root);
    let mut by_id: HashMap<String, Value> = HashMap::new();
    let mut cache_bytes: HashMap<String, u64> = HashMap::new();
    let mut cache_paths: HashMap<String, String> = HashMap::new();

    for entry in &entries {
        for id in session_ids_in_key(&entry.key) {
            *cache_bytes.entry(id.clone()).or_default() += entry.body.len() as u64;
            cache_paths
                .entry(id)
                .or_insert_with(|| entry.path.display().to_string());
        }
        if entry.key.contains("/sessions/watch") {
            for value in cache::sse_json(&entry.body) {
                if is_remote_session(&value) {
                    merge_remote_metadata(&mut by_id, value);
                }
            }
        } else if is_direct_session_key(&entry.key) {
            if let Ok(value) = serde_json::from_slice::<Value>(&entry.body) {
                let value = value.get("response_shape").unwrap_or(&value).clone();
                if is_remote_session(&value) {
                    merge_remote_metadata(&mut by_id, value);
                }
            }
        }
    }

    for id in grants.keys() {
        by_id.entry(id.clone()).or_insert_with(|| {
            serde_json::json!({
                "id": id,
                "title": "Remote Cowork session",
                "tags": ["cowork-remote"]
            })
        });
    }

    let mut out = Vec::new();
    for (id, value) in by_id {
        let folders = folders_for(&value, grants.get(&id));
        let cwd = folders.first().cloned();
        let project_key = cwd.clone().unwrap_or_else(|| "cowork-remote".into());
        let created_at = parse_time(&value["created_at"]);
        let updated_at = parse_time(&value["last_event_at"])
            .or_else(|| parse_time(&value["updated_at"]))
            .or(created_at);
        // The remote API reports "0" while a newly-started session already
        // contains user and assistant events. Hide that transient value rather
        // than presenting a confidently wrong count on the board.
        let message_count = parse_u32(&value["user_message_count"]).filter(|count| *count > 0);
        out.push(SessionRecord {
            remote: true,
            summary: SessionSummary {
                agent_id: ID.into(),
                native_id: id.clone(),
                project_key,
                title: value["title"].as_str().map(str::to_string),
                cwd,
                git_branch: None,
                model: value
                    .pointer("/external_metadata/last_served_model")
                    .or_else(|| value.pointer("/config/model"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                created_at,
                updated_at,
                message_count,
                message_count_exact: false,
                size_bytes: cache_bytes.get(&id).copied().unwrap_or(0),
                store_path: cache_paths
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| root.display().to_string()),
                maybe_live: matches!(
                    value["status_bucket"].as_str(),
                    Some("working" | "waiting_for_user")
                ),
            },
        });
    }

    out.extend(local_records(&root)?);
    Ok(out)
}

fn read_remote(
    inst: &Installation,
    root: &Path,
    locator: &SessionLocator,
) -> Result<CanonicalSession> {
    let entries = cache::relevant_entries(root);
    let mut sequenced: BTreeMap<u64, Value> = BTreeMap::new();
    let needle = format!("/sessions/{}", locator.native_id);
    let mut store_path = None;

    for entry in &entries {
        if !entry.key.contains(&needle) {
            continue;
        }
        if entry.key.contains("/events?") {
            if let Ok(value) = serde_json::from_slice::<Value>(&entry.body) {
                for event in value["data"].as_array().into_iter().flatten() {
                    insert_event(&mut sequenced, event.clone());
                }
            }
            store_path.get_or_insert_with(|| entry.path.display().to_string());
        } else if entry.key.contains("/events/stream") {
            for event in cache::sse_json(&entry.body) {
                insert_event(&mut sequenced, event);
            }
            store_path = Some(entry.path.display().to_string());
        }
    }
    if sequenced.is_empty() {
        return Err(PortalError::Other(format!(
            "Cowork history for {} is not present in Claude Desktop's cache",
            locator.native_id
        )));
    }

    let record = records(inst)?
        .into_iter()
        .find(|record| record.summary.native_id == locator.native_id);
    let summary = record.as_ref().map(|record| &record.summary);
    let cwd = summary
        .and_then(|value| value.cwd.clone())
        .unwrap_or_default();
    let title = summary.and_then(|value| value.title.clone());
    let mut losses = Vec::new();

    let sequence_numbers = sequenced.keys().copied().collect::<Vec<_>>();
    let missing = sequence_numbers
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0] + 1))
        .sum::<u64>();
    if missing > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!(
                "{missing} Cowork event(s) are absent from the evictable desktop cache"
            ),
            turn_id: None,
        });
    }

    let mut timeline = Vec::new();
    for event in sequenced.into_values() {
        append_remote_event(&mut timeline, &mut losses, event);
    }
    let usage = usage_totals(&timeline);
    if !usage.known {
        losses.push(LossNote {
            code: LossCode::UsageUnavailable,
            detail: "Cowork cache did not contain usable token counts".into(),
            turn_id: None,
        });
    }

    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: locator.native_id.clone(),
            agent_id: ID.into(),
            store_path: store_path.unwrap_or_else(|| root.display().to_string()),
            agent_version: inst.version.clone(),
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&cwd),
            project_label: if cwd.is_empty() {
                "Remote Cowork".into()
            } else {
                label_from_cwd(&cwd)
            },
            cwd,
            git_branch: None,
        },
        title,
        timeline,
        attachments: Vec::new(),
        usage,
        losses,
        fidelity: Fidelity::Partial,
    })
}

fn read_local(
    inst: &Installation,
    root: &Path,
    locator: &SessionLocator,
) -> Result<CanonicalSession> {
    let metadata = find_local_metadata(root, Some(&locator.native_id))
        .into_iter()
        .next()
        .ok_or_else(|| {
            PortalError::Other(format!("Cowork session {} not found", locator.native_id))
        })?;
    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&metadata)?).map_err(|error| {
            PortalError::Parse {
                path: metadata.display().to_string(),
                detail: error.to_string(),
            }
        })?;
    let audit = metadata.with_extension("").join("audit.jsonl");
    let file = std::fs::File::open(&audit)?;
    let mut timeline = Vec::new();
    let mut losses = Vec::new();
    for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line?;
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            losses.push(LossNote {
                code: LossCode::UnknownRecord,
                detail: format!("unparseable Cowork audit line {}", index + 1),
                turn_id: None,
            });
            continue;
        };
        append_local_record(&mut timeline, &mut losses, record);
    }
    let usage = usage_totals(&timeline);
    let cwd = local_workspace(&value).unwrap_or_default();
    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: locator.native_id.clone(),
            agent_id: ID.into(),
            store_path: audit.display().to_string(),
            agent_version: inst.version.clone(),
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&cwd),
            project_label: label_from_cwd(&cwd),
            cwd,
            git_branch: None,
        },
        title: value["title"].as_str().map(str::to_string),
        timeline,
        attachments: Vec::new(),
        usage,
        losses,
        fidelity: Fidelity::Partial,
    })
}

fn append_remote_event(timeline: &mut Vec<Turn>, losses: &mut Vec<LossNote>, event: Value) {
    match event["event_type"].as_str() {
        Some("assistant") => {
            let message = &event["payload"]["message"];
            let id = message["id"]
                .as_str()
                .or_else(|| event["event_id"].as_str())
                .unwrap_or("")
                .to_string();
            let blocks = message_blocks(&message["content"], losses, Some(&id));
            if blocks.is_empty() {
                return;
            }
            let usage = token_usage(&message["usage"]);
            if let Some(last) = timeline
                .last_mut()
                .filter(|turn| turn.role == Role::Assistant && turn.id == id)
            {
                last.blocks.extend(blocks);
                if usage.is_some() {
                    last.usage = usage;
                }
                last.raw = Some(event);
                return;
            }
            timeline.push(Turn {
                id,
                parent_id: event["payload"]["parent_tool_use_id"]
                    .as_str()
                    .map(str::to_string),
                role: Role::Assistant,
                timestamp: parse_time(&event["created_at"]),
                model: message["model"].as_str().map(str::to_string),
                is_meta: false,
                blocks,
                usage,
                raw: Some(event),
            });
        }
        Some("user") => {
            let source = event["source"].as_str().unwrap_or("").to_string();
            let id = event["event_id"].as_str().unwrap_or("").to_string();
            let content = event["payload"]["message"]["content"].clone();
            let timestamp = parse_time(&event["created_at"]);
            append_user_message(timeline, losses, source, id, content, timestamp, event);
        }
        _ => {}
    }
}

fn append_local_record(timeline: &mut Vec<Turn>, losses: &mut Vec<LossNote>, record: Value) {
    let id = record["uuid"].as_str().unwrap_or("").to_string();
    match record["type"].as_str() {
        Some("assistant") => {
            let message = &record["message"];
            let blocks = message_blocks(&message["content"], losses, Some(&id));
            if blocks.is_empty() {
                return;
            }
            timeline.push(Turn {
                id,
                parent_id: record["parent_tool_use_id"].as_str().map(str::to_string),
                role: Role::Assistant,
                timestamp: parse_time(&record["timestamp"])
                    .or_else(|| parse_time(&record["_audit_timestamp"])),
                model: message["model"].as_str().map(str::to_string),
                is_meta: false,
                blocks,
                usage: token_usage(&message["usage"]),
                raw: Some(record),
            });
        }
        Some("user") => {
            let content = record["message"]["content"].clone();
            let timestamp = parse_time(&record["timestamp"])
                .or_else(|| parse_time(&record["_audit_timestamp"]));
            append_user_message(
                timeline,
                losses,
                "client".into(),
                id,
                content,
                timestamp,
                record,
            );
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn append_user_message(
    timeline: &mut Vec<Turn>,
    losses: &mut Vec<LossNote>,
    source: String,
    id: String,
    content: Value,
    timestamp: Option<DateTime<Utc>>,
    raw: Value,
) {
    let blocks = message_blocks(&content, losses, Some(&id));
    if blocks.is_empty() {
        return;
    }
    let only_tool_results = blocks
        .iter()
        .all(|block| matches!(block, Block::ToolResult { .. }));
    let injected = content
        .as_str()
        .is_some_and(|text| text.starts_with("<system-reminder>"));
    let (role, is_meta, blocks) = if only_tool_results {
        (Role::Tool, false, blocks)
    } else if source == "worker" || injected {
        (
            Role::System,
            true,
            vec![Block::Meta {
                source_kind: "cowork_context".into(),
                raw: content,
            }],
        )
    } else {
        (Role::User, false, blocks)
    };
    timeline.push(Turn {
        id,
        parent_id: None,
        role,
        timestamp,
        model: None,
        is_meta,
        blocks,
        usage: None,
        raw: Some(raw),
    });
}

fn message_blocks(
    content: &Value,
    losses: &mut Vec<LossNote>,
    turn_id: Option<&str>,
) -> Vec<Block> {
    if let Some(text) = content.as_str() {
        return vec![Block::Text { text: text.into() }];
    }
    let Some(items) = content.as_array() else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    for item in items {
        match item["type"].as_str() {
            Some("text") => {
                let text = item["text"]
                    .as_str()
                    .or_else(|| item["content"].as_str())
                    .unwrap_or("");
                if !text.is_empty() {
                    blocks.push(Block::Text { text: text.into() });
                }
            }
            Some("thinking") => {
                let text = item["thinking"].as_str().unwrap_or("").to_string();
                let encrypted = text.is_empty()
                    && item["signature"]
                        .as_str()
                        .is_some_and(|signature| !signature.is_empty());
                if encrypted {
                    losses.push(LossNote {
                        code: LossCode::EncryptedReasoning,
                        detail: "Cowork thinking is signed but its text is unavailable".into(),
                        turn_id: turn_id.map(str::to_string),
                    });
                }
                blocks.push(Block::Thinking { text, encrypted });
            }
            Some("tool_use") => blocks.push(Block::ToolCall {
                call_id: item["id"].as_str().unwrap_or("").into(),
                name: item["name"].as_str().unwrap_or("").into(),
                arguments: item.get("input").cloned().unwrap_or(Value::Null),
            }),
            Some("tool_result") => blocks.push(Block::ToolResult {
                call_id: item["tool_use_id"].as_str().unwrap_or("").into(),
                output: item.get("content").cloned().unwrap_or(Value::Null),
                is_error: item["is_error"].as_bool().unwrap_or(false),
            }),
            Some(kind) => {
                losses.push(LossNote {
                    code: LossCode::UnknownRecord,
                    detail: format!("unknown Cowork content block '{kind}' preserved as metadata"),
                    turn_id: turn_id.map(str::to_string),
                });
                blocks.push(Block::Meta {
                    source_kind: kind.into(),
                    raw: item.clone(),
                });
            }
            None => {}
        }
    }
    blocks
}

fn token_usage(value: &Value) -> Option<TokenUsage> {
    value.as_object()?;
    Some(TokenUsage {
        input_tokens: value["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: value["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: value["cache_read_input_tokens"].as_u64().unwrap_or(0),
        cache_write_tokens: value["cache_creation_input_tokens"].as_u64().unwrap_or(0),
    })
}

fn usage_totals(timeline: &[Turn]) -> UsageTotals {
    let mut totals = UsageTotals::default();
    for usage in timeline.iter().filter_map(|turn| turn.usage) {
        totals.input_tokens += usage.input_tokens;
        totals.output_tokens += usage.output_tokens;
        totals.known = true;
    }
    totals
}

fn local_records(root: &Path) -> Result<Vec<SessionRecord>> {
    let mut out = Vec::new();
    for metadata in find_local_metadata(root, None) {
        let Ok(raw) = std::fs::read_to_string(&metadata) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let native_id = value["sessionId"]
            .as_str()
            .map(str::to_string)
            .or_else(|| metadata.file_stem()?.to_str().map(str::to_string));
        let Some(native_id) = native_id else {
            continue;
        };
        let cwd = local_workspace(&value);
        let project_key = cwd.clone().unwrap_or_else(|| "cowork-local".into());
        let audit = metadata.with_extension("").join("audit.jsonl");
        let size_bytes = std::fs::metadata(&metadata).map(|m| m.len()).unwrap_or(0)
            + std::fs::metadata(&audit).map(|m| m.len()).unwrap_or(0);
        let updated_at = parse_time(&value["lastActivityAt"]).or_else(|| modified_time(&metadata));
        out.push(SessionRecord {
            remote: false,
            summary: SessionSummary {
                agent_id: ID.into(),
                native_id,
                project_key,
                title: value["title"].as_str().map(str::to_string),
                cwd,
                git_branch: None,
                model: value["model"].as_str().map(str::to_string),
                created_at: parse_time(&value["createdAt"]),
                updated_at,
                message_count: None,
                message_count_exact: false,
                size_bytes,
                store_path: audit.display().to_string(),
                maybe_live: updated_at
                    .is_some_and(|time| Utc::now().signed_duration_since(time).num_seconds() < 120),
            },
        });
    }
    Ok(out)
}

fn find_local_metadata(root: &Path, native_id: Option<&str>) -> Vec<PathBuf> {
    let local_root = root.join("local-agent-mode-sessions");
    let mut files = Vec::new();
    collect_json_files(&local_root, 0, &mut files);
    files
        .into_iter()
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if !name.starts_with("local_") || !name.ends_with(".json") {
                return false;
            }
            if path.components().any(|part| {
                matches!(
                    part.as_os_str().to_str(),
                    Some("agent" | "skills-plugin" | "rpm")
                )
            }) {
                return false;
            }
            native_id.is_none_or(|wanted| {
                std::fs::read_to_string(path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .and_then(|value| value["sessionId"].as_str().map(str::to_string))
                    .is_some_and(|id| id == wanted)
                    || path.file_stem().and_then(|value| value.to_str()) == Some(wanted)
            })
        })
        .collect()
}

fn collect_json_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, depth + 1, out);
        } else if path.extension().and_then(|value| value.to_str()) == Some("json") {
            out.push(path);
        }
    }
}

fn remote_folder_grants(root: &Path) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let local_root = root.join("local-agent-mode-sessions");
    let mut files = Vec::new();
    collect_named_files(&local_root, 0, "remote-session-spaces.json", &mut files);
    for path in files {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        for entry in value["entries"].as_array().into_iter().flatten() {
            let Some(id) = entry["sessionId"].as_str() else {
                continue;
            };
            let id = id
                .strip_prefix("session_")
                .map(|suffix| format!("cse_{suffix}"))
                .unwrap_or_else(|| id.to_string());
            let folders = entry["folders"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            out.insert(id, folders);
        }
    }
    out
}

fn collect_named_files(dir: &Path, depth: usize, name: &str, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_named_files(&path, depth + 1, name, out);
        } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
            out.push(path);
        }
    }
}

fn folders_for(value: &Value, fallback: Option<&Vec<String>>) -> Vec<String> {
    value
        .pointer("/client_metadata/remote_cowork/userSelectedFolders")
        .and_then(Value::as_array)
        .map(|folders| {
            folders
                .iter()
                .filter_map(|folder| folder["path"].as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|folders| !folders.is_empty())
        .or_else(|| fallback.cloned())
        .unwrap_or_default()
}

fn local_workspace(value: &Value) -> Option<String> {
    value["userSelectedFolders"]
        .as_array()
        .and_then(|folders| folders.first())
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| value["cwd"].as_str().map(str::to_string))
}

fn insert_event(events: &mut BTreeMap<u64, Value>, event: Value) {
    if let Some(sequence) = parse_u64(&event["sequence_num"]) {
        events.insert(sequence, event);
    }
}

fn session_ids_in_key(key: &str) -> Vec<String> {
    key.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|part| part.starts_with("cse_") && part.len() > 4)
        .map(str::to_string)
        .collect()
}

fn is_direct_session_key(key: &str) -> bool {
    key.contains("/v1/code/sessions/cse_") && !key.contains("/events") && !key.contains("/wiggle")
}

fn is_remote_session(value: &Value) -> bool {
    value["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("cse_"))
        && value["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some("cowork-remote")))
}

fn merge_remote_metadata(sessions: &mut HashMap<String, Value>, candidate: Value) {
    let Some(id) = candidate["id"].as_str().map(str::to_string) else {
        return;
    };
    let candidate_time = parse_time(&candidate["last_event_at"])
        .or_else(|| parse_time(&candidate["updated_at"]))
        .or_else(|| parse_time(&candidate["created_at"]));
    let replace = sessions.get(&id).is_none_or(|current| {
        let current_time = parse_time(&current["last_event_at"])
            .or_else(|| parse_time(&current["updated_at"]))
            .or_else(|| parse_time(&current["created_at"]));
        candidate_time >= current_time
    });
    if replace {
        sessions.insert(id, candidate);
    }
}

fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.with_timezone(&Utc))
}

fn parse_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn parse_u32(value: &Value) -> Option<u32> {
    parse_u64(value).and_then(|number| u32::try_from(number).ok())
}

fn modified_time(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn write_cache_entry(root: &Path, name: &str, key: &str, body: &[u8], compressed: bool) {
        let dir = root.join("Cache/Cache_Data");
        std::fs::create_dir_all(&dir).unwrap();
        let mut file = std::fs::File::create(dir.join(name)).unwrap();
        let mut header = [0_u8; 24];
        header[12..16].copy_from_slice(&(key.len() as u32).to_le_bytes());
        file.write_all(&header).unwrap();
        file.write_all(key.as_bytes()).unwrap();
        if compressed {
            file.write_all(&zstd::stream::encode_all(Cursor::new(body), 1).unwrap())
                .unwrap();
        } else {
            file.write_all(body).unwrap();
        }
    }

    #[test]
    fn converts_remote_grant_ids_to_session_ids() {
        let root =
            std::env::temp_dir().join(format!("portal-cowork-grants-{}", uuid::Uuid::new_v4()));
        let dir = root.join("local-agent-mode-sessions/account/org");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("remote-session-spaces.json"),
            r#"{"entries":[{"sessionId":"session_abc","folders":["/tmp/demo"]}]}"#,
        )
        .unwrap();
        let grants = remote_folder_grants(&root);
        assert_eq!(grants["cse_abc"], vec!["/tmp/demo"]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn coalesces_assistant_blocks_and_maps_tool_results() {
        let mut timeline = Vec::new();
        let mut losses = Vec::new();
        for event in [
            serde_json::json!({"event_type":"assistant","event_id":"a1","sequence_num":"1","source":"worker","payload":{"message":{"id":"msg_1","model":"claude-test","content":[{"type":"text","text":"Working"}],"usage":{"input_tokens":2,"output_tokens":1}}}}),
            serde_json::json!({"event_type":"assistant","event_id":"a2","sequence_num":"2","source":"worker","payload":{"message":{"id":"msg_1","model":"claude-test","content":[{"type":"tool_use","id":"tool_1","name":"read","input":{"path":"x"}}],"usage":{"input_tokens":2,"output_tokens":3}}}}),
            serde_json::json!({"event_type":"user","event_id":"u1","sequence_num":"3","source":"worker","payload":{"message":{"content":[{"type":"tool_result","tool_use_id":"tool_1","content":"ok"}]}}}),
        ] {
            append_remote_event(&mut timeline, &mut losses, event);
        }
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].role, Role::Assistant);
        assert_eq!(timeline[0].blocks.len(), 2);
        assert_eq!(timeline[0].usage.unwrap().output_tokens, 3);
        assert_eq!(timeline[1].role, Role::Tool);
        assert!(losses.is_empty());
    }

    #[test]
    fn remote_cache_fixture_reads_to_valid_ir() {
        let root = std::env::temp_dir().join(format!(
            "portal-cowork-remote-fixture-{}",
            uuid::Uuid::new_v4()
        ));
        let session = serde_json::json!({
            "response_shape": {
                "id": "cse_fixture",
                "title": "Polish the app bar",
                "created_at": "2026-07-15T10:00:00Z",
                "last_event_at": "2026-07-15T10:01:00Z",
                "status": "active",
                "status_bucket": "review_ready",
                "environment_kind": "anthropic_cloud",
                "tags": ["cowork-remote"],
                "config": {"model": "claude-test"},
                "client_metadata": {"remote_cowork": {"userSelectedFolders": [
                    {"path": "/tmp/demo", "deviceName": "test"}
                ]}}
            }
        });
        write_cache_entry(
            &root,
            "session_0",
            "1/0/https://claude.ai/v1/code/sessions/cse_fixture",
            serde_json::to_string(&session).unwrap().as_bytes(),
            true,
        );
        let events = [
            serde_json::json!({"event_id":"u1","sequence_num":"1","event_type":"user","source":"client","payload":{"message":{"content":"Please polish it"}},"created_at":"2026-07-15T10:00:01Z"}),
            serde_json::json!({"event_id":"a1","sequence_num":"2","event_type":"assistant","source":"worker","payload":{"message":{"id":"msg_1","model":"claude-test","content":[{"type":"tool_use","id":"tool_1","name":"read","input":{"path":"app.scss"}}],"usage":{"input_tokens":10,"output_tokens":3}}},"created_at":"2026-07-15T10:00:02Z"}),
            serde_json::json!({"event_id":"t1","sequence_num":"3","event_type":"user","source":"worker","payload":{"message":{"content":[{"type":"tool_result","tool_use_id":"tool_1","content":"styles"}]}},"created_at":"2026-07-15T10:00:03Z"}),
            serde_json::json!({"event_id":"a2","sequence_num":"4","event_type":"assistant","source":"worker","payload":{"message":{"id":"msg_2","model":"claude-test","content":[{"type":"text","text":"Done"}],"usage":{"input_tokens":12,"output_tokens":4}}},"created_at":"2026-07-15T10:00:04Z"}),
        ];
        let sse = events
            .iter()
            .map(|event| format!("event: client_event\ndata: {}\n\n", event))
            .collect::<String>();
        write_cache_entry(
            &root,
            "events_0",
            "1/0/https://claude.ai/v1/code/sessions/cse_fixture/events/stream",
            sse.as_bytes(),
            false,
        );

        let inst = Installation {
            cli_path: None,
            version: Some("fixture".into()),
            store_root: root.display().to_string(),
        };
        let snapshot = snapshot(&inst).unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].0.cwd.as_deref(), Some("/tmp/demo"));
        assert_eq!(
            snapshot[0].1[0].title.as_deref(),
            Some("Polish the app bar")
        );

        let session = read_session(
            &inst,
            &SessionLocator {
                native_id: "cse_fixture".into(),
                store_path: None,
            },
        )
        .unwrap();
        assert!(session.validate().is_empty(), "{:?}", session.validate());
        assert_eq!(session.workspace.cwd, "/tmp/demo");
        assert_eq!(session.timeline.len(), 4);
        assert_eq!(session.timeline[0].role, Role::User);
        assert_eq!(session.timeline[2].role, Role::Tool);
        assert_eq!(session.usage.input_tokens, 22);
        assert_eq!(session.usage.output_tokens, 7);
        assert!(session.unanswered_tool_calls().is_empty());
        std::fs::remove_dir_all(root).ok();
    }
}
