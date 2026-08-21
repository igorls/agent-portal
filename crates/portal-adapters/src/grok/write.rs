//! Native write into Grok Build from Claude Code.
//!
//! Two paths, picked at write time:
//!
//! 1. **`grok import`** — older CLIs (validated against 0.2.93). The CLI reads
//!    a Claude project JSONL and materializes a Grok session. Windows builds
//!    of that importer only accept CWDs starting with `/`, so we rewrite
//!    `C:\…` → `/C:/…` for the import copy, then re-home the session and
//!    patch `summary.json` back to the real workspace path.
//! 2. **Store synthesis** — Grok 0.2.103+ removed `import`. Grok 1.0.x will
//!    resume a session that has `summary.json` (`chat_format_version = 1`)
//!    plus `chat_history.jsonl`. Validated against 1.0.5: `grok --resume`
//!    loads that pair and fills in `updates.jsonl` / events on first turn.
//!
//! We never call `grok import` on a CLI that no longer lists it (it would be
//! treated as a TUI prompt).

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use portal_core::dto::Installation;
use portal_core::error::{PortalError, Result};
use portal_core::ir::{
    tool_args_text, tool_output_text, Block, CanonicalSession, LossCode, LossNote, Role,
};
use portal_core::migration::types::{
    ArtifactKind, WriteOptions, WritePlan, WrittenArtifact, WrittenSession,
};
use portal_core::util::paths::{atomic_write, quick_hash};

/// Agent ids `grok import` is documented / validated to accept.
pub const NATIVE_IMPORT_SOURCES: &[&str] = &["claude-code"];

pub fn accepts_source(source_agent_id: &str) -> bool {
    NATIVE_IMPORT_SOURCES.contains(&source_agent_id)
}

pub fn plan_write(inst: &Installation, session: &CanonicalSession) -> Result<WritePlan> {
    ensure_compatible(session)?;

    let mut losses = session.losses.clone();
    let (_, synthesis_losses) = emit_chat_history(session);
    losses.extend(synthesis_losses);
    losses.push(LossNote {
        code: LossCode::UnknownRecord,
        detail: "native Claude→Grok uses `grok import` when the CLI still has it, otherwise a chat_history.jsonl store Grok 1.0+ can resume; thinking signatures are not preserved"
            .into(),
        turn_id: None,
    });

    let real_cwd = session.workspace.cwd.clone();
    let hint = PathBuf::from(&inst.store_root)
        .join(percent_encode(&real_cwd))
        .join("<new-grok-session-id>");

    Ok(WritePlan {
        predicted_losses: losses,
        target_path_hint: hint.display().to_string(),
    })
}

pub fn write_session(
    inst: &Installation,
    session: &CanonicalSession,
    _opts: &WriteOptions,
) -> Result<WrittenSession> {
    ensure_compatible(session)?;

    let real_cwd = session.workspace.cwd.as_str();
    if real_cwd.is_empty() {
        return Err(PortalError::Other(
            "cannot import into Grok without a working directory".into(),
        ));
    }

    if !grok_import_cli_available(inst) {
        return write_synthesized_store(inst, session);
    }

    let source_path = PathBuf::from(&session.identity.store_path);
    if !source_path.is_file() {
        return Err(PortalError::Other(format!(
            "Claude session file not found for import: {}",
            source_path.display()
        )));
    }

    let rewrite = cwd_needs_import_rewrite(real_cwd);
    let (import_path, temp_path) = if rewrite {
        let temp = write_rewritten_import_copy(&source_path, real_cwd)?;
        (temp.clone(), Some(temp))
    } else {
        (source_path.clone(), None)
    };

    let import = run_grok_import(inst, &import_path);
    if let Some(temp) = &temp_path {
        let _ = std::fs::remove_file(temp);
    }
    let import = import?;

    let native_id = import.session_id;
    let import_cwd = import.cwd.unwrap_or_else(|| {
        if rewrite {
            to_importable_cwd(real_cwd)
        } else {
            real_cwd.to_string()
        }
    });

    let store_root = PathBuf::from(&inst.store_root);
    let imported_dir = store_root
        .join(percent_encode(&import_cwd))
        .join(&native_id);

    if !imported_dir.is_dir() {
        return Err(PortalError::Other(format!(
            "grok import reported success but session dir missing: {}",
            imported_dir.display()
        )));
    }

    let final_dir = if rewrite && import_cwd != real_cwd {
        rehome_session(&store_root, &imported_dir, real_cwd, &native_id)?
    } else {
        imported_dir
    };

    // Ensure summary cwd matches the real workspace (rehome patches; no-op otherwise).
    patch_summary_cwd(&final_dir, real_cwd)?;

    let artifacts = collect_dir_artifacts(&final_dir)?;
    Ok(WrittenSession {
        native_id,
        primary_path: final_dir.display().to_string(),
        artifacts,
    })
}

