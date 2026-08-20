//! Persistent application settings owned by Agent Portal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{PortalError, Result};
use crate::migration::ollama;
use crate::util::paths::atomic_write;

/// Interactive shell used when Agent Portal opens an agent in a terminal
/// (board quick-launch, resume, migration "Open in terminal").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LaunchShell {
    /// Platform default: PowerShell 7 → Windows PowerShell → cmd on Windows;
    /// `$SHELL` (else zsh/bash) on macOS/Linux.
    #[default]
    Auto,
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// Windows PowerShell 5.x (`powershell.exe`).
    PowerShell,
    /// Command Prompt (`cmd.exe`).
    Cmd,
    /// Bash (Git Bash / WSL bash on Windows, `/bin/bash` elsewhere).
    Bash,
    /// zsh (macOS/Linux).
    Zsh,
    /// fish (macOS/Linux).
    Fish,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub ollama_host: String,
    #[serde(default = "default_naming_model")]
    pub ollama_naming_model: String,
    pub ollama_model: String,
    /// Shell that runs agent CLIs when launching from the board / wizard.
    #[serde(default)]
    pub launch_shell: LaunchShell,
    /// In-process MCP over HTTP on 127.0.0.1 (shares board/FTS/WormDB with GUI).
    #[serde(default = "default_true")]
    pub mcp_http_enabled: bool,
    /// Loopback port; `0` = ephemeral (reported via mcp_http_status after bind).
    #[serde(default = "default_mcp_port")]
    pub mcp_http_port: u16,
    /// Bearer token for `Authorization: Bearer …`. Empty → no auth (loopback only).
    #[serde(default)]
    pub mcp_http_token: String,
    /// Semantic embedder: `hash` (local FNV bag-of-words) or `ollama`.
    #[serde(default = "default_embed_provider")]
    pub embed_provider: String,
    /// Ollama embedding model when `embed_provider == "ollama"`. Empty = unset.
    /// **Changing this (or the provider) requires a full semantic reindex** —
    /// WormDB freezes dimension/embedder_id per namespace.
    #[serde(default)]
    pub ollama_embed_model: String,
}

fn default_naming_model() -> String {
    ollama::DEFAULT_NAMING_MODEL.into()
}
fn default_true() -> bool {
    true
}
fn default_mcp_port() -> u16 {
    17432
}
fn default_embed_provider() -> String {
    "hash".into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            ollama_host: ollama::DEFAULT_BASE_URL.into(),
            ollama_naming_model: default_naming_model(),
            ollama_model: ollama::DEFAULT_MODEL.into(),
            launch_shell: LaunchShell::Auto,
            mcp_http_enabled: true,
            mcp_http_port: default_mcp_port(),
            mcp_http_token: String::new(),
            embed_provider: default_embed_provider(),
            ollama_embed_model: String::new(),
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> Result<()> {
        if !(self.ollama_host.starts_with("http://") || self.ollama_host.starts_with("https://")) {
            return Err(PortalError::Other(
                "Ollama host must start with http:// or https://".into(),
            ));
        }
        if self.ollama_model.trim().is_empty() {
            return Err(PortalError::Other(
                "Ollama handoff model is required".into(),
            ));
        }
        if self.ollama_naming_model.trim().is_empty() {
            return Err(PortalError::Other("Ollama naming model is required".into()));
        }
        let prov = self.embed_provider.trim().to_ascii_lowercase();
        if prov != "hash" && prov != "ollama" {
            return Err(PortalError::Other(
                "embed_provider must be \"hash\" or \"ollama\"".into(),
            ));
        }
        if prov == "ollama" && self.ollama_embed_model.trim().is_empty() {
            return Err(PortalError::Other(
                "Ollama embedding model is required when embed provider is ollama".into(),
            ));
        }
        Ok(())
    }

    /// Stable id stored in WormDB `mem_init` / chunk rows.
    ///
    /// Sanitized to WormDB's charset (`[A-Za-z0-9._/-]`, max 128 bytes) so
    /// Ollama tags like `embeddinggemma:latest` do not fail `mem_init`.
    pub fn embedder_id(&self) -> String {
        match self.embed_provider.trim().to_ascii_lowercase().as_str() {
            "ollama" => crate::search::sanitize_embedder_id(&format!(
                "ollama/{}",
                self.ollama_embed_model.trim()
            )),
            _ => crate::search::HASH_EMBEDDER_ID.to_string(),
        }
    }

    pub fn embed_provider_is_ollama(&self) -> bool {
        self.embed_provider.trim().eq_ignore_ascii_case("ollama")
    }
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            path: app_data_dir.join("settings.json"),
        }
    }

    pub fn load(&self) -> AppSettings {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|json| {
                let has_naming_model = serde_json::from_str::<serde_json::Value>(&json)
                    .ok()
                    .and_then(|value| value.get("ollamaNamingModel").cloned())
                    .is_some();
                let mut settings = serde_json::from_str::<AppSettings>(&json).ok()?;
                if !has_naming_model {
                    settings.ollama_naming_model = settings.ollama_model.clone();
                }
                Some(settings)
            })
            .unwrap_or_default()
    }

    pub fn save(&self, settings: &AppSettings) -> Result<()> {
        let mut settings = settings.clone();
        settings.ollama_host = settings
            .ollama_host
            .trim()
            .trim_end_matches('/')
            .to_string();
        settings.ollama_model = settings.ollama_model.trim().to_string();
        settings.ollama_naming_model = settings.ollama_naming_model.trim().to_string();
        settings.embed_provider = settings.embed_provider.trim().to_ascii_lowercase();
        settings.ollama_embed_model = settings.ollama_embed_model.trim().to_string();
        settings.validate()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes =
            serde_json::to_vec_pretty(&settings).map_err(|e| PortalError::Other(e.to_string()))?;
        atomic_write(&self.path, &bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_persist_and_invalid_hosts_are_rejected() {
        let dir = std::env::temp_dir().join(format!("portal-settings-{}", uuid::Uuid::now_v7()));
        let store = SettingsStore::new(&dir);
        assert_eq!(store.load().launch_shell, LaunchShell::Auto);
        let value = AppSettings {
            ollama_host: "http://model-box:11434".into(),
            ollama_naming_model: "qwen3:0.6b".into(),
            ollama_model: "qwen3:8b".into(),
            launch_shell: LaunchShell::Pwsh,
            ..Default::default()
        };
        store.save(&value).unwrap();
        assert_eq!(store.load().ollama_model, "qwen3:8b");
        assert_eq!(store.load().launch_shell, LaunchShell::Pwsh);
        store
            .save(&AppSettings {
                ollama_model: "qwen3:14b".into(),
                ..value.clone()
            })
            .unwrap();
        assert_eq!(store.load().ollama_model, "qwen3:14b");
        assert!(store
            .save(&AppSettings {
                ollama_host: "model-box".into(),
                ..value
            })
            .is_err());

        std::fs::write(
            &store.path,
            r#"{"ollamaHost":"http://localhost:11434","ollamaModel":"legacy:7b"}"#,
        )
        .unwrap();
        assert_eq!(store.load().ollama_naming_model, "legacy:7b");
        assert_eq!(store.load().launch_shell, LaunchShell::Auto);
        std::fs::remove_dir_all(dir).ok();
    }
}
