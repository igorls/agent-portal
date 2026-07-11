//! Built-in agent adapters, one module per agent. Registered explicitly —
//! adding an agent means adding a module and one line here, then making the
//! conformance suite pass.

pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod opencode;

use std::sync::Arc;

use portal_core::adapter::AgentAdapter;

pub fn builtin_adapters() -> Vec<Arc<dyn AgentAdapter>> {
    vec![
        Arc::new(claude_code::ClaudeCodeAdapter),
        Arc::new(codex::CodexAdapter),
        Arc::new(opencode::OpenCodeAdapter),
        Arc::new(antigravity::AntigravityAdapter),
    ]
}

pub fn builtin_adapter_count() -> u32 {
    builtin_adapters().len() as u32
}
