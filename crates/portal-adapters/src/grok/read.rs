use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use portal_core::adapter::SessionLocator;
use portal_core::dto::{Installation, ProjectRef, SessionSummary};
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::{label_from_cwd, normalize_cwd};

use super::ID;

pub fn snapshot(inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
    let root = PathBuf::from(&inst.store_root);
    let mut projects = Vec::new();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(projects),
        Err(error) => return Err(error.into()),
    };

    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().to_string();
        let decoded_cwd = percent_decode(&key);
        let mut sessions = Vec::new();

        let Ok(session_entries) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for session_entry in session_entries.flatten() {
            let session_dir = session_entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let native_id = session_entry.file_name().to_string_lossy().to_string();
            if uuid::Uuid::parse_str(&native_id).is_err() {
                continue;
            }
            if let Some(summary) = summarize(&session_dir, &key, &decoded_cwd, &native_id) {
                sessions.push(summary);
            }
        }

        if sessions.is_empty() {
            continue;
        }
        sessions.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        let cwd = sessions
            .iter()
            .find_map(|session| session.cwd.clone())
            .or_else(|| (!decoded_cwd.is_empty()).then_some(decoded_cwd));
        let label = cwd
            .as_deref()
            .map(label_from_cwd)
            .unwrap_or_else(|| key.clone());
        projects.push((ProjectRef { key, cwd, label }, sessions));
    }
    Ok(projects)
}

fn summarize(
    session_dir: &Path,
    project_key: &str,
    decoded_cwd: &str,
    native_id: &str,
) -> Option<SessionSummary> {
    let summary_path = session_dir.join("summary.json");
    let raw = std::fs::read_to_string(&summary_path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    if value["info"]["id"].as_str()? != native_id {
        return None;
    }
    if value["chat_format_version"].as_u64() != Some(1) {
        return None;
    }

    let cwd = summary_cwd(&value).unwrap_or(decoded_cwd).to_string();
    let title = summary_title(&value).map(str::to_string);
    let created_at = parse_timestamp(&value["created_at"]);
    let updated_at = parse_timestamp(&value["last_active_at"])
        .or_else(|| parse_timestamp(&value["updated_at"]))
        .or_else(|| {
            std::fs::metadata(&summary_path)
                .ok()?
                .modified()
                .ok()
                .map(DateTime::<Utc>::from)
        });
    let size_bytes = directory_size(session_dir);
    let maybe_live = std::fs::metadata(&summary_path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed.as_secs() < 120);

    Some(SessionSummary {
        agent_id: ID.into(),
        native_id: native_id.into(),
        project_key: project_key.into(),
        title,
        cwd: (!cwd.is_empty()).then_some(cwd),
        git_branch: value["head_branch"].as_str().map(str::to_string),
        model: value["current_model_id"].as_str().map(str::to_string),
        created_at,
        updated_at,
        message_count: value["num_chat_messages"]
            .as_u64()
            .and_then(|count| u32::try_from(count).ok()),
        message_count_exact: value["num_chat_messages"].as_u64().is_some(),
        size_bytes,
        store_path: session_dir.display().to_string(),
        maybe_live,
    })
}

pub fn read_session(inst: &Installation, locator: &SessionLocator) -> Result<CanonicalSession> {
    let root = PathBuf::from(&inst.store_root);
    let session_dir = resolve_session_dir(&root, locator)?;
    let summary_path = session_dir.join("summary.json");
    let chat_path = session_dir.join("chat_history.jsonl");
    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(&summary_path)?).map_err(|error| {
            PortalError::Parse {
                path: summary_path.display().to_string(),
                detail: error.to_string(),
            }
        })?;
    if summary["info"]["id"].as_str() != Some(&locator.native_id) {
        return Err(PortalError::Other(format!(
            "Grok summary id does not match session {}",
            locator.native_id
        )));
    }
    if summary["chat_format_version"].as_u64() != Some(1) {
        return Err(PortalError::Other(format!(
            "unsupported Grok chat format in {} (expected version 1)",
            summary_path.display()
        )));
    }

    let file = std::fs::File::open(&chat_path)?;
    let reader = std::io::BufReader::new(file);
    let mut timeline = Vec::new();
    let mut losses = Vec::new();
    let mut encrypted_reasoning = 0usize;
    let mut unknown_records = 0usize;
    let mut invalid_lines = 0usize;

    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            invalid_lines += 1;
            continue;
        };
        let turn_id = record["id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("L{}", index + 1));
        let model = record["model_id"].as_str().map(str::to_string);
        let (role, blocks, is_meta) = match record["type"].as_str() {
            Some("system") => (Role::System, content_blocks(&record["content"]), true),
            Some("user") => (Role::User, content_blocks(&record["content"]), false),
            Some("assistant") => {
                let mut blocks = content_blocks(&record["content"]);
                if let Some(calls) = record["tool_calls"].as_array() {
                    for call in calls {
                        let raw_arguments = call["arguments"].as_str().unwrap_or("");
                        let arguments = serde_json::from_str(raw_arguments)
                            .unwrap_or_else(|_| Value::String(raw_arguments.into()));
                        blocks.push(Block::ToolCall {
                            call_id: call["id"].as_str().unwrap_or("").into(),
                            name: call["name"].as_str().unwrap_or("").into(),
                            arguments,
                        });
                    }
                }
                (Role::Assistant, blocks, false)
            }
            Some("reasoning") => {
                let text = record["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let encrypted = record["encrypted_content"]
                    .as_str()
                    .is_some_and(|content| !content.is_empty());
                if encrypted {
                    encrypted_reasoning += 1;
                }
                (
                    Role::Assistant,
                    vec![Block::Thinking { text, encrypted }],
                    false,
                )
            }
            Some("tool_result") => (
                Role::Tool,
                vec![Block::ToolResult {
                    call_id: record["tool_call_id"].as_str().unwrap_or("").into(),
                    output: record["content"].clone(),
                    is_error: false,
                }],
                false,
            ),
            other => {
                unknown_records += 1;
                (
                    Role::System,
                    vec![Block::Meta {
                        source_kind: other.unwrap_or("unknown").into(),
                        raw: record.clone(),
                    }],
                    true,
                )
            }
        };
        if blocks.is_empty() {
            continue;
        }
        timeline.push(Turn {
            id: turn_id,
            parent_id: None,
            role,
            timestamp: None,
            model: (role == Role::Assistant).then_some(model).flatten(),
            is_meta,
            blocks,
            usage: None,
            raw: Some(record),
        });
    }

    if invalid_lines > 0 || unknown_records > 0 {
        losses.push(LossNote {
            code: LossCode::UnknownRecord,
            detail: format!(
                "{unknown_records} unsupported Grok record(s) preserved as metadata; {invalid_lines} invalid line(s) skipped"
            ),
            turn_id: None,
        });
    }
    if encrypted_reasoning > 0 {
        losses.push(LossNote {
            code: LossCode::EncryptedReasoning,
            detail: format!(
                "{encrypted_reasoning} Grok reasoning item(s) contain provider-encrypted content"
            ),
            turn_id: None,
        });
    }
    losses.push(LossNote {
        code: LossCode::UsageUnavailable,
        detail: "Grok chat history does not expose stable per-turn token usage".into(),
        turn_id: None,
    });

    let cwd = summary_cwd(&summary).unwrap_or_default().to_string();
    let title = summary_title(&summary).map(str::to_string);

    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: locator.native_id.clone(),
            agent_id: ID.into(),
            store_path: session_dir.display().to_string(),
            agent_version: inst.version.clone(),
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd_normalized: normalize_cwd(&cwd),
            project_label: label_from_cwd(&cwd),
            cwd,
            git_branch: summary["head_branch"].as_str().map(str::to_string),
        },
        title,
        timeline,
        attachments: Vec::new(),
        usage: UsageTotals::default(),
        losses,
        fidelity: Fidelity::Partial,
    })
}

