//! End-to-end (file-level) migration tests exercised through the engine:
//! read → IR → write into a temp store → read-back → verify. Covers both
//! directions, a full A→B→A round-trip, and undo.

use std::path::PathBuf;
use std::sync::Arc;

use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::ir::LossCode;
use portal_core::migration::engine::{self, BriefConfig};
use portal_core::migration::ledger::Ledger;
use portal_core::migration::types::{MigrationKind, VerifyGrade};

use portal_adapters::claude_code::{claude_slug, ClaudeCodeAdapter};
use portal_adapters::codex::CodexAdapter;

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(rel)
}

fn temp_dir(tag: &str) -> PathBuf {
    // Unique-ish per test without Date/rand (unavailable): tag + line.
    std::env::temp_dir().join(format!("portal-mig-{tag}-{}", std::process::id()))
}

fn claude() -> Arc<dyn AgentAdapter> {
    Arc::new(ClaudeCodeAdapter)
}
fn codex() -> Arc<dyn AgentAdapter> {
    Arc::new(CodexAdapter)
}

fn install(store_root: PathBuf, version: &str) -> Installation {
    Installation {
        cli_path: None,
        version: Some(version.to_string()),
        store_root: store_root.display().to_string(),
    }
}

/// Fresh empty Codex store rooted at <tmp>/codex-home/sessions (so the title
/// index lands in codex-home, as on a real machine).
fn empty_codex(tmp: &std::path::Path) -> Installation {
    let sessions = tmp.join("codex-home").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    install(sessions, "codex-cli 0.143.0")
}

fn empty_claude(tmp: &std::path::Path) -> Installation {
    let root = tmp.join("claude-home").join("projects");
    std::fs::create_dir_all(&root).unwrap();
    install(root, "2.1.206 (Claude Code)")
}

