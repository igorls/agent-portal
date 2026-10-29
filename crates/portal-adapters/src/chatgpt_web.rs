//! Bounded, manually selected single-conversation JSON exports. No account API.
use chrono::{TimeZone, Utc};
use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    Attachment, Block, CanonicalSession, Fidelity, LossCode, LossNote, Role, SessionIdentity, Turn,
    UsageTotals, Workspace, IR_VERSION,
};
use portal_core::util::paths::atomic_write;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const ID: &str = "chatgpt-web";
pub struct ChatGptWebAdapter;
pub fn export_choices(raw: &str) -> Result<Vec<portal_core::dto::ChatGptExportChoice>> {
    if raw.len() > 128 * 1024 * 1024 {
        return Err(error(
            "Export JSON exceeds 128 MiB; use a smaller numbered export file",
        ));
    }
    let data: Value = serde_json::from_str(raw).map_err(|e| error(e.to_string()))?;
    let values: Vec<&Value> = data
        .as_array()
        .map(|v| v.iter().collect())
        .unwrap_or_else(|| vec![&data]);
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|v| {
            let id = v["id"]
                .as_str()
                .or_else(|| v["conversation_id"].as_str())
                .ok_or_else(|| error("Export contains a conversation without an id"))?;
            if !seen.insert(id) {
                return Err(error(
                    "Export contains duplicate conversation ids; select one numbered file",
                ));
            }
            Ok(portal_core::dto::ChatGptExportChoice {
                id: id.into(),
                title: v["title"]
                    .as_str()
                    .unwrap_or("Untitled conversation")
                    .into(),
            })
        })
        .collect()
}

pub fn stage_selected_export(root: &Path, raw: &str, id: Option<&str>) -> Result<SessionSummary> {
    let choices = export_choices(raw)?;
    if choices.len() == 1 && id.is_none() {
        return stage_export(root, raw);
    }
    let id = id.ok_or_else(|| error("Choose the conversation to import from this export"))?;
    let data: Value = serde_json::from_str(raw).map_err(|e| error(e.to_string()))?;
    let selected = if let Some(values) = data.as_array() {
        values
            .iter()
            .find(|v| v["id"] == id || v["conversation_id"] == id)
    } else {
        Some(&data).filter(|v| v["id"] == id || v["conversation_id"] == id)
    }
    .ok_or_else(|| error("Selected conversation is not in the export"))?;
    stage_export(
        root,
        &serde_json::to_string(selected).map_err(|e| error(e.to_string()))?,
    )
}
fn error(message: impl Into<String>) -> PortalError {
    PortalError::Other(message.into())
}
pub fn default_root(env: &HostEnv) -> PathBuf {
    env.store_root(ID, env.home.join(".agent-portal/imports/chatgpt"))
}
fn project() -> ProjectRef {
    ProjectRef {
        key: ID.into(),
        cwd: None,
        label: "ChatGPT web imports".into(),
    }
}
fn ts(v: &Value) -> Option<chrono::DateTime<Utc>> {
    v.as_f64()
        .filter(|n| n.is_finite())
        .and_then(|n| Utc.timestamp_millis_opt((n * 1000.0) as i64).single())
}

impl AgentAdapter for ChatGptWebAdapter {
    fn id(&self) -> &'static str {
        ID
    }
    fn display_name(&self) -> &'static str {
        "ChatGPT Web (selected exports)"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities{store_kind:StoreKind::DirPerSession,read:SupportLevel::Partial,write_native:SupportLevel::None,watch:true,launch_resume:false,launch_new:false,context_tokens:None,write_confidence:None,version_range_tested:"Single-conversation mapping/current_node schema; synthetic fixtures".into(),notes:vec!["Add one manually selected conversation JSON using Import ChatGPT export".into(),"Visible selected branch becomes transcript history; graph/events/media references are historical metadata".into()]}
    }
    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let root = default_root(env);
        root.is_dir().then(|| Installation {
            cli_path: None,
            version: None,
            store_root: root.display().to_string(),
        })
    }
    fn list_projects(&self, _: &Installation) -> Result<Vec<ProjectRef>> {
        Ok(vec![project()])
    }
    fn list_sessions(&self, inst: &Installation, _: &ProjectRef) -> Result<Vec<SessionSummary>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&inst.store_root)? {
            let path = entry?.path().join("summary.json");
            if !path.is_file() {
                continue;
            }
            bounded_path(Path::new(&inst.store_root), &path)?;
            let bytes = std::fs::read(&path)?;
            if bytes.len() > 64 * 1024 {
                return Err(error("ChatGPT summary exceeds its metadata budget"));
            }
            out.push(
                serde_json::from_slice(&bytes)
                    .map_err(|e| error(format!("ChatGPT summary: {e}")))?,
            );
        }
        Ok(out)
    }
    fn read_session(
        &self,
        inst: &Installation,
        locator: &SessionLocator,
    ) -> Result<CanonicalSession> {
        let digest = locator.native_id.rsplit('/').next().unwrap_or_default();
        if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(error("Invalid ChatGPT export locator"));
        }
        let path = Path::new(&inst.store_root)
            .join(digest)
            .join("conversation.json");
        bounded_path(Path::new(&inst.store_root), &path)?;
        let raw = std::fs::read_to_string(&path)?;
        if format!("{:x}", Sha256::digest(raw.as_bytes())) != digest {
            return Err(error(
                "Staged ChatGPT export changed; select the revised export again",
            ));
        }
        let session = parse(&raw, &path, digest)?;
        if session.identity.native_id != locator.native_id {
            return Err(error(
                "ChatGPT conversation id does not match the staged export",
            ));
        }
        Ok(session)
    }
}

