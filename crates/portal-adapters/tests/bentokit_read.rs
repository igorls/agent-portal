use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use portal_adapters::bentokit::{BentoKitAdapter, ID};
use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{Installation, SupportLevel};
use portal_core::ir::{Block, Fidelity, LossCode};
use portal_core::migration::engine::{self, BriefConfig};
use portal_core::migration::ledger::Ledger;
use portal_core::migration::types::{MigrationKind, VerifyGrade};
use portal_core::registry::AgentRegistry;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const TS: &str = "2026-10-28T12:00:00.000Z";

struct Store(PathBuf);

impl Store {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("portal-bentokit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn db_path(&self, agent: &str) -> PathBuf {
        self.0
            .join("data/agents")
            .join(agent)
            .join("conversations.db")
    }

    fn db(&self, agent: &str) -> Connection {
        let path = self.db_path(agent);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(include_str!("../fixtures/bentokit/v0.3.2/schema.sql"))
            .unwrap();
        conn
    }

    fn installation(&self) -> Installation {
        Installation {
            cli_path: None,
            version: Some("0.3.2".into()),
            store_root: self.0.display().to_string(),
        }
    }

    fn projects(&self) {
        let conn = Connection::open(self.0.join("data/projects.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE projects (id TEXT, name TEXT, source_type TEXT, uri TEXT);
            INSERT INTO projects VALUES ('local', 'Demo app', 'local', 'file:///P:/demo%20app');
            INSERT INTO projects VALUES ('remote', 'Remote app', 'ssh', 'ssh://host/app');",
        )
        .unwrap();
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn conversation(conn: &Connection, agent: &str, id: &str, project: Option<&str>) {
    conn.execute(
        "INSERT INTO conversations (id,agent_id,title,project_id,created_at,updated_at)
        VALUES (?1,?2,'Synthetic demo',?3,?4,?4)",
        params![id, agent, project, TS],
    )
    .unwrap();
}

fn message(conn: &Connection, id: &str, seq: u32, role: &str, text: &str, meta: Value) {
    conn.execute(
        "INSERT INTO messages (id,conversation_id,role,content,timestamp,metadata,seq)
        VALUES (?1,'session',?2,?3,?4,?5,?6)",
        params![id, role, text, TS, meta.to_string(), seq],
    )
    .unwrap();
}

fn read(store: &Store) -> portal_core::ir::CanonicalSession {
    BentoKitAdapter
        .read_session(
            &store.installation(),
            &SessionLocator {
                native_id: "nova/session".into(),
                store_path: None,
            },
        )
        .unwrap()
}

fn seeded() -> Store {
    let store = Store::new();
    let conn = store.db("nova");
    conversation(&conn, "nova", "session", Some("local"));
    // Insertion order differs from sequence order at the same timestamp.
    message(
        &conn,
        "reply",
        2,
        "assistant",
        "Tests pass.",
        json!({
            "reasoning":"Inspect before answering.",
            "modelInfo":{"providerId":"ollama","modelName":"demo-model"},
            "tokenUsage":{"input":100,"output":20},
            "toolCalls":[{"name":"shell","args":{"command":"cargo test"},"result":"success","output":"2 tests pass"}],
            "segments":[{"kind":"text","text":"Checking tests."},{"kind":"tool","index":0},{"kind":"text","text":"Tests pass.","final":true}]
        }),
    );
    message(&conn, "question", 1, "user", "Run the tests", Value::Null);
    store.projects();
    store
}

#[test]
fn detection_overrides_and_inbound_capability_gates() {
    let store = seeded();
    let env = HostEnv {
        home: store.0.join("absent-home"),
        path_dirs: vec![],
        store_overrides: HashMap::from([
            (ID.into(), store.0.clone()),
            ("codex".into(), store.0.clone()),
        ]),
    };
    assert_eq!(
        BentoKitAdapter.detect(&env).unwrap().store_root,
        store.installation().store_root
    );
    assert!(BentoKitAdapter
        .detect(&HostEnv {
            store_overrides: HashMap::new(),
            ..env.clone()
        })
        .is_none());
    let registry = AgentRegistry::new(vec![
        Arc::new(BentoKitAdapter),
        Arc::new(portal_adapters::codex::CodexAdapter),
    ]);
    let pairs = registry.feasibility(&env);
    let outbound = pairs.iter().find(|p| p.source == ID).unwrap();
    assert!(outbound.native && outbound.brief);
    let inbound = pairs.iter().find(|p| p.target == ID).unwrap();
    assert!(!inbound.native && !inbound.brief);
    assert_eq!(
        BentoKitAdapter.capabilities().write_native,
        SupportLevel::None
    );
}

#[test]
fn snapshot_qualifies_ids_resolves_projects_and_excludes_deleted_records() {
    let store = seeded();
    let conn = Connection::open(store.db_path("nova")).unwrap();
    conversation(&conn, "nova", "gone", Some("local"));
    conn.execute(
        "UPDATE conversations SET is_deleted = 1 WHERE id = 'gone'",
        [],
    )
    .unwrap();
    message(&conn, "deleted", 0, "user", "Deleted text", Value::Null);
    conn.execute(
        "UPDATE messages SET is_deleted = 1 WHERE id = 'deleted'",
        [],
    )
    .unwrap();
    conversation(&conn, "nova", "remote-session", Some("remote"));
    conversation(&conn, "nova", "missing-project", Some("absent"));
    conversation(&conn, "other-owner", "wrong-owner", None);
    let other = store.db("other");
    conversation(&other, "other", "session", None);
    let snapshot = BentoKitAdapter.snapshot(&store.installation()).unwrap();
    let summaries: Vec<_> = snapshot.iter().flat_map(|(_, sessions)| sessions).collect();
    assert_eq!(summaries.len(), 4);
    let demo = summaries
        .iter()
        .find(|s| s.native_id == "nova/session")
        .unwrap();
    assert_eq!(demo.cwd.as_deref(), Some("P:/demo app"));
    assert_eq!(demo.model.as_deref(), Some("demo-model"));
    assert_eq!(demo.message_count, Some(2));
    assert!(demo.message_count_exact);
    assert!(summaries.iter().any(|s| s.native_id == "other/session"));
    assert!(summaries
        .iter()
        .filter(|s| s.native_id.contains("remote") || s.native_id.contains("missing"))
        .all(|s| s.cwd.is_none()));
    assert_eq!(read(&store).timeline.len(), 2);
}

#[test]
fn reads_ordered_tools_reasoning_and_usage_without_duplicate_final_text() {
    let store = seeded();
    let session = read(&store);
    assert!(session.validate().is_empty());
    assert_eq!(session.timeline[0].id, "question");
    assert_eq!(session.fidelity, Fidelity::Partial);
    assert_eq!(session.usage.input_tokens, 100);
    assert_eq!(session.usage.output_tokens, 20);
    let reply = &session.timeline[1];
    assert_eq!(reply.model.as_deref(), Some("demo-model"));
    assert_eq!(reply.blocks.len(), 5);
    assert!(matches!(reply.blocks[0], Block::Thinking { .. }));
    assert!(matches!(reply.blocks[1], Block::Text { ref text } if text == "Checking tests."));
    assert!(matches!(reply.blocks[2], Block::ToolCall { .. }));
    assert!(matches!(
        reply.blocks[3],
        Block::ToolResult {
            is_error: false,
            ..
        }
    ));
    assert!(matches!(reply.blocks[4], Block::Text { ref text } if text == "Tests pass."));
}

#[test]
fn edits_failures_interrupted_tools_and_media_have_explicit_fidelity() {
    let store = seeded();
    let conn = Connection::open(store.db_path("nova")).unwrap();
    message(
        &conn,
        "edited",
        3,
        "assistant",
        "Corrected reply",
        json!({
            "toolCalls":[{"name":"shell","args":{},"result":"error","error":"command failed"},{"name":"pending","args":{}}],
            "segments":[{"kind":"text","text":"Stale reply"}], "images":[{"url":"local-image"}]
        }),
    );
    let session = read(&store);
    assert!(session.validate().is_empty());
    assert_eq!(session.unanswered_tool_calls(), ["edited:tool:1"]);
    let edited = session.timeline.last().unwrap();
    assert!(edited
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Text { text } if text == "Corrected reply")));
    assert!(!edited
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Text { text } if text == "Stale reply")));
    assert!(edited.blocks.iter().any(|b| matches!(b, Block::ToolResult { is_error: true, output, .. } if output == "command failed")));
    assert!(session
        .losses
        .iter()
        .any(|l| l.code == LossCode::ContentSkipped && l.detail.contains("images")));
}

