//! Claude Cowork adapter.
//!
//! Cowork has two stores. Legacy local sessions live below Claude Desktop's
//! `local-agent-mode-sessions` directory as metadata JSON plus an `audit.jsonl`
//! transcript. Current Cowork sessions run remotely; Claude Desktop retains a
//! folder grant map and Chromium HTTP-cache entries for session metadata,
//! history, and the live SSE stream. The remote cache is useful but evictable,
//! so this adapter deliberately advertises partial, read-only support.

mod cache;
mod read;

use std::path::PathBuf;

use portal_core::adapter::{AgentAdapter, HostEnv, SessionLocator};
use portal_core::dto::{
    Capabilities, Installation, ProjectRef, SessionSummary, StoreKind, SupportLevel,
};
use portal_core::error::Result;
use portal_core::ir::CanonicalSession;

pub const ID: &str = "claude-cowork";

pub struct ClaudeCoworkAdapter;

impl AgentAdapter for ClaudeCoworkAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Claude Cowork"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            store_kind: StoreKind::CloudApi,
            read: SupportLevel::Partial,
            write_native: SupportLevel::None,
            watch: true,
            launch_resume: false,
            launch_new: false,
            context_tokens: None,
            write_confidence: None,
            version_range_tested: "Claude Desktop 1.21459.x".into(),
            notes: vec![
                "Reads legacy local sessions and cached remote Cowork sessions".into(),
                "Remote history may disappear when Claude Desktop evicts its HTTP cache".into(),
                "Remote cache parsing is experimental and version-gated by fixtures".into(),
                "Native Cowork store writes are disabled".into(),
            ],
        }
    }

    fn detect(&self, env: &HostEnv) -> Option<Installation> {
        let store_root = env.store_root(ID, default_root(&env.home));
        if !store_root.is_dir() {
            return None;
        }
        Some(Installation {
            cli_path: desktop_app_path(),
            version: None,
            store_root: store_root.display().to_string(),
        })
    }

    fn list_projects(&self, inst: &Installation) -> Result<Vec<ProjectRef>> {
        Ok(read::snapshot(inst)?
            .into_iter()
            .map(|(project, _)| project)
            .collect())
    }

    fn list_sessions(
        &self,
        inst: &Installation,
        project: &ProjectRef,
    ) -> Result<Vec<SessionSummary>> {
        Ok(read::snapshot(inst)?
            .into_iter()
            .find(|(candidate, _)| candidate.key == project.key)
            .map(|(_, sessions)| sessions)
            .unwrap_or_default())
    }

    fn snapshot(&self, inst: &Installation) -> Result<Vec<(ProjectRef, Vec<SessionSummary>)>> {
        read::snapshot(inst)
    }

    fn read_session(
        &self,
        inst: &Installation,
        locator: &SessionLocator,
    ) -> Result<CanonicalSession> {
        read::read_session(inst, locator)
    }
}

#[cfg(target_os = "macos")]
fn default_root(home: &std::path::Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Claude")
}

#[cfg(target_os = "windows")]
fn default_root(home: &std::path::Path) -> PathBuf {
    home.join("AppData").join("Roaming").join("Claude")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn default_root(home: &std::path::Path) -> PathBuf {
    home.join(".config").join("Claude")
}

#[cfg(target_os = "macos")]
fn desktop_app_path() -> Option<String> {
    let path = PathBuf::from("/Applications/Claude.app");
    path.is_dir().then(|| path.display().to_string())
}

#[cfg(not(target_os = "macos"))]
fn desktop_app_path() -> Option<String> {
    None
}
