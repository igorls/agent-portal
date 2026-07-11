//! The migration pipeline: read → validate → plan (dry-run stops here) →
//! write → verify (read-back) → ledger. Hard safety rules live here, not in
//! adapters: sources are never mutated, failed verification rolls the write
//! back, and every created artifact is ledger-recorded before we report
//! success.

use std::sync::Arc;

use chrono::Utc;

use crate::adapter::{AgentAdapter, SessionLocator};
use crate::dto::Installation;
use crate::error::{PortalError, Result};
use crate::ir::{tool_args_text, tool_output_text, Block, CanonicalSession};
use crate::migration::ledger::{Ledger, LedgerEntry};
use crate::migration::types::{
    ArtifactKind, BlockCensus, DryRunReport, MigrationKind, MigrationResult, UndoReport,
    VerifyGrade, WriteOptions, WrittenArtifact,
};
use crate::migration::{brief, ollama, verify};
use crate::util::paths::{atomic_write, quick_hash};

/// Whether and how to run the optional local-LLM enrichment of a brief.
#[derive(Debug, Clone)]
pub struct BriefConfig {
    pub enhance: bool,
    pub base_url: String,
    pub model: String,
}

impl Default for BriefConfig {
    fn default() -> Self {
        Self {
            enhance: false,
            base_url: ollama::DEFAULT_BASE_URL.to_string(),
            model: ollama::DEFAULT_MODEL.to_string(),
        }
    }
}

pub struct PlannedMigration {
    pub report: DryRunReport,
    pub session: CanonicalSession,
    pub source_agent: String,
    pub target_agent: String,
    pub kind: MigrationKind,
    /// Brief mode: the exact text to write (deterministic, possibly enriched).
    pub brief_text: Option<String>,
}

pub fn census(session: &CanonicalSession) -> BlockCensus {
    let mut c = BlockCensus::default();
    for turn in &session.timeline {
        for block in &turn.blocks {
            match block {
                Block::Text { .. } => c.text += 1,
                Block::Thinking { .. } => c.thinking += 1,
                Block::ToolCall { .. } => c.tool_calls += 1,
                Block::ToolResult { .. } => c.tool_results += 1,
                Block::Compaction { .. } => c.compaction += 1,
                Block::Meta { .. } => c.meta += 1,
            }
        }
    }
    c
}

/// Rough token estimate of a session's transferable content (~4 chars/token).
/// Meta turns aren't written, so they're excluded; this approximates how much a
/// target would have to load to resume the migrated session.
pub fn estimate_tokens(session: &CanonicalSession) -> u32 {
    let mut chars: u64 = 0;
    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        for block in &turn.blocks {
            chars += match block {
                Block::Text { text } => text.len() as u64,
                Block::Thinking { text, .. } => text.len() as u64,
                Block::ToolCall { name, arguments, .. } => {
                    (name.len() + tool_args_text(arguments).len()) as u64
                }
                Block::ToolResult { output, .. } => tool_output_text(output).len() as u64,
                Block::Compaction { summary } => summary.len() as u64,
                Block::Meta { .. } => 0,
            };
        }
    }
    (chars / 4).min(u32::MAX as u64) as u32
}

