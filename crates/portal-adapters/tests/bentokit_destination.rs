//! Real Core protocol integration, isolated data and no model calls.
//! BENTOKIT_CORE_TEST_ROOT=/path/to/bentokit-core cargo test -p portal-adapters --test bentokit_destination -- --ignored
#![cfg(unix)]
use portal_adapters::{bentokit::BentoKitAdapter, claude_code::ClaudeCodeAdapter};
use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{Installation, SupportLevel};
use portal_core::migration::{
    engine::{self, BriefConfig},
    ledger::Ledger,
    types::{MigrationDestination, MigrationKind, VerifyGrade},
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn quote(s: &Path) -> String {
    format!("'{}'", s.display().to_string().replace('\'', "'\"'\"'"))
}

#[test]
#[ignore = "requires sibling BentoKit Core checkout and Bun; isolated real protocol test"]
fn native_destination_preserves_history_retries_continues_and_guards_undo() {
    let core = PathBuf::from(
        std::env::var_os("BENTOKIT_CORE_TEST_ROOT").expect("set BENTOKIT_CORE_TEST_ROOT"),
    );
    let bun = portal_core::util::paths::find_cli(&HostEnv::from_system().path_dirs, "bun")
        .expect("Bun installed");
    let temp = Temp(
        std::env::temp_dir().join(format!("portal-bento-destination-{}", uuid::Uuid::new_v4())),
    );
    let root = temp.0.join("isolated data");
    std::fs::create_dir_all(root.join("agents/nova/workspace")).unwrap();
    std::fs::write(root.join("agents/nova/soul.json"), "{}").unwrap();
    let shim = temp.0.join("bentokit test cli");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nshift\nexec {} {} \"$@\"\n",
            quote(&bun),
            quote(&core.join("src/cli/session-transfer.ts"))
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target: Arc<dyn AgentAdapter> = Arc::new(BentoKitAdapter);
    let inst = Installation {
        cli_path: Some(
            std::env::var("BENTOKIT_CORE_TEST_CLI").unwrap_or_else(|_| shim.display().to_string()),
        ),
        version: Some("source test".into()),
        store_root: root.display().to_string(),
    };
    assert_eq!(
        target.capabilities_for(Some(&inst)).write_native,
        SupportLevel::Full
    );
    assert_eq!(
        target.destination_choices(&inst).unwrap().profiles,
        ["nova"]
    );
    let source: Arc<dyn AgentAdapter> = Arc::new(ClaudeCodeAdapter);
    let source_inst = Installation {
        cli_path: None,
        version: None,
        store_root: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/claude-code/v2.1/store")
            .display()
            .to_string(),
    };
    let locator = SessionLocator {
        native_id: "11111111-1111-1111-1111-111111111111".into(),
        store_path: None,
    };
    let destination = MigrationDestination {
        profile: "nova".into(),
        project_id: None,
        endpoint: Some("ws://127.0.0.1:12345/api/ws".into()),
        operation_id: None,
    };
    let planned = engine::plan_with_destination(
        &source,
        &source_inst,
        &target,
        &inst,
        &locator,
        MigrationKind::Native,
        &BriefConfig::default(),
        Some(destination.clone()),
    )
    .unwrap();
    assert!(
        !root.join("data/agents/nova/conversations.db").exists(),
        "planning must not initialize target storage"
    );
    let ledger = Ledger::new(temp.0.join("ledger"));
    let first = engine::execute(&planned, &target, &inst, &ledger).unwrap();
    assert_eq!(first.verify.as_ref().unwrap().grade, VerifyGrade::Exact);
    let retried = engine::execute(&planned, &target, &inst, &ledger).unwrap();
    assert_eq!(retried.target_native_id, first.target_native_id);
    assert_eq!(ledger.list().unwrap().len(), 1);
    let read = target
        .read_session(
            &inst,
            &SessionLocator {
                native_id: first.target_native_id.clone(),
                store_path: None,
            },
        )
        .unwrap();
    let original = source.read_session(&source_inst, &locator).unwrap();
    assert_eq!(
        serde_json::to_value(&read.timeline).unwrap(),
        serde_json::to_value(&original.timeline).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&read.attachments).unwrap(),
        serde_json::to_value(&original.attachments).unwrap()
    );
    assert!(first
        .resume_command
        .args
        .contains(&"ws://127.0.0.1:12345/api/ws".to_string()));
    let entry = ledger.get(&first.migration_id).unwrap().unwrap();
    assert!(
        engine::undo_with_adapter(&entry, &ledger, false, Some(&target))
            .unwrap()
            .skipped
            .is_empty()
    );
    // A deliberate new plan after undo uses a fresh operation key.
    let again = engine::plan_with_destination(
        &source,
        &source_inst,
        &target,
        &inst,
        &locator,
        MigrationKind::Native,
        &BriefConfig::default(),
        Some(destination),
    )
    .unwrap();
    let next = engine::execute(&again, &target, &inst, &ledger).unwrap();
    assert_ne!(next.target_native_id, first.target_native_id);
    let id = next.target_native_id.split_once('/').unwrap().1;
    let continuation = temp.0.join("continue.ts");
    std::fs::write(&continuation,format!("import {{ConversationStore}} from {};const s=new ConversationStore(process.argv[2]!,'nova');const c=s.get(process.argv[3]!)!;const m=c.messages.find(m=>m.role==='assistant')!;await s.updateMessage(c.id,m.id,'Synthetic edited reply');await s.addMessage(c.id,'user','Synthetic persisted continuation');s.close();",serde_json::to_string(&core.join("src/storage/conversations.ts")).unwrap())).unwrap();
    assert!(std::process::Command::new(&bun)
        .arg(&continuation)
        .arg(&root)
        .arg(id)
        .status()
        .unwrap()
        .success());
    let entry = ledger.get(&next.migration_id).unwrap().unwrap();
    let refused = engine::undo_with_adapter(&entry, &ledger, true, Some(&target)).unwrap();
    assert_eq!(
        refused.skipped.len(),
        1,
        "even force must refuse continued history"
    );
    let continued = target
        .read_session(
            &inst,
            &SessionLocator {
                native_id: next.target_native_id,
                store_path: None,
            },
        )
        .unwrap();
    assert_eq!(continued.timeline.len(), original.timeline.len() + 1);
    assert!(
        continued.timeline.iter().flat_map(|t| &t.blocks).any(
            |b| matches!(b,portal_core::ir::Block::Text{text} if text=="Synthetic edited reply")
        ),
        "Core edits must replace stale saved source content"
    );
    assert_eq!(
        portal_core::migration::verify::compare(&original, &continued).grade,
        VerifyGrade::Failed
    );
    assert!(!ledger.get(&entry.id).unwrap().unwrap().undone);

    // Missing source timestamps are legitimate. Verification must still read
    // actual storage, and detect corruption despite intact provenance copies.
    let mut missing_ts = original.clone();
    missing_ts.timeline[0].timestamp = None;
    let opts = portal_core::migration::types::WriteOptions {
        title: None,
        destination: Some(MigrationDestination {
            profile: "nova".into(),
            project_id: None,
            endpoint: None,
            operation_id: Some("missing-timestamp-test".into()),
        }),
    };
    let written = target.write_session(&inst, &missing_ts, &opts).unwrap();
    let mut round = target
        .read_session(
            &inst,
            &SessionLocator {
                native_id: written.native_id.clone(),
                store_path: None,
            },
        )
        .unwrap();
    assert_eq!(round.timeline[0].timestamp, None);
    assert_eq!(
        portal_core::migration::verify::compare(&missing_ts, &round).grade,
        VerifyGrade::Exact
    );
    let db = rusqlite::Connection::open(&written.primary_path).unwrap();
    let session_id = written.native_id.split_once('/').unwrap().1;
    db.execute("UPDATE messages SET content='CORRUPTED operational row' WHERE conversation_id=?1 AND seq=1",[session_id]).unwrap();
    round = target
        .read_session(
            &inst,
            &SessionLocator {
                native_id: written.native_id,
                store_path: None,
            },
        )
        .unwrap();
    assert_eq!(
        portal_core::migration::verify::compare(&missing_ts, &round).grade,
        VerifyGrade::Failed
    );

    // Selected ChatGPT branch creates exactly its visible native messages;
    // the graph/provenance archive never becomes a phantom user turn.
    let chat_root = temp.0.join("chatgpt-source");
    let summary = portal_adapters::chatgpt_web::stage_export(
        &chat_root,
        include_str!("../fixtures/chatgpt-web/synthetic-conversation.json"),
    )
    .unwrap();
    let chat_source: Arc<dyn AgentAdapter> =
        Arc::new(portal_adapters::chatgpt_web::ChatGptWebAdapter);
    let chat_inst = Installation {
        cli_path: None,
        version: None,
        store_root: chat_root.display().to_string(),
    };
    let chat_plan = engine::plan_with_destination(
        &chat_source,
        &chat_inst,
        &target,
        &inst,
        &SessionLocator {
            native_id: summary.native_id,
            store_path: None,
        },
        MigrationKind::Native,
        &BriefConfig::default(),
        Some(MigrationDestination {
            profile: "nova".into(),
            project_id: None,
            endpoint: None,
            operation_id: None,
        }),
    )
    .unwrap();
    let chat_result = engine::execute(&chat_plan, &target, &inst, &ledger).unwrap();
    let chat_id = chat_result.target_native_id.split_once('/').unwrap().1;
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE conversation_id=?1 AND is_deleted=0",
            [chat_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 2,
        "Only the selected visible user and assistant turns belong in the native conversation"
    );
    let chat_read = target
        .read_session(
            &inst,
            &SessionLocator {
                native_id: chat_result.target_native_id.clone(),
                store_path: None,
            },
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(&chat_read.timeline).unwrap(),
        serde_json::to_value(&chat_plan.session.timeline).unwrap()
    );
    assert!(chat_read
        .losses
        .iter()
        .any(|l| l.code == portal_core::ir::LossCode::AbandonedBranch));
    assert_eq!(chat_read.attachments.len(), 1);
    let check_api = temp.0.join("check-tui.ts");
    std::fs::write(&check_api,format!("import {{ConversationStore}} from {};import {{buildAgentApi}} from {};import {{SessionRegistry}} from {};const s=new ConversationStore(process.argv[2]!,'nova');const p={{id:'nova',conversations:s}};const api=buildAgentApi({{registry:{{list:()=>[p]}},resolveAgent:()=>p,sessionRegistry:new SessionRegistry(),agentChatState:new Map(),projectStore:{{get:()=>null}}}} as any);const r=await api.openTuiSession!({{agentId:'nova',sessionId:process.argv[3]!,mode:'resume'}});if(r.session.messageCount!==2)throw new Error('phantom graph turn');const messages=s.get(process.argv[3]!)!.messages;if(messages[0]?.content!=='Synthetic question\\nPreserve this exact text.'||messages[1]?.content!=='Selected branch fixture.')throw new Error('visible text differs');s.close();",serde_json::to_string(&core.join("src/storage/conversations.ts")).unwrap(),serde_json::to_string(&core.join("src/agent/api-handlers.ts")).unwrap(),serde_json::to_string(&core.join("src/agent/session-registry.ts")).unwrap())).unwrap();
    assert!(std::process::Command::new(&bun)
        .arg(&check_api)
        .arg(&root)
        .arg(chat_id)
        .status()
        .unwrap()
        .success());
}
