//! Import one selected ChatGPT export/capture into an explicit BentoKit profile.
//! No model calls. Usage: bentokit_import <json> <data-dir> <profile> <core-cli> [conversation-id]
use portal_core::adapter::{AgentAdapter, SessionLocator};
use portal_core::dto::Installation;
use portal_core::migration::{
    engine::{self, BriefConfig},
    ledger::Ledger,
    types::{MigrationDestination, MigrationKind},
};
use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        return Err(
            "usage: bentokit_import <json> <data-dir> <profile> <core-cli> [conversation-id]"
                .into(),
        );
    }
    let root = PathBuf::from(&args[1]);
    let raw = std::fs::read_to_string(&args[0])?;
    let source_root = root.join("portal-selected-source");
    let summary = portal_adapters::chatgpt_web::stage_selected_export(
        &source_root,
        &raw,
        args.get(4).map(String::as_str),
    )?;
    let source: Arc<dyn AgentAdapter> = Arc::new(portal_adapters::chatgpt_web::ChatGptWebAdapter);
    let target: Arc<dyn AgentAdapter> = Arc::new(portal_adapters::bentokit::BentoKitAdapter);
    let source_inst = Installation {
        cli_path: None,
        version: None,
        store_root: source_root.display().to_string(),
    };
    let target_inst = Installation {
        cli_path: Some(args[3].clone()),
        version: None,
        store_root: root.display().to_string(),
    };
    let plan = engine::plan_with_destination(
        &source,
        &source_inst,
        &target,
        &target_inst,
        &SessionLocator {
            native_id: summary.native_id,
            store_path: None,
        },
        MigrationKind::Native,
        &BriefConfig::default(),
        Some(MigrationDestination {
            profile: args[2].clone(),
            project_id: None,
            endpoint: None,
            operation_id: None,
        }),
    )?;
    let ledger = Ledger::new(root.join("portal-test-ledger"));
    let result = engine::execute(&plan, &target, &target_inst, &ledger)?;
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"result":result,"sourceTurns":plan.session.timeline.len(),"sourceVisibleTurns":plan.session.timeline.iter().filter(|t|!t.is_meta).count(),"losses":plan.report.predicted_losses})
        )?
    );
    Ok(())
}
