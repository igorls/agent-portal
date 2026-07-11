//! Optional local-LLM brief enrichment via Ollama.
//!
//! This is the only place the app talks to a model, and it only ever talks to
//! a local Ollama server. It takes the deterministic brief (which already
//! holds every fact) and asks a local model to rewrite it into cleaner prose.
//! Any failure — server down, model missing, timeout, malformed reply —
//! returns `None`, and the caller keeps the deterministic text. The LLM can
//! only improve wording, never invent or gate the migration.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";
pub const DEFAULT_MODEL: &str = "gemma4:12b-it-qat";

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStatus {
    pub available: bool,
    pub base_url: String,
    pub models: Vec<String>,
    /// The default model, present in `models` when true.
    pub default_model: String,
    pub default_present: bool,
}

/// Probe the local Ollama server. Short timeout — this gates a UI toggle, so a
/// missing server must fail fast, not hang the dry-run.
pub fn status(base_url: &str) -> OllamaStatus {
    let models = list_models(base_url).unwrap_or_default();
    let default_present = models.iter().any(|m| m == DEFAULT_MODEL);
    OllamaStatus {
        available: !models.is_empty() || reachable(base_url),
        base_url: base_url.to_string(),
        default_present,
        default_model: DEFAULT_MODEL.to_string(),
        models,
    }
}

fn reachable(base_url: &str) -> bool {
    ureq::get(&format!("{base_url}/api/tags"))
        .timeout(Duration::from_secs(2))
        .call()
        .is_ok()
}

fn list_models(base_url: &str) -> Option<Vec<String>> {
    let resp = ureq::get(&format!("{base_url}/api/tags"))
        .timeout(Duration::from_secs(2))
        .call()
        .ok()?;
    let json: serde_json::Value = resp.into_json().ok()?;
    let models = json
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    Some(models)
}

const SYSTEM_PROMPT: &str = "\
You are a technical editor. You are given a machine-generated handoff brief that \
transfers a coding session from one AI coding agent to another. Rewrite it into a \
clear, well-organized markdown handoff for the receiving agent.

Rules:
- Preserve every fact: goal, file paths, commands, tool names, and next steps. Do NOT invent anything.
- Keep all file paths and commands verbatim, in backticks.
- Be concise and skimmable. Use short sections and bullet lists.
- Do not add preamble, sign-off, or commentary about the rewrite itself.
- Output only the finished markdown brief.";

/// Rewrite the deterministic brief with a local model. Returns `None` (caller
/// keeps the deterministic text) on any failure.
pub fn enrich(base_url: &str, model: &str, deterministic_brief: &str) -> Option<String> {
    let body = serde_json::json!({
        "model": model,
        "system": SYSTEM_PROMPT,
        "prompt": format!("Rewrite this handoff brief:\n\n{deterministic_brief}"),
        "stream": false,
        "options": { "temperature": 0.2 }
    });

    let resp = ureq::post(&format!("{base_url}/api/generate"))
        .timeout(Duration::from_secs(90))
        .send_json(body)
        .ok()?;
    let json: serde_json::Value = resp.into_json().ok()?;
    let text = json.get("response")?.as_str()?.trim();
    if text.len() < 40 {
        return None; // implausibly short → not a usable brief
    }
    Some(strip_code_fence(text))
}

/// Improve an extractive compaction summary. The deterministic text remains
/// the fallback and constrains the model to facts already present.
pub fn compact(base_url: &str, model: &str, deterministic: &str) -> Option<String> {
    let generated = generate(base_url, model,
        "Summarize coding-session history for a successor agent. Preserve goals, decisions, file paths, commands, failures, completed work, and next steps. Never invent facts. Output only a concise markdown summary.",
        deterministic, 90)?;
    Some(generated.chars().take(24_000).collect())
}

/// Generate a short title describing the work at the current tail.
pub fn title(base_url: &str, model: &str, recent_activity: &str) -> Option<String> {
    let text = generate(base_url, model,
        "Name the current coding task from its recent activity. Return only a specific 3-8 word title, no quotes, punctuation suffix, or explanation. Describe the latest work, not the initial request.",
        recent_activity, 30)?;
    let title = text
        .lines()
        .next()?
        .trim()
        .trim_matches(['\"', '\'', '`'])
        .to_string();
    if title.len() < 3 || title.len() > 80 {
        None
    } else {
        Some(title)
    }
}

fn generate(
    base_url: &str,
    model: &str,
    system: &str,
    prompt: &str,
    timeout_secs: u64,
) -> Option<String> {
    let body = serde_json::json!({"model": model, "system": system, "prompt": prompt, "stream": false, "options": {"temperature": 0.1}});
    let resp = ureq::post(&format!("{base_url}/api/generate"))
        .timeout(Duration::from_secs(timeout_secs))
        .send_json(body)
        .ok()?;
    let json: serde_json::Value = resp.into_json().ok()?;
    let text = json.get("response")?.as_str()?.trim();
    (!text.is_empty()).then(|| strip_code_fence(text))
}

/// Models sometimes wrap the whole output in a ```markdown fence; unwrap it.
fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.strip_prefix("markdown").unwrap_or(rest);
        if let Some(inner) = rest.trim_start().strip_suffix("```") {
            return inner.trim().to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markdown_fence() {
        assert_eq!(
            strip_code_fence("```markdown\n# Hi\n\ntext\n```"),
            "# Hi\n\ntext"
        );
        assert_eq!(
            strip_code_fence("# plain\n\nno fence"),
            "# plain\n\nno fence"
        );
    }
}
