use std::sync::Arc;

use crate::adapter::{AgentAdapter, HostEnv};
use crate::dto::{
    AgentDescriptor, BoardSnapshot, Lane, PairFeasibility, ProjectGroup, ProjectRef,
    SessionSummary, SupportLevel,
};
use crate::util::paths::normalize_cwd;

/// Explicit adapter registry. No linker magic: portal-adapters hands us the
/// full list and everything (detection, board assembly, feasibility) derives
/// from it.
pub struct AgentRegistry {
    adapters: Vec<Arc<dyn AgentAdapter>>,
}

impl AgentRegistry {
    pub fn new(adapters: Vec<Arc<dyn AgentAdapter>>) -> Self {
        Self { adapters }
    }

    pub fn adapters(&self) -> &[Arc<dyn AgentAdapter>] {
        &self.adapters
    }

    pub fn descriptor(&self, adapter: &dyn AgentAdapter, env: &HostEnv) -> AgentDescriptor {
        AgentDescriptor {
            id: adapter.id().to_string(),
            display_name: adapter.display_name().to_string(),
            capabilities: adapter.capabilities(),
            installation: adapter.detect(env),
        }
    }

    pub fn descriptors(&self, env: &HostEnv) -> Vec<AgentDescriptor> {
        self.adapters
            .iter()
            .map(|a| self.descriptor(a.as_ref(), env))
            .collect()
    }

    /// Assemble the whole board: one lane per adapter, sessions grouped by
    /// project, everything sorted most-recent-first. Enumeration failures in
    /// one lane must never take down the board — they surface as lane errors.
    pub fn board(&self, env: &HostEnv) -> BoardSnapshot {
        let lanes = self
            .adapters
            .iter()
            .map(|adapter| {
                let descriptor = self.descriptor(adapter.as_ref(), env);
                let (projects, error) = match &descriptor.installation {
                    Some(inst) => match adapter.snapshot(inst) {
                        Ok(snapshot) => (group_projects(snapshot), None),
                        Err(e) => (Vec::new(), Some(e.to_string())),
                    },
                    None => (Vec::new(), None),
                };
                Lane {
                    agent: descriptor,
                    projects,
                    error,
                }
            })
            .collect();

        BoardSnapshot {
            feasibility: self.feasibility(env),
            lanes,
            generated_at: chrono::Utc::now(),
        }
    }

    /// Ordered (source, target) migration feasibility over detected agents.
    pub fn feasibility(&self, env: &HostEnv) -> Vec<PairFeasibility> {
        let detected: Vec<(&Arc<dyn AgentAdapter>, bool)> = self
            .adapters
            .iter()
            .map(|a| (a, a.detect(env).is_some()))
            .collect();

        let mut out = Vec::new();
        for (source, source_detected) in &detected {
            let source_caps = source.capabilities();
            let can_read = *source_detected && source_caps.read != SupportLevel::None;
            for (target, target_detected) in &detected {
                if source.id() == target.id() {
                    continue;
                }
                let target_caps = target.capabilities();
                let native =
                    can_read && *target_detected && target.accepts_native_from(source.id());
                let brief = can_read && *target_detected && target_caps.launch_new;
                out.push(PairFeasibility {
                    source: source.id().to_string(),
                    target: target.id().to_string(),
                    native,
                    brief,
                    write_confidence: target_caps.write_confidence.clone(),
                });
            }
        }
        out
    }
}

fn group_projects(snapshot: Vec<(ProjectRef, Vec<SessionSummary>)>) -> Vec<ProjectGroup> {
    let mut groups: Vec<ProjectGroup> = snapshot
        .into_iter()
        .filter(|(_, sessions)| !sessions.is_empty())
        .map(|(project, mut sessions)| {
            sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
            ProjectGroup {
                cwd_normalized: project.cwd.as_deref().map(normalize_cwd),
                key: project.key,
                label: project.label,
                sessions,
            }
        })
        .collect();

    // Most recently active project first.
    groups.sort_by_key(|g| std::cmp::Reverse(g.sessions.first().and_then(|s| s.updated_at)));
    groups
}
