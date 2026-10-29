//! Supported Core CLI protocol, never direct SQLite/journal mutation.
use portal_core::dto::Installation;
use portal_core::error::{PortalError, Result};
use portal_core::ir::{Block, CanonicalSession, LossCode, LossNote, Role};
use portal_core::migration::types::{
    ArtifactKind, DestinationChoices, MigrationDestination, SessionArtifact, WriteOptions,
    WritePlan, WrittenArtifact, WrittenSession,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn err(message: impl Into<String>) -> PortalError {
    PortalError::Other(message.into())
}

pub(super) fn cli(inst: &Installation, operation: &str, input: Option<&Value>) -> Result<Value> {
    let executable = inst.cli_path.as_ref().ok_or_else(|| {
        err("Install a BentoKit CLI with session import-protocol v1 to receive migrations")
    })?;
    let mut command = Command::new(executable);
    command.args(["session", operation, "--data", &inst.store_root]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let body = input
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|e| err(e.to_string()))?
        .unwrap_or_default();
    let writer = std::thread::spawn(move || stdin.write_all(&body));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.take(1024 * 1024).read_to_end(&mut b).map(|_| b)
    });
    let errors = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.take(64 * 1024).read_to_end(&mut b).map(|_| b)
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = writer.join();
    let stdout = output
        .join()
        .map_err(|_| err("BentoKit stdout reader failed"))??;
    let stderr = errors
        .join()
        .map_err(|_| err("BentoKit stderr reader failed"))??;
    let status=status.ok_or_else(||err("BentoKit import operation timed out; retry the same migration to recover its durable import"))?;
    if !status.success() {
        return Err(err(format!(
            "BentoKit {operation} failed: {}",
            String::from_utf8_lossy(&stderr)
        )));
    }
    serde_json::from_slice(&stdout).map_err(|_|err("BentoKit CLI does not implement the versioned session import protocol; rebuild/install Core"))
}

pub(super) fn supports(inst: &Installation) -> bool {
    cli(inst, "import-protocol", None).is_ok_and(|v| v["version"] == 1)
}

pub(super) fn choices(inst: &Installation) -> Result<DestinationChoices> {
    let value = cli(inst, "import-targets", None)?;
    serde_json::from_value(value).map_err(|e| err(format!("BentoKit import target choices: {e}")))
}

fn payload(session: &CanonicalSession, destination: &MigrationDestination) -> Result<Value> {
    if destination.profile.is_empty() {
        return Err(err("Select a BentoKit destination profile"));
    }
    let unanswered = session.unanswered_tool_calls();
    let mut messages = Vec::new();
    let mut archived_turns = Vec::new();
    for (index, turn) in session.timeline.iter().enumerate() {
        if turn.is_meta || turn.role == Role::System {
            archived_turns.push(json!({"index":index,"turn":turn}));
            continue;
        }
        let mut blocks: Vec<_> = turn
            .blocks
            .iter()
            .filter(|b| match b {
                Block::Text { .. } | Block::ToolResult { .. } => true,
                Block::ToolCall { call_id, .. } => !unanswered.contains(call_id),
                _ => false,
            })
            .cloned()
            .collect();
        if blocks.is_empty() {
            blocks.push(Block::Text {
                text: String::new(),
            });
        }
        messages.push(json!({"role":if turn.role==Role::Assistant{"assistant"}else{"user"},
            "blocks":blocks,"sourceMessage":turn,"sourceTimestamp":turn.timestamp.map(|t|t.to_rfc3339())}));
    }
    let mut request = json!({"version":1,"profile":destination.profile,"projectId":destination.project_id,
        "title":session.title.as_deref().unwrap_or("Imported session"),
        "source":{"agent":session.identity.agent_id,"id":session.identity.native_id},"messages":messages,
        "sourceSession":{"workspace":session.workspace,"attachments":session.attachments,"usage":session.usage,
            "losses":session.losses,"fidelity":session.fidelity,"agentVersion":session.identity.agent_version,"archivedTurns":archived_turns}});
    let bytes = serde_json::to_vec(&request).map_err(|e| err(e.to_string()))?;
    request["idempotencyKey"] = json!(format!(
        "portal-{}",
        destination
            .operation_id
            .clone()
            .unwrap_or_else(|| format!("{:x}", Sha256::digest(bytes)))
    ));
    Ok(request)
}

