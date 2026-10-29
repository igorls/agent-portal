use portal_adapters::chatgpt_web::{stage_export, ChatGptWebAdapter};
use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::ir::{Block, LossCode, Role};
use serde_json::{json, Value};
use std::path::PathBuf;
const FIXTURE: &str = include_str!("../fixtures/chatgpt-web/synthetic-conversation.json");
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("portal-chatgpt-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn selected_branch_exact_text_ids_metadata_and_media_references_survive() {
    let temp = Temp::new();
    let summary = stage_export(&temp.0, FIXTURE).unwrap();
    let inst = Installation {
        cli_path: None,
        version: None,
        store_root: temp.0.display().to_string(),
    };
    let read = ChatGptWebAdapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: summary.native_id.clone(),
                store_path: None,
            },
        )
        .unwrap();
    assert_eq!(summary.message_count, Some(2));
    assert_eq!(read.timeline[0].id, "user-original");
    assert_eq!(read.timeline[0].role, Role::User);
    assert!(
        matches!(&read.timeline[0].blocks[0],Block::Text{text} if text=="Synthetic question\nPreserve this exact text.")
    );
    assert_eq!(read.timeline[1].id, "assistant-original");
    assert_eq!(read.timeline[1].model.as_deref(), Some("synthetic-model"));
    assert_eq!(
        read.timeline[1].raw.as_ref().unwrap()["node"]["message"]["metadata"]["content_references"]
            [0]["type"],
        "synthetic-citation"
    );
    assert!(read
        .losses
        .iter()
        .any(|l| l.code == LossCode::AbandonedBranch));
    assert!(read
        .losses
        .iter()
        .any(|l| l.detail.contains("attachment bytes")));
    assert_eq!(read.attachments.len(), 1);
    assert!(read.validate().is_empty());
    assert_eq!(
        ChatGptWebAdapter
            .list_sessions(&inst, &ChatGptWebAdapter.list_projects(&inst).unwrap()[0])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        stage_export(&temp.0, FIXTURE).unwrap().native_id,
        summary.native_id
    );
}

#[test]
fn malformed_branches_cycles_and_account_archives_are_rejected_without_staging() {
    for raw in [
        "[]".to_string(),
        format!("[{FIXTURE},{FIXTURE}]"),
        json!({"id":"bad","mapping":{},"current_node":"absent"}).to_string(),
    ] {
        let temp = Temp::new();
        assert!(stage_export(&temp.0, &raw).is_err());
        assert!(!temp.0.exists());
    }
    let mut cycle: Value = serde_json::from_str(FIXTURE).unwrap();
    cycle["mapping"]["root"]["parent"] = json!("answer-current");
    assert!(stage_export(&Temp::new().0, &cycle.to_string()).is_err());
}

#[test]
fn changed_staged_content_and_path_traversal_are_rejected() {
    let temp = Temp::new();
    let summary = stage_export(&temp.0, FIXTURE).unwrap();
    let inst = Installation {
        cli_path: None,
        version: None,
        store_root: temp.0.display().to_string(),
    };
    assert!(ChatGptWebAdapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: "../secret".into(),
                store_path: None
            }
        )
        .is_err());
    std::fs::write(&summary.store_path, "{}").unwrap();
    assert!(ChatGptWebAdapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: summary.native_id,
                store_path: None
            }
        )
        .is_err());
}

#[test]
fn normal_export_arrays_offer_choices_and_stage_only_the_selected_conversation() {
    let first: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut second = first.clone();
    second["id"] = json!("second-chat");
    second["title"] = json!("Second fixture");
    let export = json!([first, second]).to_string();
    let temp = Temp::new();
    let choices = portal_adapters::chatgpt_web::export_choices(&export).unwrap();
    assert_eq!(choices.len(), 2);
    assert!(!temp.0.exists());
    let selected =
        portal_adapters::chatgpt_web::stage_selected_export(&temp.0, &export, Some("second-chat"))
            .unwrap();
    assert!(selected.native_id.starts_with("second-chat/"));
    assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), 1);
}

#[test]
fn visible_dom_capture_keeps_actual_ids_roles_text_and_declares_capture_provenance() {
    let temp = Temp::new();
    let raw=json!({"capture_kind":"chatgpt-visible-dom-v1","id":"synthetic-visible-capture","title":"Fabricated DOM fixture","url":"https://chatgpt.com/c/synthetic-visible-capture","branch_scope":"visible_selected_thread_only","messages":[
  {"id":"real-looking-fixture-user-id","role":"user","text":"Exact visible\nuser text","timestamp":null,"html":"<p>Exact visible user text</p>"},
  {"id":"real-looking-fixture-assistant-id","role":"assistant","text":"Exact assistant reply.","timestamp":null,"html":"<p>Exact assistant reply.</p>"}
 ]}).to_string();
    let summary = stage_export(&temp.0, &raw).unwrap();
    let inst = Installation {
        cli_path: None,
        version: None,
        store_root: temp.0.display().to_string(),
    };
    let session = ChatGptWebAdapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id: summary.native_id,
                store_path: None,
            },
        )
        .unwrap();
    assert_eq!(session.timeline[0].id, "real-looking-fixture-user-id");
    assert_eq!(session.timeline[0].timestamp, None);
    assert!(
        matches!(&session.timeline[0].blocks[0],Block::Text{text} if text=="Exact visible\nuser text")
    );
    let Block::Meta { raw, .. } = &session.timeline.last().unwrap().blocks[0] else {
        panic!("source capture provenance");
    };
    assert_eq!(
        raw["source_capture"]["capture_kind"],
        "chatgpt-visible-dom-v1"
    );
}