fn ensure_compatible(session: &CanonicalSession) -> Result<()> {
    if !accepts_source(&session.identity.agent_id) {
        return Err(PortalError::Other(format!(
            "Grok native import only supports Claude Code sessions (got agent '{}'); use a handoff brief for other sources",
            session.identity.agent_id
        )));
    }
    Ok(())
}

#[derive(Debug)]
struct ImportResult {
    session_id: String,
    cwd: Option<String>,
}

fn grok_import_cli_available(inst: &Installation) -> bool {
    let Some(program) = inst.cli_path.as_deref().filter(|p| !p.is_empty()) else {
        return false;
    };
    grok_import_subcommand_available(program)
}

fn write_synthesized_store(
    inst: &Installation,
    session: &CanonicalSession,
) -> Result<WrittenSession> {
    // Grok's dashboard/session resolver indexes its native UUIDv7 directory
    // names. Reusing a source agent's valid UUIDv4 produces a store that
    // Agent Portal can read but `grok sessions` and `grok export` ignore.
    let native_id = uuid::Uuid::now_v7().to_string();
    write_synthesized_store_with_id(inst, session, &native_id)
}

fn write_synthesized_store_with_id(
    inst: &Installation,
    session: &CanonicalSession,
    native_id: &str,
) -> Result<WrittenSession> {
    let dest = PathBuf::from(&inst.store_root)
        .join(percent_encode(&session.workspace.cwd))
        .join(native_id);
    ensure_destination_available(&dest)?;

    let parent = dest.parent().ok_or_else(|| {
        PortalError::Other(format!("Grok session has no parent: {}", dest.display()))
    })?;
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{native_id}.agent-portal-{}.tmp",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir(&staging)?;

    if let Err(error) = write_synthesized_files(&staging, session, &native_id) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    // Recheck immediately before publishing to handle concurrent attempts.
    // An empty destination is incomplete residue from an interrupted older
    // write; a destination containing anything is real data and is preserved.
    if let Err(error) = remove_empty_destination(&dest) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staging, &dest) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error.into());
    }

    let artifacts = match collect_dir_artifacts(&dest) {
        Ok(artifacts) => artifacts,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(error);
        }
    };
    Ok(WrittenSession {
        native_id: native_id.to_string(),
        primary_path: dest.display().to_string(),
        artifacts,
    })
}

fn ensure_destination_available(dest: &Path) -> Result<()> {
    if !dest.exists() {
        return Ok(());
    }
    if dest.is_dir() && std::fs::read_dir(dest)?.next().is_none() {
        return Ok(());
    }
    Err(PortalError::Other(format!(
        "Grok session already exists at {}",
        dest.display()
    )))
}

fn remove_empty_destination(dest: &Path) -> Result<()> {
    ensure_destination_available(dest)?;
    if dest.exists() {
        std::fs::remove_dir(dest)?;
    }
    Ok(())
}

fn write_synthesized_files(
    dest: &Path,
    session: &CanonicalSession,
    native_id: &str,
) -> Result<Vec<WrittenArtifact>> {
    let (records, _losses) = emit_chat_history(session);
    if records.is_empty() {
        return Err(PortalError::Other(
            "cannot synthesize an empty Grok session".into(),
        ));
    }

    let mut chat = String::new();
    for record in &records {
        chat.push_str(&serde_json::to_string(record).map_err(|e| {
            PortalError::Other(format!("serializing Grok chat_history record: {e}"))
        })?);
        chat.push('\n');
    }
    let chat_path = dest.join("chat_history.jsonl");
    atomic_write(&chat_path, chat.as_bytes())?;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let title = session
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Imported from Claude Code");
    let chat_messages = records
        .iter()
        .filter(|record| record["type"].as_str() != Some("system"))
        .count();
    let summary = json!({
        "info": {
            "id": native_id,
            "cwd": session.workspace.cwd,
        },
        "session_summary": title,
        "generated_title": title,
        "created_at": now,
        "updated_at": now,
        "last_active_at": now,
        "num_messages": records.len(),
        "num_chat_messages": chat_messages,
        // Grok 1.0.5's dashboard deserializer requires these fields even for
        // imported histories; without them it silently omits the directory.
        "current_model_id": "grok-4.6",
        "next_trace_turn": 0,
        "chat_format_version": 1,
        "session_kind": "claude_import",
        "head_branch": session.workspace.git_branch,
    });
    let summary_bytes = serde_json::to_vec_pretty(&summary)
        .map_err(|e| PortalError::Other(format!("serializing Grok summary: {e}")))?;
    atomic_write(&dest.join("summary.json"), &summary_bytes)?;

    collect_dir_artifacts(dest)
}