#[test]
fn wal_commits_are_visible_and_readers_do_not_write_the_source() {
    let store = Store::new();
    let writer = store.db("nova");
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    conversation(&writer, "nova", "session", None);
    message(
        &writer,
        "question",
        1,
        "user",
        "Uncheckpointed WAL data",
        Value::Null,
    );
    let db_before = std::fs::read(store.db_path("nova")).unwrap();
    let wal_path = store.db_path("nova").with_file_name("conversations.db-wal");
    let wal_before = std::fs::read(&wal_path).unwrap();
    assert_eq!(
        BentoKitAdapter.snapshot(&store.installation()).unwrap()[0].1[0].message_count,
        Some(1)
    );
    assert_eq!(read(&store).timeline.len(), 1);
    assert_eq!(std::fs::read(store.db_path("nova")).unwrap(), db_before);
    assert_eq!(std::fs::read(wal_path).unwrap(), wal_before);
}

#[test]
fn locators_cannot_escape_the_data_root_or_read_deleted_sessions() {
    let store = seeded();
    for native_id in [
        "../session",
        "nova/../../secret",
        "nova\\escape/session",
        "unqualified",
    ] {
        assert!(BentoKitAdapter
            .read_session(
                &store.installation(),
                &SessionLocator {
                    native_id: native_id.into(),
                    store_path: None
                }
            )
            .is_err());
    }
    // An untrusted hint cannot redirect a valid id to another database.
    let session = BentoKitAdapter
        .read_session(
            &store.installation(),
            &SessionLocator {
                native_id: "nova/session".into(),
                store_path: Some(PathBuf::from("/untrusted/conversations.db")),
            },
        )
        .unwrap();
    assert_eq!(
        session.identity.store_path,
        store
            .db_path("nova")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    let conn = Connection::open(store.db_path("nova")).unwrap();
    conn.execute("UPDATE conversations SET is_deleted=1", [])
        .unwrap();
    assert!(BentoKitAdapter
        .read_session(
            &store.installation(),
            &SessionLocator {
                native_id: "nova/session".into(),
                store_path: None
            }
        )
        .is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_profiles_outside_the_store_are_rejected() {
    let external = seeded();
    let store = Store::new();
    std::fs::create_dir_all(store.0.join("data/agents")).unwrap();
    std::os::unix::fs::symlink(
        external.0.join("data/agents/nova"),
        store.0.join("data/agents/nova"),
    )
    .unwrap();
    assert!(BentoKitAdapter.snapshot(&store.installation()).is_err());
}

#[test]
fn resume_uses_bentokit_tui_and_the_profile_qualified_id() {
    let store = seeded();
    let mut inst = store.installation();
    inst.cli_path = Some("/custom bin/bentokit".into());
    let cmd = BentoKitAdapter
        .resume_command(&inst, "nova/session", "P:/demo app")
        .unwrap();
    assert_eq!(cmd.program, "/custom bin/bentokit");
    assert_eq!(
        cmd.args,
        [
            "tui",
            "--agent",
            "nova",
            "--session",
            "session",
            "--cwd",
            "P:/demo app"
        ]
    );
}

fn target_install(root: &Path) -> Installation {
    Installation {
        cli_path: None,
        version: None,
        store_root: root.display().to_string(),
    }
}

#[test]
fn migrates_natively_to_claude_and_codex_with_verified_content_and_undo() {
    let store = seeded();
    let before = std::fs::read(store.db_path("nova")).unwrap();
    let source: Arc<dyn AgentAdapter> = Arc::new(BentoKitAdapter);
    let targets: Vec<(Arc<dyn AgentAdapter>, &str)> = vec![
        (
            Arc::new(portal_adapters::claude_code::ClaudeCodeAdapter),
            "claude/projects",
        ),
        (
            Arc::new(portal_adapters::codex::CodexAdapter),
            "codex/sessions",
        ),
    ];
    for (target, dir) in targets {
        let inst = target_install(&store.0.join(dir));
        let planned = engine::plan(
            &source,
            &store.installation(),
            &target,
            &inst,
            &SessionLocator {
                native_id: "nova/session".into(),
                store_path: None,
            },
            MigrationKind::Native,
            &BriefConfig::default(),
        )
        .unwrap();
        let ledger = Ledger::new(store.0.join(format!("ledger-{}", target.id())));
        let result = engine::execute(&planned, &target, &inst, &ledger).unwrap();
        assert_eq!(
            result.verify.as_ref().unwrap().grade,
            VerifyGrade::Exact,
            "{:?}",
            result.verify
        );
        assert!(Path::new(&result.target_path).is_file());
        let entry = ledger.get(&result.migration_id).unwrap().unwrap();
        let undo = engine::undo(&entry, &ledger, false).unwrap();
        assert!(undo.skipped.is_empty());
        assert!(!Path::new(&result.target_path).exists());
    }
    assert_eq!(std::fs::read(store.db_path("nova")).unwrap(), before);
}

#[test]
fn brief_exports_use_distinct_safe_filenames_for_profile_qualified_ids() {
    let store = Store::new();
    let conn = store.db("nova");
    for id in ["session", "session-two"] {
        conversation(&conn, "nova", id, None);
        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, content, timestamp, seq)
            VALUES (?1, ?1, 'user', 'Continue the implementation', ?2, 1)",
            [id, TS],
        )
        .unwrap();
    }
    let cwd = store.0.join("agents/nova/workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let source: Arc<dyn AgentAdapter> = Arc::new(BentoKitAdapter);
    let target: Arc<dyn AgentAdapter> = Arc::new(portal_adapters::codex::CodexAdapter);
    let inst = target_install(&store.0.join("codex/sessions"));
    let ledger = Ledger::new(store.0.join("ledger"));
    let mut paths = Vec::new();
    for id in ["session", "session-two"] {
        let plan = engine::plan(
            &source,
            &store.installation(),
            &target,
            &inst,
            &SessionLocator {
                native_id: format!("nova/{id}"),
                store_path: None,
            },
            MigrationKind::Brief,
            &BriefConfig::default(),
        )
        .unwrap();
        let result = engine::execute(&plan, &target, &inst, &ledger).unwrap();
        let path = PathBuf::from(&result.target_path);
        assert_eq!(path.parent(), Some(cwd.join(".agent-portal").as_path()));
        assert!(path.is_file());
        paths.push(path);
    }
    assert_ne!(paths[0], paths[1]);
}