fn content_blocks(content: &Value) -> Vec<Block> {
    if let Some(text) = content.as_str() {
        return (!text.is_empty())
            .then(|| Block::Text { text: text.into() })
            .into_iter()
            .collect();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| match item["type"].as_str() {
            Some("text") | Some("input_text") | Some("output_text") => Block::Text {
                text: item["text"].as_str().unwrap_or_default().into(),
            },
            other => Block::Meta {
                source_kind: other.unwrap_or("unknown-content").into(),
                raw: item.clone(),
            },
        })
        .collect()
}

fn resolve_session_dir(root: &Path, locator: &SessionLocator) -> Result<PathBuf> {
    if let Some(hint) = &locator.store_path {
        if hint.starts_with(root)
            && hint.is_dir()
            && hint.file_name().and_then(|name| name.to_str()) == Some(&locator.native_id)
        {
            return Ok(hint.clone());
        }
    }
    let entries = std::fs::read_dir(root)?;
    for project in entries.flatten().filter(|entry| entry.path().is_dir()) {
        let candidate = project.path().join(&locator.native_id);
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err(PortalError::Other(format!(
        "Grok session {} not found under {}",
        locator.native_id,
        root.display()
    )))
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn summary_cwd(summary: &Value) -> Option<&str> {
    summary["info"]["cwd"]
        .as_str()
        .or_else(|| summary["git_root_dir"].as_str())
        .filter(|cwd| !cwd.is_empty())
}

fn summary_title(summary: &Value) -> Option<&str> {
    summary["generated_title"]
        .as_str()
        .or_else(|| summary["session_summary"].as_str())
        .filter(|title| !title.trim().is_empty())
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                directory_size(&path)
            } else {
                entry.metadata().map(|meta| meta.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                output.push(byte);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn decodes_windows_workspace_key() {
        assert_eq!(percent_decode("P%3A%5Cdemo%5Capp"), r"P:\demo\app");
        assert_eq!(
            percent_decode("P%3A%5Cclientes%5CS%C3%A3o-Paulo"),
            r"P:\clientes\São-Paulo"
        );
    }
}
