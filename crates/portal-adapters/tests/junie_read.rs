use std::path::PathBuf;

use portal_adapters::junie::JunieAdapter;
use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::ir::{Block, Fidelity, Role};

fn fixture_store() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/junie/v26.7/store")
}

fn installation() -> Installation {
    Installation {
        cli_path: None,
        version: Some("Junie version: 26.7.13 (2285.5)".into()),
        store_root: fixture_store().display().to_string(),
    }
}

#[test]
fn enumerates_from_index_and_session_dir() {
    let adapter = JunieAdapter;
    let snapshot = adapter.snapshot(&installation()).expect("snapshot");
    assert_eq!(snapshot.len(), 1);

    let (project, sessions) = &snapshot[0];
    assert_eq!(project.cwd.as_deref(), Some(r"P:\demo\app"));
    assert_eq!(project.label, "app");
    assert_eq!(sessions.len(), 1);

    let summary = &sessions[0];
    assert_eq!(summary.native_id, "session-260702-100000-demo");
    assert_eq!(
        summary.title.as_deref(),
        Some("Review the Junie adapter design")
    );
    assert_eq!(summary.model.as_deref(), Some("glm-5.2:cloud"));
    assert_eq!(summary.message_count, Some(1));
    assert!(summary.message_count_exact);
}

#[test]
fn reads_event_stream_into_canonical_ir() {
    let adapter = JunieAdapter;
    let session = adapter
        .read_session(
            &installation(),
            &SessionLocator {
                native_id: "session-260702-100000-demo".into(),
                store_path: None,
            },
        )
        .expect("read session");

    assert_eq!(session.identity.agent_id, "junie");
    assert_eq!(session.workspace.cwd, r"P:\demo\app");
    assert_eq!(
        session.title.as_deref(),
        Some("Review the Junie adapter design")
    );
    assert_eq!(session.fidelity, Fidelity::Partial);
    assert!(session.validate().is_empty(), "{:?}", session.validate());

    let content: Vec<_> = session.timeline.iter().filter(|t| !t.is_meta).collect();
    assert!(
        content.len() >= 4,
        "expected user+thought+tools+result, got {}",
        content.len()
    );

    assert_eq!(content[0].role, Role::User);
    assert!(matches!(
        content[0].blocks[0],
        Block::Text { ref text } if text.contains("Junie adapter")
    ));

    assert!(content.iter().any(|t| {
        t.role == Role::Assistant && t.blocks.iter().any(|b| matches!(b, Block::Thinking { .. }))
    }));

    assert!(content.iter().any(|t| {
        t.blocks.iter().any(|b| {
            matches!(
                b,
                Block::ToolCall {
                    ref name,
                    ..
                } if name == "Skill" || name == "terminal"
            )
        })
    }));

    assert!(content.iter().any(|t| {
        t.blocks.iter().any(|b| {
            matches!(
                b,
                Block::Text { ref text } if text.contains("~/.junie/sessions")
            )
        })
    }));

    assert!(session.usage.known);
    assert_eq!(session.usage.input_tokens, 1500);
    assert_eq!(session.usage.output_tokens, 130);
}

#[test]
fn resume_and_new_commands() {
    let adapter = JunieAdapter;
    let inst = installation();
    let resume = adapter
        .resume_command(&inst, "session-260702-100000-demo", r"P:\demo\app")
        .expect("resume");
    assert_eq!(resume.program, "junie");
    assert_eq!(
        resume.args,
        vec![
            "--session-id".to_string(),
            "session-260702-100000-demo".to_string(),
            "--resume".to_string(),
            "--project".to_string(),
            r"P:\demo\app".to_string(),
        ]
    );

    let fresh = adapter
        .new_session_command(&inst, r"P:\demo\app", "continue the work")
        .expect("new");
    assert_eq!(fresh.program, "junie");
    assert!(fresh.args.iter().any(|a| a == "continue the work"));
    assert!(fresh.args.iter().any(|a| a == "--prompt"));
}
