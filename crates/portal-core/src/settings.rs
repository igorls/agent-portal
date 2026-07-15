//! Persistent application settings owned by Agent Portal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{PortalError, Result};
use crate::migration::ollama;
use crate::util::paths::atomic_write;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub ollama_host: String,
    #[serde(default = "default_naming_model")]
    pub ollama_naming_model: String,
    pub ollama_model: String,
}

fn default_naming_model() -> String {
    ollama::DEFAULT_NAMING_MODEL.into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            ollama_host: ollama::DEFAULT_BASE_URL.into(),
            ollama_naming_model: default_naming_model(),
            ollama_model: ollama::DEFAULT_MODEL.into(),
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
        Ok(())
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
        let value = AppSettings {
            ollama_host: "http://model-box:11434".into(),
            ollama_naming_model: "qwen3:0.6b".into(),
            ollama_model: "qwen3:8b".into(),
        };
        store.save(&value).unwrap();
        assert_eq!(store.load().ollama_model, "qwen3:8b");
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
        std::fs::remove_dir_all(dir).ok();
    }
}