#[test]
fn claude_to_codex_verifies_exact() {
    let tmp = temp_dir("c2x");
    std::fs::remove_dir_all(&tmp).ok();
    let source = install(fixture("claude-code/v2.1/store"), "2.1.206 (Claude Code)");
    let target = empty_codex(&tmp);

    let planned = engine::plan_native(
        &claude(),
        &source,
        &codex(),
        &target,
        &SessionLocator {
            native_id: "11111111-1111-1111-1111-111111111111".into(),
            store_path: None,
        },
    )
    .expect("plan");
    assert!(planned
        .report
        .predicted_losses
        .iter()
        .any(|l| l.code == LossCode::ThinkingDropped));

    let ledger = Ledger::new(tmp.join("appdata"));
    let result = engine::execute(&planned, &codex(), &target, &ledger).expect("execute");
    assert_eq!(
        result.verify.as_ref().unwrap().grade,
        VerifyGrade::Exact,
        "{:?}",
        result.verify.as_ref().unwrap().diffs
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn codex_to_claude_verifies_exact_and_regroups() {
    let tmp = temp_dir("x2c");
    std::fs::remove_dir_all(&tmp).ok();
    let source = install(fixture("codex/v0.143/store"), "codex-cli 0.143.0");
    let target = empty_claude(&tmp);

    let planned = engine::plan_native(
        &codex(),
        &source,
        &claude(),
        &target,
        &SessionLocator {
            native_id: "019f0000-0000-7000-8000-000000000001".into(),
            store_path: None,
        },
    )
    .expect("plan");

    // Codex reasoning is encrypted → surfaces as a loss on read; the Claude
    // writer additionally drops thinking on write.
    assert!(planned
        .report
        .predicted_losses
        .iter()
        .any(|l| l.code == LossCode::ThinkingDropped));

    let ledger = Ledger::new(tmp.join("appdata"));
    let result = engine::execute(&planned, &claude(), &target, &ledger).expect("execute");
    assert_eq!(
        result.verify.as_ref().unwrap().grade,
        VerifyGrade::Exact,
        "{:?}",
        result.verify.as_ref().unwrap().diffs
    );

    // The written Claude session must itself parse to a valid IR: the flat
    // Codex stream was regrouped into Anthropic shape with coherent tool
    // pairing (no orphan tool_result, no unanswered tool_use).
    let written = claude()
        .read_session(
            &target,
            &SessionLocator {
                native_id: result.target_native_id.clone(),
                store_path: Some(result.target_path.clone().into()),
            },
        )
        .expect("read written claude");
    assert!(written.validate().is_empty(), "{:?}", written.validate());
    assert!(written.unanswered_tool_calls().is_empty());

    // Tool ids were reminted to Claude's toolu_ shape.
    let has_toolu = written.timeline.iter().any(|t| {
        t.blocks.iter().any(|b| {
            matches!(b, portal_core::ir::Block::ToolCall { call_id, .. } if call_id.starts_with("toolu_"))
        })
    });
    assert!(has_toolu, "tool ids should be reminted to toolu_ form");

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn claude_round_trips_through_codex_and_back() {
    let tmp = temp_dir("rt");
    std::fs::remove_dir_all(&tmp).ok();
    let claude_source = install(fixture("claude-code/v2.1/store"), "2.1.206 (Claude Code)");
    let codex_mid = empty_codex(&tmp);
    let claude_back = empty_claude(&tmp);
    let ledger = Ledger::new(tmp.join("appdata"));

    // A → B
    let p1 = engine::plan_native(
        &claude(),
        &claude_source,
        &codex(),
        &codex_mid,
        &SessionLocator {
            native_id: "11111111-1111-1111-1111-111111111111".into(),
            store_path: None,
        },
    )
    .expect("plan A→B");
    let r1 = engine::execute(&p1, &codex(), &codex_mid, &ledger).expect("exec A→B");
    assert_eq!(r1.verify.as_ref().unwrap().grade, VerifyGrade::Exact);

    // B → A' (from the freshly written Codex session)
    let p2 = engine::plan_native(
        &codex(),
        &codex_mid,
        &claude(),
        &claude_back,
        &SessionLocator {
            native_id: r1.target_native_id.clone(),
            store_path: Some(r1.target_path.clone().into()),
        },
    )
    .expect("plan B→A'");
    let r2 = engine::execute(&p2, &claude(), &claude_back, &ledger).expect("exec B→A'");
    assert_eq!(
        r2.verify.as_ref().unwrap().grade,
        VerifyGrade::Exact,
        "{:?}",
        r2.verify.as_ref().unwrap().diffs
    );

    // The twice-migrated session still carries the original conversation text.
    let final_session = claude()
        .read_session(
            &claude_back,
            &SessionLocator {
                native_id: r2.target_native_id.clone(),
                store_path: Some(r2.target_path.clone().into()),
            },
        )
        .expect("read final");
    let all_text: String = final_session
        .timeline
        .iter()
        .flat_map(|t| &t.blocks)
        .filter_map(|b| match b {
            portal_core::ir::Block::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        all_text.contains("REST API"),
        "lost original user text: {all_text}"
    );
    assert!(
        all_text.contains("scaffolded"),
        "lost assistant reply: {all_text}"
    );

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn undo_removes_written_artifacts_and_guards_changed_files() {
    let tmp = temp_dir("undo");
    std::fs::remove_dir_all(&tmp).ok();
    let source = install(fixture("claude-code/v2.1/store"), "2.1.206 (Claude Code)");
    let target = empty_codex(&tmp);
    let ledger = Ledger::new(tmp.join("appdata"));

    let planned = engine::plan_native(
        &claude(),
        &source,
        &codex(),
        &target,
        &SessionLocator {
            native_id: "11111111-1111-1111-1111-111111111111".into(),
            store_path: None,
        },
    )
    .unwrap();
    let result = engine::execute(&planned, &codex(), &target, &ledger).unwrap();
    let target_path = PathBuf::from(&result.target_path);
    assert!(target_path.is_file());

    // Change guard: if the agent "continued" the session (the rollout file
    // changed), undo leaves that file alone.
    let entry = ledger.get(&result.migration_id).unwrap().unwrap();
    std::fs::write(&target_path, b"the agent continued this session\n").unwrap();
    let guarded = engine::undo(&entry, &ledger, false).unwrap();
    assert!(
        target_path.is_file(),
        "guarded undo must not delete a changed file"
    );
    assert!(
        guarded.skipped.iter().any(|s| s.contains("changed")),
        "changed rollout should be skipped: {:?}",
        guarded.skipped
    );

    // Forced undo removes it.
    let forced = engine::undo(&entry, &ledger, true).unwrap();
    assert!(!target_path.is_file(), "forced undo should remove the file");
    assert!(forced
        .removed
        .iter()
        .any(|r| r.contains(&result.target_native_id)));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn brief_mode_writes_handoff_into_workspace_and_ledgers() {
    let tmp = temp_dir("brief");
    std::fs::remove_dir_all(&tmp).ok();
    // A real workspace directory the brief can be written into.
    let workspace = tmp.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace_str = workspace.display().to_string();

    // A Claude store holding one session whose cwd IS that workspace.
    let store = tmp.join("claude").join("projects");
    let slug = claude_slug(&workspace_str);
    let session_dir = store.join(&slug);
    std::fs::create_dir_all(&session_dir).unwrap();
    let sid = "22222222-2222-2222-2222-222222222222";
    let jsonl = format!(
        "{u}\n{a}\n",
        u = serde_json::json!({
            "type": "user", "uuid": "u1", "parentUuid": null, "isMeta": false,
            "timestamp": "2026-07-01T10:00:00.000Z", "cwd": workspace_str,
            "sessionId": sid, "version": "2.1.206", "gitBranch": "main",
            "message": { "role": "user", "content": "Refactor the payment module" }
        }),
        a = serde_json::json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "timestamp": "2026-07-01T10:00:05.000Z", "cwd": workspace_str,
            "sessionId": sid, "version": "2.1.206", "gitBranch": "main",
            "message": { "role": "assistant", "model": "claude-fable-5",
                "content": [{ "type": "text", "text": "Refactor complete; next split the invoice service." }] }
        }),
    );
    std::fs::write(session_dir.join(format!("{sid}.jsonl")), jsonl).unwrap();

    let source = install(store, "2.1.206 (Claude Code)");
    // Codex target: brief mode only needs new_session_command (no store write).
    let target = install(tmp.join("codex").join("sessions"), "codex-cli 0.143.0");

    let planned = engine::plan(
        &claude(),
        &source,
        &codex(),
        &target,
        &SessionLocator {
            native_id: sid.to_string(),
            store_path: None,
        },
        MigrationKind::Brief,
        &BriefConfig::default(), // deterministic (no Ollama in CI)
    )
    .expect("plan brief");

    assert_eq!(planned.report.kind, MigrationKind::Brief);
    let preview = planned
        .report
        .brief_preview
        .as_ref()
        .expect("brief preview");
    assert!(preview.contains("Refactor the payment module"));
    assert!(!planned.report.brief_enhanced);

    let ledger = Ledger::new(tmp.join("appdata"));
    let result = engine::execute(&planned, &codex(), &target, &ledger).expect("execute brief");

    // Handoff written into the workspace, no native verify.
    assert_eq!(result.kind, MigrationKind::Brief);
    assert!(result.verify.is_none());
    let doc = workspace.join(".agent-portal").join("handoff-22222222.md");
    assert!(
        doc.is_file(),
        "handoff doc should exist at {}",
        doc.display()
    );
    let body = std::fs::read_to_string(&doc).unwrap();
    assert!(body.contains("# Handoff"));
    assert!(body.contains("Refactor the payment module"));

    // Launch is a fresh session seeded to read the handoff.
    assert_eq!(result.resume_command.program, "codex");
    assert!(result.resume_command.args[0].contains(".agent-portal/handoff-22222222.md"));

    // A .gitignore keeps the scratch out of the user's repo.
    assert!(workspace.join(".agent-portal").join(".gitignore").is_file());

    // Ledgered and undoable.
    let entry = ledger.get(&result.migration_id).unwrap().unwrap();
    let undo = engine::undo(&entry, &ledger, false).unwrap();
    assert!(!doc.is_file(), "undo should remove the handoff doc");
    assert!(undo.skipped.is_empty());

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn undo_restores_a_preexisting_index_and_removes_the_rollout() {
    let tmp = temp_dir("undo-idx");
    std::fs::remove_dir_all(&tmp).ok();
    let source = install(fixture("claude-code/v2.1/store"), "2.1.206 (Claude Code)");
    let target = empty_codex(&tmp);

    // Seed a pre-existing Codex title index (the real-machine case: migration
    // APPENDS to it, so undo must restore the backup rather than delete it).
    let index_path = PathBuf::from(&target.store_root)
        .parent()
        .unwrap()
        .join("session_index.jsonl");
    let original_index =
        "{\"id\":\"pre-existing\",\"thread_name\":\"kept\",\"updated_at\":\"2026-07-01T00:00:00Z\"}\n";
    std::fs::write(&index_path, original_index).unwrap();

    let ledger = Ledger::new(tmp.join("appdata"));
    let planned = engine::plan_native(
        &claude(),
        &source,
        &codex(),
        &target,
        &SessionLocator {
            native_id: "11111111-1111-1111-1111-111111111111".into(),
            store_path: None,
        },
    )
    .unwrap();
    let result = engine::execute(&planned, &codex(), &target, &ledger).unwrap();
    assert!(PathBuf::from(&result.target_path).is_file());
    assert!(std::fs::read_to_string(&index_path)
        .unwrap()
        .contains(&result.target_native_id));

    let entry = ledger.get(&result.migration_id).unwrap().unwrap();
    let report = engine::undo(&entry, &ledger, false).unwrap();

    assert!(
        !PathBuf::from(&result.target_path).is_file(),
        "rollout should be removed"
    );
    assert_eq!(
        std::fs::read_to_string(&index_path).unwrap(),
        original_index,
        "index must be restored to its pre-migration content"
    );
    assert!(
        report.skipped.is_empty(),
        "nothing should be skipped: {:?}",
        report.skipped
    );
    assert_eq!(
        report.removed.len(),
        2,
        "rollout + index restore: {:?}",
        report.removed
    );

    std::fs::remove_dir_all(&tmp).ok();
}