fn emit_chat_history(session: &CanonicalSession) -> (Vec<Value>, Vec<LossNote>) {
    let unanswered: HashSet<String> = session.unanswered_tool_calls().into_iter().collect();
    let mut records = Vec::new();
    let mut thinking_dropped = 0u32;
    let mut interrupted = 0u32;

    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        match turn.role {
            Role::System => {}
            Role::User | Role::Tool => {
                for block in &turn.blocks {
                    match block {
                        Block::Text { text } if !text.trim().is_empty() => {
                            // Keep distinct canonical text blocks distinct.
                            // Joining them changes the text stream and makes a
                            // valid synthesized session fail read-back verification.
                            records.push(json!({
                                "type": "user",
                                "content": [{ "type": "text", "text": text }],
                            }));
                        }
                        Block::ToolResult {
                            call_id, output, ..
                        } => {
                            records.push(json!({
                                "type": "tool_result",
                                "tool_call_id": call_id,
                                "content": tool_output_text(output),
                            }));
                        }
                        Block::Thinking { .. } => thinking_dropped += 1,
                        _ => {}
                    }
                }
            }
            Role::Assistant => {
                let mut texts = Vec::new();
                let mut tool_calls = Vec::new();
                for block in &turn.blocks {
                    match block {
                        Block::Text { text } => {
                            if !texts.is_empty() || !tool_calls.is_empty() {
                                flush_assistant(
                                    &mut records,
                                    &mut texts,
                                    &mut tool_calls,
                                    turn.model.as_deref(),
                                );
                            }
                            texts.push(text.clone());
                        }
                        Block::Thinking { .. } => thinking_dropped += 1,
                        Block::ToolCall {
                            call_id,
                            name,
                            arguments,
                        } => {
                            if unanswered.contains(call_id) {
                                interrupted += 1;
                                continue;
                            }
                            tool_calls.push(json!({
                                "id": call_id,
                                "name": name,
                                "arguments": tool_args_text(arguments),
                            }));
                        }
                        Block::ToolResult {
                            call_id, output, ..
                        } => {
                            flush_assistant(
                                &mut records,
                                &mut texts,
                                &mut tool_calls,
                                turn.model.as_deref(),
                            );
                            records.push(json!({
                                "type": "tool_result",
                                "tool_call_id": call_id,
                                "content": tool_output_text(output),
                            }));
                        }
                        Block::Compaction { summary } => {
                            texts.push(format!("[compacted] {summary}"));
                        }
                        Block::Meta { .. } => {}
                    }
                }
                flush_assistant(
                    &mut records,
                    &mut texts,
                    &mut tool_calls,
                    turn.model.as_deref(),
                );
            }
        }
    }

    let mut losses = Vec::new();
    if thinking_dropped > 0 {
        losses.push(LossNote {
            code: LossCode::ThinkingDropped,
            detail: format!(
                "{thinking_dropped} thinking block(s) dropped — provider reasoning signatures can't be reconstructed"
            ),
            turn_id: None,
        });
    }
    if interrupted > 0 {
        losses.push(LossNote {
            code: LossCode::InterruptedToolCall,
            detail: format!(
                "{interrupted} tool call(s) had no result (interrupted) and will be dropped"
            ),
            turn_id: None,
        });
    }
    (records, losses)
}

fn flush_assistant(
    records: &mut Vec<Value>,
    texts: &mut Vec<String>,
    tool_calls: &mut Vec<Value>,
    model: Option<&str>,
) {
    if texts.is_empty() && tool_calls.is_empty() {
        return;
    }
    let mut record = json!({
        "type": "assistant",
        "content": texts.join("\n\n"),
        "tool_calls": tool_calls.clone(),
    });
    if let Some(model) = model {
        record["model_id"] = json!(model);
    }
    records.push(record);
    texts.clear();
    tool_calls.clear();
}

fn grok_import_subcommand_available(program: &str) -> bool {
    let Ok(output) = Command::new(program).arg("help").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("import ") || trimmed == "import"
    })
}