#[allow(clippy::too_many_arguments)]
pub fn plan(
    source_adapter: &Arc<dyn AgentAdapter>,
    source_inst: &Installation,
    target_adapter: &Arc<dyn AgentAdapter>,
    target_inst: &Installation,
    locator: &SessionLocator,
    kind: MigrationKind,
    brief_cfg: &BriefConfig,
) -> Result<PlannedMigration> {
    let session = source_adapter.read_session(source_inst, locator)?;

    let issues = session.validate();
    if !issues.is_empty() {
        return Err(PortalError::Other(format!(
            "source session fails IR validation: {}",
            issues.join("; ")
        )));
    }

    let mut warnings = Vec::new();
    let recently_modified = std::fs::metadata(&session.identity.store_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(|elapsed| elapsed.as_secs() < 120)
        .unwrap_or(false);
    if recently_modified {
        warnings.push(
            "source session was modified in the last two minutes — the agent may still be working in it"
                .to_string(),
        );
    }
    if session.workspace.cwd.is_empty() {
        warnings.push("source session has no recorded working directory".to_string());
    } else if !std::path::Path::new(&session.workspace.cwd).is_dir() {
        warnings.push(format!(
            "workspace directory {} no longer exists",
            session.workspace.cwd
        ));
    }

    // A native migration loads the whole transcript into the target at once; if
    // the estimate runs well past the target's default window, nudge toward a
    // brief instead of letting the user hit an unloadable session.
    let estimated_tokens = estimate_tokens(&session);
    let target_context_tokens = target_adapter.capabilities().context_tokens;
    if kind == MigrationKind::Native {
        if let Some(limit) = target_context_tokens {
            if estimated_tokens > limit {
                warnings.push(format!(
                    "large session (≈{}k tokens) may exceed {}'s default context window (≈{}k) and fail to load — consider the handoff brief; a larger-context model (e.g. Claude Sonnet 1M) can still resume it natively",
                    estimated_tokens / 1000,
                    target_adapter.display_name(),
                    limit / 1000,
                ));
            }
        }
    }

    let unanswered = session.unanswered_tool_calls();
    let mut report = DryRunReport {
        plan_id: uuid::Uuid::now_v7().to_string(),
        kind,
        source_agent: source_adapter.id().to_string(),
        source_native_id: session.identity.native_id.clone(),
        source_title: session.title.clone(),
        target_agent: target_adapter.id().to_string(),
        cwd: session.workspace.cwd.clone(),
        turn_count: session.timeline.len() as u32,
        census: census(&session),
        predicted_losses: session.losses.clone(),
        unanswered_tool_calls: unanswered.len() as u32,
        estimated_tokens,
        target_context_tokens,
        warnings,
        target_path_hint: String::new(),
        resume_preview: String::new(),
        brief_preview: None,
        brief_enhanced: false,
    };

    let mut brief_text = None;
    match kind {
        MigrationKind::Native => {
            let write_plan = target_adapter.plan_write(target_inst, &session)?;
            report
                .predicted_losses
                .extend(write_plan.predicted_losses.clone());
            report.target_path_hint = write_plan.target_path_hint;
            report.resume_preview = target_adapter
                .resume_command(target_inst, "<new-session-id>", &session.workspace.cwd)
                .map(|c| c.display())
                .unwrap_or_else(|_| "(resume command unavailable)".to_string());
        }
        MigrationKind::Brief => {
            let facts = brief::extract_facts(&session);
            let deterministic = brief::render(&facts);
            let (text, enhanced) = if brief_cfg.enhance {
                match ollama::enrich(&brief_cfg.base_url, &brief_cfg.model, &deterministic) {
                    Some(polished) => (polished, true),
                    None => (deterministic, false),
                }
            } else {
                (deterministic, false)
            };
            report.brief_preview = Some(text.clone());
            report.brief_enhanced = enhanced;
            report.target_path_hint =
                brief_doc_path(&session.workspace.cwd, &report.source_native_id)
                    .display()
                    .to_string();
            report.resume_preview = target_adapter
                .new_session_command(target_inst, &session.workspace.cwd, "Read the handoff…")
                .map(|c| c.display())
                .unwrap_or_else(|_| "(launch command unavailable)".to_string());
            brief_text = Some(text);
        }
    }

    Ok(PlannedMigration {
        report,
        source_agent: source_adapter.id().to_string(),
        target_agent: target_adapter.id().to_string(),
        kind,
        brief_text,
        session,
    })
}

/// Convenience for the common native-migration case (tests, tooling).
pub fn plan_native(
    source_adapter: &Arc<dyn AgentAdapter>,
    source_inst: &Installation,
    target_adapter: &Arc<dyn AgentAdapter>,
    target_inst: &Installation,
    locator: &SessionLocator,
) -> Result<PlannedMigration> {
    plan(
        source_adapter,
        source_inst,
        target_adapter,
        target_inst,
        locator,
        MigrationKind::Native,
        &BriefConfig::default(),
    )
}

/// Where a handoff document lands: inside the workspace, in a portal folder.
fn brief_doc_path(cwd: &str, source_id: &str) -> std::path::PathBuf {
    let short: String = source_id.chars().take(8).collect();
    std::path::Path::new(cwd)
        .join(".agent-portal")
        .join(format!("handoff-{short}.md"))
}

pub fn execute(
    planned: &PlannedMigration,
    target_adapter: &Arc<dyn AgentAdapter>,
    target_inst: &Installation,
    ledger: &Ledger,
) -> Result<MigrationResult> {
    match planned.kind {
        MigrationKind::Native => execute_native(planned, target_adapter, target_inst, ledger),
        MigrationKind::Brief => execute_brief(planned, target_adapter, target_inst, ledger),
    }
}

/// Brief mode: write the handoff document into the workspace and launch a
/// fresh target session pointed at it. Nothing is written into the target's
/// session store, so there's no read-back to verify.
fn execute_brief(
    planned: &PlannedMigration,
    target_adapter: &Arc<dyn AgentAdapter>,
    target_inst: &Installation,
    ledger: &Ledger,
) -> Result<MigrationResult> {
    let cwd = &planned.session.workspace.cwd;
    if cwd.is_empty() || !std::path::Path::new(cwd).is_dir() {
        return Err(PortalError::Other(
            "brief mode needs an existing workspace directory to write the handoff into"
                .to_string(),
        ));
    }
    let text = planned
        .brief_text
        .clone()
        .unwrap_or_else(|| "# Handoff\n".to_string());
    let doc_path = brief_doc_path(cwd, &planned.session.identity.native_id);
    if let Some(dir) = doc_path.parent() {
        std::fs::create_dir_all(dir)?;
        // Keep the portal's scratch out of the user's git history.
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "*\n");
        }
    }
    atomic_write(&doc_path, text.as_bytes())?;

    let migration_id = uuid::Uuid::now_v7().to_string();
    let rel = format!(
        ".agent-portal/{}",
        doc_path.file_name().unwrap().to_string_lossy()
    );
    let prompt = format!(
        "Read {rel} — it's a handoff brief describing work migrated from another agent. Pick up where it leaves off, confirming the current state with me first."
    );
    let resume_command = target_adapter.new_session_command(target_inst, cwd, &prompt)?;

    ledger.append(&LedgerEntry {
        id: migration_id.clone(),
        at: Utc::now(),
        source_agent: planned.source_agent.clone(),
        source_native_id: planned.session.identity.native_id.clone(),
        source_path: planned.session.identity.store_path.clone(),
        target_agent: planned.target_agent.clone(),
        target_native_id: doc_path.display().to_string(),
        artifacts: vec![WrittenArtifact {
            kind: ArtifactKind::File,
            path: doc_path.display().to_string(),
            backup: None,
            content_hash: Some(quick_hash(text.as_bytes())),
        }],
        verify_grade: VerifyGrade::Exact,
        undone: false,
    })?;

    Ok(MigrationResult {
        migration_id,
        kind: MigrationKind::Brief,
        target_agent: planned.target_agent.clone(),
        target_native_id: doc_path.display().to_string(),
        target_path: doc_path.display().to_string(),
        verify: None,
        resume_command,
        finished_at: Utc::now(),
    })
}

