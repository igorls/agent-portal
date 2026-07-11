//! OpenCode reader test: build a synthetic opencode.db and verify it reads
//! into a valid IR — in particular that a single combined `tool` part becomes a
//! paired ToolCall + ToolResult, and that reasoning parts become thinking.

use std::path::PathBuf;

use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::ir::{Block, Role};
use rusqlite::Connection;

use portal_adapters::opencode::OpenCodeAdapter;

fn build_db(path: &PathBuf) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT,
             directory TEXT NOT NULL, title TEXT, version TEXT, model TEXT,
             time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
             time_created INTEGER, time_updated INTEGER, data TEXT NOT NULL);
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
             time_created INTEGER, time_updated INTEGER, data TEXT NOT NULL);",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version, model, time_created, time_updated)
         VALUES ('ses_test', 'proj', NULL, 'demo', 'P:/demo/proj', 'OpenCode demo', '1.17.9',
                 '{\"id\":\"glm-5.2\",\"providerID\":\"ollama-cloud\",\"variant\":\"max\"}', 1000, 2000)",
        [],
    ).unwrap();

    // user message + text part
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, data) VALUES ('m1','ses_test',1000,'{\"role\":\"user\"}')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data)
         VALUES ('p1','m1','ses_test',1000,'{\"type\":\"text\",\"text\":\"Explain the auth module\"}')",
        [],
    ).unwrap();

    // assistant message: reasoning, one combined tool part, text, a step-finish
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, data)
         VALUES ('m2','ses_test',1100,'{\"role\":\"assistant\",\"modelID\":\"glm-5.2\",\"tokens\":{\"input\":10,\"output\":5,\"cache\":{\"read\":0,\"write\":0}}}')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data)
         VALUES ('p2','m2','ses_test',1101,'{\"type\":\"reasoning\",\"text\":\"weighing the auth options\"}')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data)
         VALUES ('p3','m2','ses_test',1102,'{\"type\":\"tool\",\"tool\":\"read\",\"callID\":\"call_x\",\"state\":{\"status\":\"completed\",\"input\":{\"file\":\"auth.ts\"},\"output\":\"login handler\"}}')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data)
         VALUES ('p4','m2','ses_test',1103,'{\"type\":\"text\",\"text\":\"The auth module handles login.\"}')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data)
         VALUES ('p5','m2','ses_test',1104,'{\"type\":\"step-finish\",\"reason\":\"stop\"}')",
        [],
    )
    .unwrap();
    // Connection drops here (clean close, no WAL) so the adapter can open immutable.
}

#[test]
fn reads_opencode_session_with_paired_tools_and_reasoning() {
    let tmp = std::env::temp_dir().join(format!("portal-oc-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let db = tmp.join("opencode.db");
    let _ = std::fs::remove_file(&db);
    build_db(&db);

    let adapter = OpenCodeAdapter;
    let inst = Installation {
        cli_path: None,
        version: Some("1.17.9".into()),
        store_root: db.display().to_string(),
    };

    // Enumeration.
    let snapshot = adapter.snapshot(&inst).expect("snapshot");
    let sessions: Vec<_> = snapshot.into_iter().flat_map(|(_, s)| s).collect();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("OpenCode demo"));
    assert_eq!(sessions[0].message_count, Some(2));
    assert_eq!(sessions[0].model.as_deref(), Some("glm-5.2"));

    // Full read.
    let session = adapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: "ses_test".into(),
                store_path: None,
            },
        )
        .expect("read");

    assert!(session.validate().is_empty(), "{:?}", session.validate());
    assert_eq!(session.workspace.cwd, "P:/demo/proj");
    assert_eq!(session.title.as_deref(), Some("OpenCode demo"));

    // Turn 1: user text. Turn 2: assistant with thinking, a paired tool
    // call+result (from ONE part), text, and a preserved meta block.
    assert_eq!(session.timeline.len(), 2);
    assert_eq!(session.timeline[0].role, Role::User);
    let a = &session.timeline[1];
    assert_eq!(a.role, Role::Assistant);
    assert_eq!(a.model.as_deref(), Some("glm-5.2"));

    let mut thinking = 0;
    let mut call = None;
    let mut result = None;
    let mut text = 0;
    let mut meta = 0;
    for b in &a.blocks {
        match b {
            Block::Thinking { text: t, .. } => {
                thinking += 1;
                assert!(t.contains("weighing the auth options"));
            }
            Block::ToolCall { call_id, name, .. } => call = Some((call_id.clone(), name.clone())),
            Block::ToolResult {
                call_id, is_error, ..
            } => result = Some((call_id.clone(), *is_error)),
            Block::Text { .. } => text += 1,
            Block::Meta { .. } => meta += 1,
            _ => {}
        }
    }
    assert_eq!(thinking, 1);
    assert_eq!(text, 1);
    assert_eq!(meta, 1, "step-finish preserved as meta");
    let (call_id, name) = call.expect("tool call");
    let (result_id, is_error) = result.expect("tool result");
    assert_eq!(name, "read");
    assert_eq!(call_id, "call_x");
    assert_eq!(
        result_id, "call_x",
        "result pairs with the call from the same part"
    );
    assert!(!is_error);
    assert!(session.unanswered_tool_calls().is_empty());

    std::fs::remove_dir_all(&tmp).ok();
}
