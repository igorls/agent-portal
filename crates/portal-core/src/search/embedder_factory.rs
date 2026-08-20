//! Build an [`Embedder`] from [`AppSettings`].

use std::sync::Arc;

use crate::search::embed::{Embedder, HashEmbedder, OllamaEmbedder};
use crate::settings::AppSettings;

pub fn embedder_from_settings(settings: &AppSettings) -> Arc<dyn Embedder> {
    if settings.embed_provider_is_ollama() {
        let model = settings.ollama_embed_model.trim();
        if !model.is_empty() {
            return Arc::new(OllamaEmbedder::new(&settings.ollama_host, model));
        }
    }
    Arc::new(HashEmbedder::default())
}
