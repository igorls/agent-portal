//! Effective installation capabilities must preserve source-restricted writers.
use std::collections::HashMap;
use std::sync::Arc;

use portal_adapters::{
    bentokit::BentoKitAdapter, claude_code::ClaudeCodeAdapter, codex::CodexAdapter,
    grok::GrokAdapter,
};
use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::Installation;
use portal_core::migration::engine;
use portal_core::registry::AgentRegistry;

struct TestEnv(HostEnv);
impl TestEnv {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("portal-capabilities-{}", uuid::Uuid::new_v4()));
        let mut store_overrides = HashMap::new();
        for id in ["claude-code", "codex", "grok-build", "bentokit"] {
            let dir = root.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            store_overrides.insert(id.to_string(), dir);
        }
        Self(HostEnv {
            home: root,
            path_dirs: vec![],
            store_overrides,
        })
    }
}
impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0.home);
    }
}
fn registry() -> AgentRegistry {
    AgentRegistry::new(vec![
        Arc::new(ClaudeCodeAdapter),
        Arc::new(CodexAdapter),
        Arc::new(GrokAdapter),
        Arc::new(BentoKitAdapter),
    ])
}

#[test]
fn grok_keeps_its_source_restriction_and_bento_without_protocol_stays_read_only() {
    let env = TestEnv::new();
    let pairs = registry().feasibility(&env.0);
    let pair = |source: &str, target: &str| {
        pairs
            .iter()
            .find(|p| p.source == source && p.target == target)
            .unwrap()
    };
    assert!(pair("claude-code", "grok-build").native);
    assert!(!pair("codex", "grok-build").native);
    assert!(pair("codex", "grok-build").brief);
    assert!(pair("bentokit", "codex").native);
    assert!(pairs
        .iter()
        .filter(|p| p.target == "bentokit")
        .all(|p| !p.native));

    let source: Arc<dyn AgentAdapter> = Arc::new(CodexAdapter);
    let source_inst = Installation {
        cli_path: None,
        version: None,
        store_root: env.0.home.join("codex").display().to_string(),
    };
    // Reject unsupported pairs before reading or mutating any source/target data.
    for target in [
        Arc::new(GrokAdapter) as Arc<dyn AgentAdapter>,
        Arc::new(BentoKitAdapter),
    ] {
        let target_inst = target.detect(&env.0).unwrap();
        let error = engine::plan_native(
            &source,
            &source_inst,
            &target,
            &target_inst,
            &SessionLocator {
                native_id: "not-read".into(),
                store_path: None,
            },
        )
        .err()
        .unwrap();
        assert!(error
            .to_string()
            .contains("does not support native imports"));
    }
}

#[cfg(unix)]
#[test]
fn bento_protocol_probe_enables_destination_without_broadening_grok_sources() {
    use std::os::unix::fs::PermissionsExt;
    let mut env = TestEnv::new();
    let bin = env.0.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cli = bin.join("bentokit");
    std::fs::write(&cli, "#!/bin/sh\nprintf 'unknown session command\\n'\n").unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.0.path_dirs.push(bin);
    assert!(registry()
        .feasibility(&env.0)
        .iter()
        .filter(|p| p.target == "bentokit")
        .all(|p| !p.native));

    std::fs::write(&cli, "#!/bin/sh\nprintf '{\"version\":1}\\n'\n").unwrap();
    let pairs = registry().feasibility(&env.0);
    assert!(pairs
        .iter()
        .filter(|p| p.target == "bentokit")
        .all(|p| p.native));
    assert!(
        pairs
            .iter()
            .find(|p| p.source == "claude-code" && p.target == "grok-build")
            .unwrap()
            .native
    );
    assert!(
        !pairs
            .iter()
            .find(|p| p.source == "codex" && p.target == "grok-build")
            .unwrap()
            .native
    );
}
