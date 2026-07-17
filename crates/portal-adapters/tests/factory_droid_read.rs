use std::path::PathBuf;

use portal_adapters::factory_droid::FactoryDroidAdapter;
use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::ir::{Block, Fidelity, Role};

fn fixture_store() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/factory-droid/v0.162/store")
}

fn installation() -> Installation {
    Installation {
        cli_path: None,
        version: Some("0.162.1".into()),
        store_root: fixture_store().display().to_string(),
    }
}

#[test]
fn enumerates_project_and_session_from_session_start() {
    let adapter = FactoryDroidAdapter;
    let snapshot = adapter.snapshot(&installation()).expect("snapshot");
    assert_eq!(snapshot.len(), 1);

    let (project, sessions) = &snapshot[0];
    assert_eq!(project.key, "-P-demo-app");
    assert_eq!(project.cwd.as_deref(), Some(r"P:\demo\app"));
    assert_eq!(project.label, "app");
    assert_eq!(sessions.len(), 1);

    let summary = &sessions[0];
    assert_eq!(summary.native_id, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    assert_eq!(
        summary.title.as_deref(),
        Some("Wire Factory Droid adapter")
    );
    assert_eq!(summary.model.as_deref(), Some("custom:glm-5.2:cloud-0"));
    assert_eq!(summary.message_count, Some(4));
    assert!(summary.message_count_exact);
}

#[test]
fn reads_transcript_into_canonical_ir() {
    let adapter = FactoryDroidAdapter;
    let session = adapter
        .read_session(
            &installation(),
            &SessionLocator {
                native_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
                store_path: None,
            },
        )
        .expect("read session");

    assert_eq!(session.identity.agent_id, "factory-droid");
    assert_eq!(session.workspace.cwd, r"P:\demo\app");
    assert_eq!(
        session.title.as_deref(),
        Some("Wire Factory Droid adapter")
    );
    assert!(session.validate().is_empty(), "{:?}", session.validate());
    assert_eq!(session.fidelity, Fidelity::Full);

    // session_start meta + user + assistant(tool) + tool + assistant
    let content: Vec<_> = session
        .timeline
        .iter()
        .filter(|t| !t.is_meta)
        .collect();
    assert_eq!(content.len(), 4);
    assert_eq!(content[0].role, Role::User);
    assert!(
        matches!(content[0].blocks[0], Block::Text { ref text } if text.contains("Factory Droid"))
    );

    let assistant = content[1];
    assert_eq!(assistant.role, Role::Assistant);
    assert!(matches!(assistant.blocks[0], Block::Thinking { .. }));
    assert!(matches!(
        assistant.blocks[2],
        Block::ToolCall {
            ref call_id,
            ref name,
            ..
        } if call_id == "call_read1" && name == "Read"
    ));

    assert_eq!(content[2].role, Role::Tool);
    assert!(matches!(
        content[2].blocks[0],
        Block::ToolResult { ref call_id, .. } if call_id == "call_read1"
    ));

    assert!(session.usage.known);
    assert_eq!(session.usage.input_tokens, 1200);
    assert_eq!(session.usage.output_tokens, 340);

    // todo_state preserved as meta, not dropped silently.
    assert!(session
        .timeline
        .iter()
        .any(|t| t.is_meta
            && t.blocks
                .iter()
                .any(|b| matches!(b, Block::Meta { source_kind, .. } if source_kind == "todo_state"))));
}

#[test]
fn resume_and_new_commands() {
    let adapter = FactoryDroidAdapter;
    let inst = installation();
    let resume = adapter
        .resume_command(&inst, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", r"P:\demo\app")
        .expect("resume");
    assert_eq!(resume.program, "droid");
    assert_eq!(
        resume.args,
        vec![
            "--cwd".to_string(),
            r"P:\demo\app".to_string(),
            "--resume".to_string(),
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
        ]
    );

    let fresh = adapter
        .new_session_command(&inst, r"P:\demo\app", "continue the work")
        .expect("new");
    assert_eq!(fresh.program, "droid");
    assert!(fresh.args.iter().any(|a| a == "continue the work"));
}