fn execute_native(
    planned: &PlannedMigration,
    target_adapter: &Arc<dyn AgentAdapter>,
    target_inst: &Installation,
    ledger: &Ledger,
) -> Result<MigrationResult> {
    let opts = WriteOptions {
        title: planned.session.title.clone(),
    };

    let written = target_adapter.write_session(target_inst, &planned.session, &opts)?;

    // Read-back verification through the target's own reader.
    let verify_report = match target_adapter.read_session(
        target_inst,
        &SessionLocator {
            native_id: written.native_id.clone(),
            store_path: Some(written.primary_path.clone().into()),
        },
    ) {
        Ok(round_tripped) => verify::compare(&planned.session, &round_tripped),
        Err(e) => crate::migration::types::VerifyReport {
            grade: VerifyGrade::Failed,
            compared_blocks: 0,
            diffs: vec![format!("read-back failed: {e}")],
        },
    };

    if verify_report.grade == VerifyGrade::Failed {
        rollback(&written.artifacts);
        return Err(PortalError::Other(format!(
            "verification failed, migration rolled back: {}",
            verify_report.diffs.join("; ")
        )));
    }

    let migration_id = uuid::Uuid::now_v7().to_string();
    let resume_command = target_adapter.resume_command(
        target_inst,
        &written.native_id,
        &planned.session.workspace.cwd,
    )?;

    ledger.append(&LedgerEntry {
        id: migration_id.clone(),
        at: Utc::now(),
        source_agent: planned.source_agent.clone(),
        source_native_id: planned.session.identity.native_id.clone(),
        source_path: planned.session.identity.store_path.clone(),
        target_agent: planned.target_agent.clone(),
        target_native_id: written.native_id.clone(),
        artifacts: written.artifacts.clone(),
        verify_grade: verify_report.grade,
        undone: false,
    })?;

    Ok(MigrationResult {
        migration_id,
        kind: MigrationKind::Native,
        target_agent: planned.target_agent.clone(),
        target_native_id: written.native_id,
        target_path: written.primary_path,
        verify: Some(verify_report),
        resume_command,
        finished_at: Utc::now(),
    })
}

