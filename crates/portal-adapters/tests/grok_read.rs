use std::path::PathBuf;

use portal_adapters::grok::GrokAdapter;
use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::ir::{Block, Fidelity, LossCode, Role};

fn fixture_store() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/grok/v0.2.93/store")
}

fn installation() -> Installation {
    Installation {
        cli_path: None,
        version: Some("grok 0.2.93".into()),
        store_root: fixture_store().display().to_string(),
    }
}

#[test]
fn enumerates_workspace_and_summary_without_full_transcript_parse() {
    let adapter = GrokAdapter;
    let snapshot = adapter.snapshot(&installation()).expect("snapshot");
    assert_eq!(snapshot.len(), 1);

    let (project, sessions) = &snapshot[0];
    assert_eq!(project.key, "P%3A%5Cdemo%5Capp");
    assert_eq!(project.cwd.as_deref(), Some(r"P:\demo\app"));
    assert_eq!(project.label, "app");
    assert_eq!(sessions.len(), 1);

    let summary = &sessions[0];
    assert_eq!(summary.native_id, "019f0000-0000-7000-8000-000000000001");
    assert_eq!(summary.title.as_deref(), Some("Implement Grok adapter"));
    assert_eq!(summary.model.as_deref(), Some("grok-4.5"));
    assert_eq!(summary.git_branch.as_deref(), Some("feature/grok"));
    assert_eq!(summary.message_count, Some(7));
    assert!(summary.message_count_exact);
}

#[test]
fn reads_transcript_into_canonical_ir() {
    let adapter = GrokAdapter;
    let session = adapter
        .read_session(
            &installation(),
            &SessionLocator {
                native_id: "019f0000-0000-7000-8000-000000000001".into(),
                store_path: None,
            },
        )
        .expect("read session");

    assert_eq!(session.identity.agent_id, "grok-build");
    assert_eq!(session.workspace.cwd, r"P:\demo\app");
    assert_eq!(session.title.as_deref(), Some("Implement Grok adapter"));
    assert_eq!(session.fidelity, Fidelity::Partial);
    assert!(session.validate().is_empty());

    assert_eq!(session.timeline[0].role, Role::System);
    assert!(session.timeline[0].is_meta);
    assert!(
        matches!(session.timeline[1].blocks[0], Block::Text { ref text } if text == "Add a Grok adapter.")
    );
    assert!(matches!(
        session.timeline[2].blocks[0],
        Block::Thinking {
            encrypted: true,
            ..
        }
    ));
    assert!(
        matches!(session.timeline[3].blocks[1], Block::ToolCall { ref call_id, ref name, .. } if call_id == "call-1" && name == "read_file")
    );
    assert!(
        matches!(session.timeline[4].blocks[0], Block::ToolResult { ref call_id, .. } if call_id == "call-1")
    );
    assert!(session
        .losses
        .iter()
        .any(|loss| loss.code == LossCode::EncryptedReasoning));
    assert!(session
        .losses
        .iter()
        .any(|loss| loss.code == LossCode::UnknownRecord));
}

#[test]
fn command_uses_native_resume() {
    let adapter = GrokAdapter;
    let inst = installation();
    let resume = adapter
        .resume_command(
            &inst,
            "019f0000-0000-7000-8000-000000000001",
            r"P:\demo\app",
        )
        .expect("resume");
    assert_eq!(resume.program, "grok");
    assert_eq!(
        resume.args,
        [
            "--cwd",
            r"P:\demo\app",
            "--resume",
            "019f0000-0000-7000-8000-000000000001"
        ]
    );
}

#[test]
fn open_project_launches_interactive_session_without_prompt() {
    let adapter = GrokAdapter;
    let launch = adapter
        .open_project_command(&installation(), r"P:\demo\app")
        .expect("open_project");
    assert_eq!(launch.program, "grok");
    assert_eq!(
        launch.args,
        vec!["--cwd".to_string(), r"P:\demo\app".to_string()]
    );
    assert_eq!(launch.cwd, r"P:\demo\app");
}

#[test]
fn brief_target_launches_interactive_session_with_prompt() {
    let adapter = GrokAdapter;
    assert!(adapter.capabilities().launch_new);
    assert_eq!(
        adapter.capabilities().write_native,
        portal_core::dto::SupportLevel::Partial
    );
    assert!(adapter.accepts_native_from("claude-code"));
    assert!(!adapter.accepts_native_from("codex"));

    let launch = adapter
        .new_session_command(
            &installation(),
            r"P:\demo\app",
            "Read .agent-portal/handoff-abc.md — pick up where it leaves off.",
        )
        .expect("new session");
    assert_eq!(launch.program, "grok");
    assert_eq!(
        launch.args,
        [
            "--cwd",
            r"P:\demo\app",
            "Read .agent-portal/handoff-abc.md — pick up where it leaves off.",
        ]
    );
    assert_eq!(launch.cwd, r"P:\demo\app");
}