fn bounded_path(root: &Path, path: &Path) -> Result<()> {
    if !path.canonicalize()?.starts_with(root.canonicalize()?) {
        return Err(error("ChatGPT export path escapes its store"));
    }
    Ok(())
}

/// Only explicitly selected JSON enters the import store. A summary sidecar
/// keeps normal board scans independent of transcript size.
pub fn stage_export(root: &Path, raw: &str) -> Result<SessionSummary> {
    let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let dir = root.join(&digest);
    let path = dir.join("conversation.json");
    let session = parse(raw, &path, &digest)?;
    std::fs::create_dir_all(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::create_dir_all(&dir)?;
    bounded_path(root, &dir)?;
    if path.exists() {
        bounded_path(root, &path)?;
        if std::fs::read_to_string(&path)? != raw {
            return Err(error("Staged export collision"));
        }
    } else {
        atomic_write(&path, raw.as_bytes())?;
    }
    let visible: Vec<_> = session.timeline.iter().filter(|t| !t.is_meta).collect();
    let summary = SessionSummary {
        agent_id: ID.into(),
        native_id: session.identity.native_id,
        project_key: ID.into(),
        title: session.title,
        cwd: None,
        git_branch: None,
        model: visible.iter().rev().find_map(|t| t.model.clone()),
        created_at: visible.first().and_then(|t| t.timestamp),
        updated_at: visible.last().and_then(|t| t.timestamp),
        message_count: Some(visible.len() as u32),
        message_count_exact: true,
        size_bytes: raw.len() as u64,
        store_path: path.display().to_string(),
        maybe_live: false,
    };
    let summary_path = dir.join("summary.json");
    if summary_path.exists() {
        bounded_path(root, &summary_path)?;
    }
    atomic_write(
        &summary_path,
        &serde_json::to_vec(&summary).map_err(|e| error(e.to_string()))?,
    )?;
    Ok(summary)
}

fn parse(raw: &str, path: &Path, digest: &str) -> Result<CanonicalSession> {
    if raw.len() > 32 * 1024 * 1024 {
        return Err(error(
            "Select a single ChatGPT conversation smaller than 32 MiB",
        ));
    }
    let mut data: Value =
        serde_json::from_str(raw).map_err(|e| error(format!("ChatGPT export JSON: {e}")))?;
    if data["capture_kind"] == "chatgpt-visible-dom-v1" {
        let messages = data["messages"]
            .as_array()
            .ok_or_else(|| error("Visible-page capture has no messages"))?;
        let mut mapping = serde_json::Map::new();
        let mut parent: Option<String> = None;
        for message in messages {
            let id = message["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error("Visible-page capture is missing a source message id"))?;
            if mapping.contains_key(id) {
                return Err(error("Visible-page capture repeats a message id"));
            }
            let role = message["role"]
                .as_str()
                .filter(|r| matches!(*r, "user" | "assistant"))
                .ok_or_else(|| error("Visible-page capture has an unsupported role"))?;
            let text = message["text"]
                .as_str()
                .ok_or_else(|| error("Visible-page capture is missing message text"))?;
            mapping.insert(id.into(),json!({"id":id,"parent":parent,"message":{"id":id,"author":{"role":role},"create_time":null,"content":{"content_type":"text","parts":[text]},"metadata":{"visible_dom_capture":message}}}));
            parent = Some(id.into());
        }
        data = json!({"id":data["id"],"title":data["title"],"current_node":parent,"mapping":mapping,"source_capture":data});
    }
    if let Some(items) = data.as_array_mut() {
        if items.len() != 1 {
            return Err(error("Select exactly one conversation; multi-conversation account archives are outside this import route"));
        }
        data = items.pop().unwrap();
    }
    let id = data["id"]
        .as_str()
        .or_else(|| data["conversation_id"].as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| error("ChatGPT export has no conversation id"))?;
    let mapping = data["mapping"]
        .as_object()
        .ok_or_else(|| error("Expected a ChatGPT mapping/current_node conversation export"))?;
    let leaf = data["current_node"]
        .as_str()
        .ok_or_else(|| error("Export must identify the selected branch with current_node"))?;
    if mapping.len() > 100_000 {
        return Err(error("ChatGPT conversation graph exceeds 100000 nodes"));
    }
    let mut seen = HashSet::new();
    let mut chain = Vec::new();
    let mut current = Some(leaf);
    while let Some(key) = current {
        if !seen.insert(key) {
            return Err(error("ChatGPT branch contains a cycle"));
        }
        let node = mapping
            .get(key)
            .ok_or_else(|| error("ChatGPT branch references a missing node"))?;
        chain.push((key, node));
        current = node["parent"].as_str();
    }
    chain.reverse();
    let mut timeline: Vec<Turn> = Vec::new();
    let mut losses = Vec::new();
    if data["source_capture"]["capture_kind"] == "chatgpt-visible-dom-v1" {
        losses.push(LossNote{code:LossCode::ContentSkipped,detail:"Visible DOM capture: unavailable native timestamps/model ids and unrendered branches remain unknown; parent links reflect visible display order, with observed DOM ids/HTML retained as provenance".into(),turn_id:None});
    }
    for (node_id, node) in chain {
        let message = &node["message"];
        if message.is_null() {
            continue;
        }
        // Only exported visible content is interpreted. No hidden reasoning
        // retrieval, inference, or reconstruction is performed.
        if message["metadata"]["is_visually_hidden_from_conversation"] == true
            || message["channel"] == "analysis"
        {
            continue;
        }
        let role = match message["author"]["role"].as_str() {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("system") => Role::System,
            Some("tool") => Role::Tool,
            _ => {
                losses.push(LossNote {
                    code: LossCode::UnknownRecord,
                    detail: format!("Unknown author at node {node_id}; retained in source graph"),
                    turn_id: None,
                });
                continue;
            }
        };
        let mut blocks = Vec::new();
        if let Some(parts) = message["content"]["parts"].as_array() {
            for part in parts {
                if let Some(text) = part.as_str() {
                    blocks.push(Block::Text { text: text.into() });
                } else {
                    blocks.push(Block::Meta {
                        source_kind: "chatgpt-web/media-reference".into(),
                        raw: part.clone(),
                    });
                    losses.push(LossNote {
                        code: LossCode::ContentSkipped,
                        detail: "Media/file references retained; attachment bytes are not copied"
                            .into(),
                        turn_id: message["id"].as_str().map(str::to_string),
                    });
                }
            }
        } else if let Some(text) = message["content"]["text"].as_str() {
            blocks.push(Block::Text { text: text.into() });
        } else {
            blocks.push(Block::Meta {
                source_kind: "chatgpt-web/content".into(),
                raw: message["content"].clone(),
            });
            losses.push(LossNote {
                code: LossCode::UnknownRecord,
                detail: format!("Unconverted content/event at {node_id}; source frame retained"),
                turn_id: None,
            });
        }
        if role == Role::Tool {
            losses.push(LossNote{code:LossCode::ToolPairingIncomplete,detail:"Exported tool records are historical text/events; native tool call associations remain in their source frames".into(),turn_id:message["id"].as_str().map(str::to_string)});
        }
        let turn = Turn {
            id: message["id"].as_str().unwrap_or(node_id).into(),
            parent_id: timeline.last().map(|t| t.id.clone()),
            role,
            timestamp: ts(&message["create_time"]),
            model: message["metadata"]["model_slug"]
                .as_str()
                .map(str::to_string),
            is_meta: role == Role::System,
            blocks,
            usage: None,
            raw: Some(json!({"nodeId":node_id,"node":node})),
        };
        timeline.push(turn);
    }
    if !timeline.iter().any(|t| !t.is_meta) {
        return Err(error(
            "Selected ChatGPT branch contains no visible messages",
        ));
    }
    let other = mapping.len().saturating_sub(seen.len());
    if other > 0 {
        losses.push(LossNote{code:LossCode::AbandonedBranch,detail:format!("{other} nodes outside the selected branch retained in the source graph; only the selected branch is continued"),turn_id:None});
    }
    // Preserve supplied graph, branch selection, references and export events
    // as historical data. These are never installed as target system authority.
    timeline.push(Turn {
        id: format!("{id}:source-graph"),
        parent_id: None,
        role: Role::System,
        timestamp: None,
        model: None,
        is_meta: true,
        blocks: vec![Block::Meta {
            source_kind: "chatgpt-web/source-graph".into(),
            raw: data.clone(),
        }],
        usage: None,
        raw: Some(data.clone()),
    });
    losses.push(LossNote{code:LossCode::UsageUnavailable,detail:"No normalized token-usage accounting; supplied metadata remains in the original source frames".into(),turn_id:None});
    Ok(CanonicalSession {
        ir_version: IR_VERSION,
        identity: SessionIdentity {
            portal_id: uuid::Uuid::now_v7().to_string(),
            native_id: format!("{id}/{digest}"),
            agent_id: ID.into(),
            store_path: path.display().to_string(),
            agent_version: None,
            read_at: Utc::now(),
        },
        workspace: Workspace {
            cwd: String::new(),
            cwd_normalized: String::new(),
            git_branch: None,
            project_label: "ChatGPT web imports".into(),
        },
        title: data["title"].as_str().map(str::to_string),
        timeline,
        attachments: vec![Attachment::Sidechain {
            id: "chatgpt-export-source-graph".into(),
            file: path.display().to_string(),
            size_bytes: Some(raw.len() as u64),
        }],
        usage: UsageTotals::default(),
        losses,
        fidelity: Fidelity::Partial,
    })
}