/// Best-effort cleanup of a failed write: delete created files, restore
/// backed-up index files. Never touches anything not in the artifact list.
fn rollback(artifacts: &[WrittenArtifact]) {
    for artifact in artifacts {
        match artifact.kind {
            ArtifactKind::File => {
                let _ = std::fs::remove_file(&artifact.path);
            }
            ArtifactKind::IndexAppendLine => {
                if let Some(backup) = &artifact.backup {
                    let _ = std::fs::copy(backup, &artifact.path);
                }
            }
        }
    }
}

/// Undo a migration: delete the artifacts it created (files) and restore any
/// index it appended to. Because migration only ever *creates* target
/// artifacts, undo is always complete. A migrated file that has changed since
/// (the agent resumed and continued it) is left in place — losing that work to
/// an undo would be worse than an orphaned session.
pub fn undo(entry: &LedgerEntry, ledger: &Ledger, force: bool) -> Result<UndoReport> {
    let mut removed = Vec::new();
    let mut skipped = Vec::new();

    // Restore index appends first, then delete files.
    for artifact in &entry.artifacts {
        if artifact.kind == ArtifactKind::IndexAppendLine {
            match &artifact.backup {
                Some(backup) => match std::fs::copy(backup, &artifact.path) {
                    Ok(_) => {
                        let _ = std::fs::remove_file(backup);
                        removed.push(format!("restored index {}", artifact.path));
                    }
                    Err(e) => skipped.push(format!("could not restore {}: {e}", artifact.path)),
                },
                None => skipped.push(format!("no backup for index {}", artifact.path)),
            }
        }
    }

    for artifact in &entry.artifacts {
        if artifact.kind != ArtifactKind::File {
            continue;
        }
        let path = std::path::Path::new(&artifact.path);
        if !path.exists() {
            removed.push(format!("{} (already gone)", artifact.path));
            continue;
        }
        // Change guard: if the file no longer matches what we wrote, the agent
        // likely continued it — don't delete unless forced.
        if !force {
            if let Some(expected) = &artifact.content_hash {
                let current = std::fs::read(path).map(|b| quick_hash(&b)).ok();
                if current.as_deref() != Some(expected.as_str()) {
                    skipped.push(format!(
                        "{} — changed since migration (the agent may have continued it); use force to remove",
                        artifact.path
                    ));
                    continue;
                }
            }
        }
        match std::fs::remove_file(path) {
            Ok(_) => removed.push(artifact.path.clone()),
            Err(e) => skipped.push(format!("{}: {e}", artifact.path)),
        }
    }

    ledger.mark_undone(&entry.id)?;

    Ok(UndoReport {
        migration_id: entry.id.clone(),
        removed,
        skipped,
    })
}