fn run_grok_import(inst: &Installation, import_path: &Path) -> Result<ImportResult> {
    let program = inst
        .cli_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .unwrap_or("grok");

    let grok_home = Path::new(&inst.store_root)
        .parent()
        .ok_or_else(|| {
            PortalError::Other(format!(
                "Grok store root has no parent (expected …/.grok/sessions): {}",
                inst.store_root
            ))
        })?
        .to_path_buf();

    // Guard: recent Grok CLIs removed `import` (it is no longer a subcommand and
    // would be treated as a free-form TUI prompt). Fail closed with a clear note.
    if !grok_import_subcommand_available(program) {
        return Err(PortalError::Other(
            "this Grok Build CLI no longer provides `grok import`; native Claude→Grok migration is unavailable until a replacement lands"
                .into(),
        ));
    }

    let output = Command::new(program)
        .args(["import", "--json"])
        .arg(import_path)
        .env("GROK_HOME", &grok_home)
        .output()
        .map_err(|e| {
            PortalError::Other(format!(
                "failed to run `{program} import`: {e}. Is the Grok Build CLI installed?"
            ))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut last_error = None;
    let mut success: Option<ImportResult> = None;

    for line in stdout.lines().chain(stderr.lines()) {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let outcome = value["outcome"].as_str().unwrap_or("");
        match outcome {
            "imported" | "already_imported" => {
                if let Some(session_id) = value["sessionId"].as_str() {
                    // Prefer real session UUIDs over path strings used on failure lines.
                    if uuid::Uuid::parse_str(session_id).is_ok() {
                        success = Some(ImportResult {
                            session_id: session_id.to_string(),
                            cwd: value["cwd"].as_str().map(str::to_string),
                        });
                    }
                }
            }
            "failed" => {
                last_error = Some(
                    value["error"]
                        .as_str()
                        .unwrap_or("import failed")
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    if let Some(ok) = success {
        return Ok(ok);
    }

    let detail = last_error
        .or_else(|| {
            let combined = format!("{stdout}{stderr}");
            let trimmed = combined.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .unwrap_or_else(|| format!("exit status {:?}", output.status.code()));

    Err(PortalError::Other(format!(
        "grok import failed for {}: {detail}",
        import_path.display()
    )))
}

/// True when Grok's importer will reject this cwd (Windows drive paths).
pub(crate) fn cwd_needs_import_rewrite(cwd: &str) -> bool {
    !cwd.starts_with('/')
}

/// `C:\Users\me\proj` → `/C:/Users/me/proj` so Grok's absolute-path check passes.
pub(crate) fn to_importable_cwd(cwd: &str) -> String {
    if cwd.starts_with('/') {
        return cwd.to_string();
    }
    // Prefix `/` so Grok treats the path as absolute; keep drive letter form.
    format!("/{}", cwd.replace('\\', "/"))
}

fn write_rewritten_import_copy(source: &Path, real_cwd: &str) -> Result<PathBuf> {
    let importable = to_importable_cwd(real_cwd);
    let temp = std::env::temp_dir().join(format!(
        "portal-grok-import-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));

    let file = std::fs::File::open(source)?;
    let reader = std::io::BufReader::new(file);
    let mut out = std::fs::File::create(&temp)?;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            writeln!(out)?;
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(mut value) => {
                if value.get("cwd").and_then(|v| v.as_str()) == Some(real_cwd) {
                    value["cwd"] = Value::String(importable.clone());
                } else if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str()) {
                    // Defensive: rewrite any Windows-looking cwd on the event.
                    if cwd_needs_import_rewrite(cwd) {
                        value["cwd"] = Value::String(to_importable_cwd(cwd));
                    }
                }
                let serialized = serde_json::to_string(&value).map_err(|e| {
                    PortalError::Other(format!("serializing rewritten Claude event: {e}"))
                })?;
                writeln!(out, "{serialized}")?;
            }
            Err(_) => {
                // Preserve unparseable lines as-is.
                writeln!(out, "{line}")?;
            }
        }
    }

    Ok(temp)
}

fn rehome_session(
    store_root: &Path,
    imported_dir: &Path,
    real_cwd: &str,
    native_id: &str,
) -> Result<PathBuf> {
    let dest_parent = store_root.join(percent_encode(real_cwd));
    std::fs::create_dir_all(&dest_parent)?;
    let dest = dest_parent.join(native_id);
    remove_empty_destination(&dest)?;
    std::fs::rename(imported_dir, &dest).map_err(|e| {
        PortalError::Other(format!(
            "failed to re-home imported Grok session to real workspace path: {e}"
        ))
    })?;

    // Best-effort: drop empty import workspace dir.
    if let Some(parent) = imported_dir.parent() {
        let _ = std::fs::remove_dir(parent);
    }

    Ok(dest)
}

fn patch_summary_cwd(session_dir: &Path, real_cwd: &str) -> Result<()> {
    let summary_path = session_dir.join("summary.json");
    if !summary_path.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&summary_path)?;
    let mut value: Value = serde_json::from_str(&raw).map_err(|e| PortalError::Parse {
        path: summary_path.display().to_string(),
        detail: e.to_string(),
    })?;
    if value["info"]["cwd"].as_str() != Some(real_cwd) {
        value["info"]["cwd"] = Value::String(real_cwd.into());
        let pretty = serde_json::to_vec_pretty(&value)
            .map_err(|e| PortalError::Other(format!("serializing patched Grok summary: {e}")))?;
        std::fs::write(&summary_path, pretty)?;
    }
    Ok(())
}

fn collect_dir_artifacts(session_dir: &Path) -> Result<Vec<WrittenArtifact>> {
    let mut artifacts = Vec::new();
    collect_dir_artifacts_rec(session_dir, &mut artifacts)?;
    if artifacts.is_empty() {
        return Err(PortalError::Other(format!(
            "imported Grok session has no files: {}",
            session_dir.display()
        )));
    }
    Ok(artifacts)
}

fn collect_dir_artifacts_rec(dir: &Path, out: &mut Vec<WrittenArtifact>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_dir_artifacts_rec(&path, out)?;
        } else if path.is_file() {
            let bytes = std::fs::read(&path)?;
            out.push(WrittenArtifact {
                kind: ArtifactKind::File,
                path: path.display().to_string(),
                backup: None,
                content_hash: Some(quick_hash(&bytes)),
            });
        }
    }
    Ok(())
}

/// Percent-encode a workspace path the way Grok names session parent dirs.
pub(crate) fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_windows_workspace_key_like_grok() {
        assert_eq!(percent_encode(r"P:\demo\app"), "P%3A%5Cdemo%5Capp");
        assert_eq!(percent_encode(r"C:\Users\igorl"), "C%3A%5CUsers%5Cigorl");
    }

    #[test]
    fn importable_cwd_rewrites_windows_drive_paths() {
        assert!(cwd_needs_import_rewrite(r"C:\Users\igorl\proj"));
        assert!(!cwd_needs_import_rewrite("/tmp/demo"));
        assert_eq!(
            to_importable_cwd(r"C:\Users\igorl\proj"),
            "/C:/Users/igorl/proj"
        );
        assert_eq!(to_importable_cwd("/tmp/demo"), "/tmp/demo");
    }

    #[test]
    fn only_claude_code_is_native_source() {
        assert!(accepts_source("claude-code"));
        assert!(!accepts_source("codex"));
        assert!(!accepts_source("grok-build"));
        assert!(!accepts_source("opencode"));
    }

    #[test]
    fn synthesizes_resumeable_store_without_grok_cli() {
        use chrono::Utc;
        use portal_core::ir::{
            Block, CanonicalSession, Fidelity, Role, SessionIdentity, Turn, UsageTotals, Workspace,
            IR_VERSION,
        };
        use serde_json::{json, Value};
        use std::path::PathBuf;

        let tmp = std::env::temp_dir().join(format!(
            "portal-grok-synth-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cwd = tmp.join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let source_sid = "55555555-5555-4555-8555-555555555555";
        let target_sid = "018f7777-7777-7777-8777-777777777777";
        let session = CanonicalSession {
            ir_version: IR_VERSION,
            identity: SessionIdentity {
                portal_id: uuid::Uuid::now_v7().to_string(),
                native_id: source_sid.into(),
                agent_id: "claude-code".into(),
                store_path: "missing.jsonl".into(),
                agent_version: Some("2.1.206".into()),
                read_at: Utc::now(),
            },
            workspace: Workspace {
                cwd: cwd.display().to_string(),
                cwd_normalized: cwd.display().to_string(),
                git_branch: Some("main".into()),
                project_label: "workspace".into(),
            },
            title: Some("Synthetic Grok import".into()),
            timeline: vec![
                Turn {
                    id: "u1".into(),
                    parent_id: None,
                    role: Role::User,
                    timestamp: None,
                    model: None,
                    is_meta: false,
                    blocks: vec![Block::Text {
                        text: "Scaffold the importer".into(),
                    }],
                    usage: None,
                    raw: None,
                },
                Turn {
                    id: "a1".into(),
                    parent_id: Some("u1".into()),
                    role: Role::Assistant,
                    timestamp: None,
                    model: Some("claude-sonnet-4".into()),
                    is_meta: false,
                    blocks: vec![
                        Block::Text {
                            text: "Reading the module.".into(),
                        },
                        Block::ToolCall {
                            call_id: "toolu_01".into(),
                            name: "Read".into(),
                            arguments: json!({"file_path": "mod.rs"}),
                        },
                    ],
                    usage: None,
                    raw: None,
                },
                Turn {
                    id: "u2".into(),
                    parent_id: Some("a1".into()),
                    role: Role::User,
                    timestamp: None,
                    model: None,
                    is_meta: false,
                    blocks: vec![Block::ToolResult {
                        call_id: "toolu_01".into(),
                        output: json!("pub struct GrokAdapter;"),
                        is_error: false,
                    }],
                    usage: None,
                    raw: None,
                },
                Turn {
                    id: "a2".into(),
                    parent_id: Some("u2".into()),
                    role: Role::Assistant,
                    timestamp: None,
                    model: Some("claude-sonnet-4".into()),
                    is_meta: false,
                    blocks: vec![
                        Block::Text {
                            text: "Importer ready.".into(),
                        },
                        Block::Text {
                            text: "Distinct follow-up block.".into(),
                        },
                    ],
                    usage: None,
                    raw: None,
                },
            ],
            attachments: Vec::new(),
            usage: UsageTotals::default(),
            losses: Vec::new(),
            fidelity: Fidelity::Partial,
        };

        let inst = Installation {
            cli_path: None,
            version: Some("grok 1.0.5".into()),
            store_root: tmp.join("sessions").display().to_string(),
        };
        // An interrupted prior attempt may leave the predicted directory but
        // no Grok files. A retry should reclaim that empty residue.
        let expected_dir = PathBuf::from(&inst.store_root)
            .join(percent_encode(&session.workspace.cwd))
            .join(target_sid);
        std::fs::create_dir_all(&expected_dir).unwrap();
        let written = write_synthesized_store_with_id(&inst, &session, target_sid).unwrap();
        assert_eq!(written.native_id, target_sid);
        let dir = PathBuf::from(&written.primary_path);
        let chat = std::fs::read_to_string(dir.join("chat_history.jsonl")).unwrap();
        assert!(chat.contains("Scaffold the importer"));
        assert!(chat.contains("\"name\":\"Read\""));
        assert!(chat.contains("tool_result"));
        let summary: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap())
                .unwrap();
        assert_eq!(summary["chat_format_version"].as_u64(), Some(1));
        assert_eq!(summary["current_model_id"].as_str(), Some("grok-4.6"));
        assert_eq!(summary["next_trace_turn"].as_u64(), Some(0));
        assert_eq!(summary["session_kind"].as_str(), Some("claude_import"));
        assert_eq!(
            summary["info"]["cwd"].as_str(),
            Some(cwd.display().to_string().as_str())
        );
        let round_tripped = crate::grok::read::read_session(
            &inst,
            &portal_core::adapter::SessionLocator {
                native_id: target_sid.into(),
                store_path: Some(dir.clone()),
            },
        )
        .unwrap();
        let text_verify =
            portal_core::migration::verify::compare_text_stream(&session, &round_tripped);
        assert_eq!(
            text_verify.grade,
            portal_core::migration::types::VerifyGrade::Equivalent,
            "{:?}",
            text_verify.diffs
        );

        // Once real session data exists, a retry must not overwrite it.
        let duplicate = write_synthesized_store_with_id(&inst, &session, target_sid).unwrap_err();
        assert!(duplicate
            .to_string()
            .contains("Grok session already exists"));
        assert_eq!(
            std::fs::read_to_string(dir.join("chat_history.jsonl")).unwrap(),
            chat
        );

        // Public synthesis always allocates a Grok-native UUIDv7 instead of
        // reusing the source agent's UUID (which may be UUIDv4).
        let fresh = write_session(&inst, &session, &WriteOptions::default()).unwrap();
        let parsed = uuid::Uuid::parse_str(&fresh.native_id).unwrap();
        assert_eq!(parsed.get_version_num(), 7);
        assert_ne!(fresh.native_id, source_sid);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
