//! Fixture-driven reader tests: real store layouts -> CanonicalSession.

use std::path::PathBuf;

use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::ir::{Block, Fidelity, LossCode, Role};

use portal_adapters::claude_code::ClaudeCodeAdapter;
use portal_adapters::codex::CodexAdapter;

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(rel)
}

fn install(store_root: PathBuf) -> Installation {
    Installation {
        cli_path: None,
        version: None,
        store_root: store_root.display().to_string(),
    }
}

#[test]
fn claude_fixture_reads_to_valid_ir() {
    let adapter = ClaudeCodeAdapter;
    let inst = install(fixture("claude-code/v2.1/store"));
    let session = adapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: "11111111-1111-1111-1111-111111111111".into(),
                store_path: None,
            },
        )
        .expect("read fixture");

    assert!(session.validate().is_empty(), "{:?}", session.validate());
    assert_eq!(session.title.as_deref(), Some("Demo app REST API"));
    assert_eq!(session.workspace.cwd, "P:\\demo-app");
    assert_eq!(session.workspace.cwd_normalized, "p:/demo-app");
    assert_eq!(session.workspace.git_branch.as_deref(), Some("main"));
    assert_eq!(session.identity.agent_version.as_deref(), Some("2.1.206"));
    assert_eq!(session.fidelity, Fidelity::Full);

    // Active chain: user, assistant(tool), user(result), assistant, system. The
    // abandoned fork record must NOT be in the timeline.
    assert_eq!(session.timeline.len(), 5);
    assert!(session.timeline.iter().all(|t| t
        .blocks
        .iter()
        .all(|b| !matches!(b, Block::Text { text } if text.contains("abandoned")))));

    // Roles and block mapping.
    assert_eq!(session.timeline[0].role, Role::User);
    let assistant = &session.timeline[1];
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(assistant.model.as_deref(), Some("claude-fable-5"));
    assert!(matches!(assistant.blocks[0], Block::Thinking { .. }));
    assert!(matches!(assistant.blocks[2], Block::ToolCall { .. }));
    assert!(matches!(
        session.timeline[2].blocks[0],
        Block::ToolResult { .. }
    ));

    // Usage summed from assistant records.
    assert!(session.usage.known);
    assert_eq!(session.usage.input_tokens, 320);
    assert_eq!(session.usage.output_tokens, 75);

    // Losses: abandoned branch + unknown record type.
    assert!(session
        .losses
        .iter()
        .any(|l| l.code == LossCode::AbandonedBranch));
    assert!(session
        .losses
        .iter()
        .any(|l| l.code == LossCode::UnknownRecord && l.detail.contains("wormhole-experimental")));

    // No unanswered tool calls in this fixture.
    assert!(session.unanswered_tool_calls().is_empty());
}

#[test]
fn codex_fixture_reads_to_valid_ir() {
    let adapter = CodexAdapter;
    let inst = install(fixture("codex/v0.143/store"));
    let session = adapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: "019f0000-0000-7000-8000-000000000001".into(),
                store_path: None,
            },
        )
        .expect("read fixture");

    assert!(session.validate().is_empty(), "{:?}", session.validate());
    assert_eq!(session.workspace.cwd, "P:\\demo-app");
    assert_eq!(session.identity.agent_version.as_deref(), Some("0.143.0"));
    assert_eq!(session.title.as_deref(), Some("Fix the failing tests"));

    // user msg, reasoning, function_call, function_call_output, assistant msg.
    assert_eq!(session.timeline.len(), 5);
    assert_eq!(session.timeline[0].role, Role::User);
    assert!(matches!(
        &session.timeline[1].blocks[0],
        Block::Thinking {
            encrypted: true,
            ..
        }
    ));
    let Block::ToolCall {
        call_id,
        name,
        arguments,
    } = &session.timeline[2].blocks[0]
    else {
        panic!("expected tool call");
    };
    assert_eq!(call_id, "call_1");
    assert_eq!(name, "shell_command");
    assert_eq!(arguments["command"], "pytest -q");
    let Block::ToolResult {
        call_id, is_error, ..
    } = &session.timeline[3].blocks[0]
    else {
        panic!("expected tool result");
    };
    assert_eq!(call_id, "call_1");
    assert!(!is_error);

    // Assistant model came from turn_context.
    let assistant = session
        .timeline
        .iter()
        .find(|t| {
            t.role == Role::Assistant && t.blocks.iter().any(|b| matches!(b, Block::Text { .. }))
        })
        .unwrap();
    assert_eq!(assistant.model.as_deref(), Some("gpt-5.5"));

    // Usage from token_count event.
    assert!(session.usage.known);
    assert_eq!(session.usage.input_tokens, 5000);
    assert_eq!(session.usage.output_tokens, 250);

    // Encrypted reasoning surfaced as a loss.
    assert!(session
        .losses
        .iter()
        .any(|l| l.code == LossCode::EncryptedReasoning));
}