pub(super) fn plan(
    inst: &Installation,
    session: &CanonicalSession,
    destination: Option<&MigrationDestination>,
) -> Result<WritePlan> {
    let destination = destination.ok_or_else(|| {
        err("Choose the BentoKit destination profile and project before planning")
    })?;
    let request = payload(session, destination)?;
    let planned = cli(inst, "import-plan", Some(&request))?;
    let mut losses=vec![LossNote{code:LossCode::ContentSkipped,detail:"History is cloned as persistent BentoKit messages. Source system authority, credentials, permissions, active model and provider/tool runtime state are not transferred; historical tools are context, never re-executed.".into(),turn_id:None}];
    if session
        .timeline
        .iter()
        .any(|t| t.blocks.iter().any(|b| matches!(b, Block::Thinking { .. })))
    {
        losses.push(LossNote {
            code: LossCode::ThinkingDropped,
            detail: "Reasoning and signatures remain historical metadata and do not become a target reasoning checkpoint".into(),
            turn_id: None,
        });
    }
    if !session.unanswered_tool_calls().is_empty() {
        losses.push(LossNote {
            code: LossCode::InterruptedToolCall,
            detail: "Interrupted calls remain historical records and cannot be resumed as pending target tool executions".into(),
            turn_id: None,
        });
    }
    if !session.attachments.is_empty() {
        losses.push(LossNote{code:LossCode::SidechainNotConverted,detail:"Original attachment/branch references are retained as provenance; referenced file bytes are not copied into Core".into(),turn_id:None});
    }
    Ok(WritePlan {
        predicted_losses: losses,
        target_path_hint: format!(
            "{} · {}",
            inst.store_root,
            planned["nativeId"].as_str().unwrap_or("import")
        ),
    })
}

pub(super) fn write(
    inst: &Installation,
    session: &CanonicalSession,
    opts: &WriteOptions,
) -> Result<WrittenSession> {
    let destination = opts
        .destination
        .as_ref()
        .ok_or_else(|| err("Missing BentoKit destination"))?;
    let receipt = cli(inst, "import", Some(&payload(session, destination)?))?;
    let native_id = receipt["nativeId"]
        .as_str()
        .ok_or_else(|| err("BentoKit import returned no session id"))?
        .to_string();
    let revision = receipt["revision"]
        .as_str()
        .ok_or_else(|| err("BentoKit import returned no guarded undo revision"))?
        .to_string();
    let (profile, _) = super::read::split_id(&native_id)?;
    if profile != destination.profile {
        return Err(err("BentoKit import returned the wrong profile"));
    }
    let path = std::path::Path::new(&inst.store_root)
        .join("data/agents")
        .join(profile)
        .join("conversations.db")
        .display()
        .to_string();
    Ok(WrittenSession {
        native_id: native_id.clone(),
        primary_path: path.clone(),
        artifacts: vec![WrittenArtifact {
            kind: ArtifactKind::AgentSession,
            path,
            backup: None,
            content_hash: None,
            session: Some(SessionArtifact {
                installation: inst.clone(),
                native_id,
                revision,
            }),
        }],
    })
}

pub(super) fn undo(artifact: &SessionArtifact) -> Result<()> {
    let (profile, id) = super::read::split_id(&artifact.native_id)?;
    cli(
        &artifact.installation,
        "undo-import",
        Some(&json!({"profile":profile,"conversationId":id,"revision":artifact.revision})),
    )?;
    Ok(())
}
