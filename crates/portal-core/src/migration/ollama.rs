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
use sysinfo::System;
use ts_rs::TS;

pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";
pub const DEFAULT_MODEL: &str = "gemma4:12b";
pub const DEFAULT_NAMING_MODEL: &str = "qwen3.5:0.8b";

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStatus {
    pub available: bool,
    pub base_url: String,
    pub models: Vec<String>,
    pub model_details: Vec<OllamaModel>,
    /// The default model, present in `models` when true.
    pub default_model: String,
    pub default_present: bool,
    pub hardware: HardwareProfile,
    pub recommendations: Vec<ModelRecommendation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OllamaModel {
    pub name: String,
    pub size: u64,
    pub parameter_size: Option<String>,
    pub quantization_level: Option<String>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HardwareProfile {
    pub total_memory: u64,
    pub available_memory: u64,
    pub logical_cpus: usize,
    pub platform: String,
    pub architecture: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ModelRecommendation {
    pub name: String,
    pub label: String,
    pub description: String,
    pub download_size: u64,
    pub estimated_memory: u64,
    pub installed: bool,
    pub recommended: bool,
    pub fit: String,
}

/// Probe the local Ollama server. Short timeout — this gates a UI toggle, so a
/// missing server must fail fast, not hang the dry-run.
pub fn status(base_url: &str) -> OllamaStatus {
    let model_details = list_models(base_url).unwrap_or_default();
    let models = model_details
        .iter()
        .map(|model| model.name.clone())
        .collect::<Vec<_>>();
    let default_present = models.iter().any(|m| m == DEFAULT_MODEL);
    let hardware = hardware_profile();
    let recommendations = recommendations(&hardware, &models);
    OllamaStatus {
        available: !models.is_empty() || reachable(base_url),
        base_url: base_url.to_string(),
        default_present,
        default_model: DEFAULT_MODEL.to_string(),
        models,
        model_details,
        hardware,
        recommendations,
    }
}

fn reachable(base_url: &str) -> bool {
    ureq::get(&format!("{base_url}/api/tags"))
        .timeout(Duration::from_secs(2))
        .call()
        .is_ok()
}

fn list_models(base_url: &str) -> Option<Vec<OllamaModel>> {
    let resp = ureq::get(&format!("{base_url}/api/tags"))
        .timeout(Duration::from_secs(2))
        .call()
        .ok()?;
    let json: serde_json::Value = resp.into_json().ok()?;
    let models = json
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|model| {
            Some(OllamaModel {
                name: model.get("name")?.as_str()?.to_string(),
                size: model
                    .get("size")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                parameter_size: model
                    .pointer("/details/parameter_size")
                    .and_then(|value| value.as_str())
                    .map(String::from),
                quantization_level: model
                    .pointer("/details/quantization_level")
                    .and_then(|value| value.as_str())
                    .map(String::from),
                modified_at: model
                    .get("modified_at")
                    .and_then(|value| value.as_str())
                    .map(String::from),
            })
        })
        .collect();
    Some(models)
}

pub fn pull(base_url: &str, model: &str) -> Result<(), String> {
    let response = ureq::post(&format!("{base_url}/api/pull"))
        .timeout(Duration::from_secs(20 * 60))
        .send_json(serde_json::json!({ "model": model, "stream": false }))
        .map_err(|error| error.to_string())?;
    let json: serde_json::Value = response.into_json().map_err(|error| error.to_string())?;
    match json.get("status").and_then(|value| value.as_str()) {
        Some("success") => Ok(()),
        Some(status) => Err(format!("Ollama pull ended with status '{status}'")),
        None => Err("Ollama returned an invalid pull response".into()),
    }
}

fn hardware_profile() -> HardwareProfile {
    let mut system = System::new_all();
    system.refresh_all();
    HardwareProfile {
        total_memory: system.total_memory(),
        available_memory: system.available_memory(),
        logical_cpus: system.cpus().len(),
        platform: System::name().unwrap_or_else(|| std::env::consts::OS.to_string()),
        architecture: std::env::consts::ARCH.to_string(),
    }
}

fn recommendations(hardware: &HardwareProfile, installed: &[String]) -> Vec<ModelRecommendation> {
    const GIB: u64 = 1024 * 1024 * 1024;
    let apple_silicon = hardware.platform.to_ascii_lowercase().contains("mac")
        && matches!(hardware.architecture.as_str(), "aarch64" | "arm64");
    let choices = if apple_silicon {
        [
            (
                "qwen3.5:0.8b-mlx",
                "Fastest",
                "Current Qwen 3.5 MLX model for near-instant short titles",
                1_200_000_000,
                2_000_000_000,
            ),
            (
                "qwen3.5:2b-mlx",
                "Recommended",
                "Best speed and title-quality balance on Apple Silicon",
                3_100_000_000,
                4_500_000_000,
            ),
            (
                "qwen3.5:4b-mlx",
                "Higher quality",
                "Stronger instruction following while remaining responsive",
                4_000_000_000,
                6_000_000_000,
            ),
            (
                "qwen3.5:9b-mlx",
                "Best quality",
                "More nuanced titles for Macs with generous unified memory",
                8_900_000_000,
                12_000_000_000,
            ),
        ]
    } else {
        [
            (
                "qwen3.5:0.8b",
                "Fastest",
                "Current Qwen 3.5 model for near-instant short titles",
                1_000_000_000,
                1_800_000_000,
            ),
            (
                "qwen3.5:2b",
                "Recommended",
                "Best speed and title-quality balance for most computers",
                2_700_000_000,
                4_000_000_000,
            ),
            (
                "qwen3.5:4b",
                "Higher quality",
                "Stronger instruction following while remaining responsive",
                3_400_000_000,
                5_500_000_000,
            ),
            (
                "qwen3.5:9b",
                "Best quality",
                "More nuanced titles for machines with generous memory",
                6_600_000_000,
                10_000_000_000,
            ),
        ]
    };
    let preferred_index = if hardware.total_memory < 8 * GIB {
        0
    } else if hardware.total_memory < 24 * GIB {
        1
    } else if hardware.total_memory < 48 * GIB {
        2
    } else {
        3
    };
    let preferred = choices[preferred_index].0;

    choices
        .into_iter()
        .map(
            |(name, label, description, download_size, estimated_memory)| ModelRecommendation {
                name: name.to_string(),
                label: label.to_string(),
                description: description.to_string(),
                download_size,
                estimated_memory,
                installed: installed.iter().any(|candidate| candidate == name),
                recommended: name == preferred,
                fit: if estimated_memory * 3 < hardware.total_memory {
                    "great".into()
                } else if estimated_memory * 2 < hardware.total_memory {
                    "good".into()
                } else {
                    "tight".into()
                },
            },
        )
        .collect()
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
        "think": false,
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
    let body = serde_json::json!({"model": model, "system": system, "prompt": prompt, "stream": false, "think": false, "options": {"temperature": 0.1}});
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

    #[test]
    fn recommendations_choose_a_model_that_fits_memory() {
        let hardware = HardwareProfile {
            total_memory: 16 * 1024 * 1024 * 1024,
            available_memory: 8 * 1024 * 1024 * 1024,
            logical_cpus: 8,
            platform: "test".into(),
            architecture: "test".into(),
        };
        let choices = recommendations(&hardware, &["qwen3.5:2b".into()]);
        assert_eq!(
            choices
                .iter()
                .find(|choice| choice.recommended)
                .unwrap()
                .name,
            "qwen3.5:2b"
        );
        assert!(
            choices
                .iter()
                .find(|choice| choice.name == "qwen3.5:2b")
                .unwrap()
                .installed
        );
    }

    #[test]
    fn recommendations_use_mlx_models_on_apple_silicon() {
        let hardware = HardwareProfile {
            total_memory: 16 * 1024 * 1024 * 1024,
            available_memory: 8 * 1024 * 1024 * 1024,
            logical_cpus: 8,
            platform: "macOS".into(),
            architecture: "aarch64".into(),
        };
        let choices = recommendations(&hardware, &[]);
        assert_eq!(
            choices
                .iter()
                .find(|choice| choice.recommended)
                .unwrap()
                .name,
            "qwen3.5:2b-mlx"
        );
        assert!(choices.iter().all(|choice| choice.name.ends_with("-mlx")));
    }
}
