//! Local embedders for WormDB `mem_*` vectors.
//!
//! Hash embedder mirrors WormDB's demo embedder (deterministic FNV-1a bag-of-words)
//! so plumbing/recall can be tested without a model. Swap to Ollama when configured.

use crate::error::{PortalError, Result};

pub const HASH_EMBED_DIM: usize = 128;
pub const HASH_EMBEDDER_ID: &str = "hash-fnv1a.dim-128.v1";

/// WormDB `mem_init` / `mem_add` embedder_id rules:
/// 1..=128 bytes, only `[A-Za-z0-9._/-]`.
///
/// Ollama model tags use `:` (`embeddinggemma:latest`) which is rejected — map
/// invalid chars so namespace init always succeeds.
pub fn sanitize_embedder_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(128));
    for c in raw.chars() {
        let mapped = match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '/' | '-' => c,
            // Common Ollama form: name:tag → name/tag (still human-readable).
            ':' => '/',
            _ => '_',
        };
        out.push(mapped);
        if out.len() >= 128 {
            break;
        }
    }
    if out.is_empty() {
        "embedder".into()
    } else {
        out
    }
}

pub trait Embedder: Send + Sync {
    fn id(&self) -> &str;
    fn dim(&self) -> usize;
    /// L2-normalized little-endian f32 bytes.
    fn embed(&self, text: &str) -> Result<Vec<u8>>;

    /// Embed many texts. Default loops [`embed`]; Ollama overrides with one
    /// `/api/embed` request (much faster bulk indexing).
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// Convert f64 embedding to little-endian f32 bytes (WormDB / mem_add).
pub fn f64_embedding_to_bytes(values: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for &v in values {
        out.extend_from_slice(&(v as f32).to_le_bytes());
    }
    out
}

/// Deterministic hash embedder (not semantic — for plumbing + synthetic recall).
pub struct HashEmbedder {
    dim: usize,
    id: String,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(HASH_EMBED_DIM)
    }
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self {
            dim: dim.max(8),
            id: format!("hash-fnv1a.dim-{dim}.v1"),
        }
    }
}

impl Embedder for HashEmbedder {
    fn id(&self) -> &str {
        &self.id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Vec<u8>> {
        let mut vec = vec![0f32; self.dim];
        let tokens = tokenize(text);
        if tokens.is_empty() {
            vec[0] = 1.0;
        } else {
            for token in tokens {
                let bytes = token.as_bytes();
                let bucket = (fnv1a(FNV_OFFSET, bytes) % self.dim as u32) as usize;
                let sign_hash = fnv1a(FNV_OFFSET ^ 0x9e37_79b9, bytes);
                let sign = if sign_hash & 1 == 0 { 1.0 } else { -1.0 };
                vec[bucket] += sign;
            }
        }
        // L2 normalize
        let mut norm = 0f32;
        for v in &vec {
            norm += v * v;
        }
        let inv = if norm > 0.0 { 1.0 / norm.sqrt() } else { 0.0 };
        for v in &mut vec {
            *v *= inv;
        }
        let mut out = Vec::with_capacity(self.dim * 4);
        for v in vec {
            out.extend_from_slice(&v.to_le_bytes());
        }
        Ok(out)
    }
}

/// Ollama `/api/embeddings` embedder (optional; fails soft when server/model missing).
pub struct OllamaEmbedder {
    host: String,
    model: String,
    id: String,
    dim: std::sync::Mutex<Option<usize>>,
}

impl OllamaEmbedder {
    pub fn new(host: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        let host = host.into().trim_end_matches('/').to_string();
        // Must match WormDB charset; keep Ollama API model string unchanged.
        let id = sanitize_embedder_id(&format!("ollama/{model}"));
        Self {
            host,
            model,
            id,
            dim: std::sync::Mutex::new(None),
        }
    }
}

impl Embedder for OllamaEmbedder {
    fn id(&self) -> &str {
        &self.id
    }

    fn dim(&self) -> usize {
        self.dim.lock().unwrap().unwrap_or(0)
    }

    fn embed(&self, text: &str) -> Result<Vec<u8>> {
        let batch = self.embed_batch(&[text.to_string()])?;
        batch
            .into_iter()
            .next()
            .ok_or_else(|| PortalError::Other("ollama embed empty batch".into()))
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // Prefer modern multi-input /api/embed; fall back to legacy one-by-one.
        match self.embed_new_batch(texts) {
            Ok(rows) => Ok(rows),
            Err(e_batch) => {
                if texts.len() == 1 {
                    let values = self.embed_legacy(&texts[0]).map_err(|e2| {
                        PortalError::Other(format!(
                            "ollama embed failed ({e_batch}); legacy also failed: {e2}"
                        ))
                    })?;
                    if values.is_empty() {
                        return Err(PortalError::Other("ollama returned empty embedding".into()));
                    }
                    *self.dim.lock().unwrap() = Some(values.len());
                    return Ok(vec![f64_embedding_to_bytes(&values)]);
                }
                // Multi: try sequential modern single, then legacy.
                let mut out = Vec::with_capacity(texts.len());
                for t in texts {
                    let values = self
                        .embed_new_batch(std::slice::from_ref(t))
                        .or_else(|_| {
                            self.embed_legacy(t)
                                .map(|v| vec![f64_embedding_to_bytes(&v)])
                        })
                        .map_err(|e| {
                            PortalError::Other(format!(
                                "ollama embed batch fallback failed ({e_batch}): {e}"
                            ))
                        })?;
                    let row = values
                        .into_iter()
                        .next()
                        .ok_or_else(|| PortalError::Other("ollama embed empty row".into()))?;
                    out.push(row);
                }
                Ok(out)
            }
        }
    }
}

impl OllamaEmbedder {
    /// Ollama `/api/embed` with `input` as string or array of strings.
    fn embed_new_batch(&self, texts: &[String]) -> Result<Vec<Vec<u8>>> {
        #[derive(serde::Serialize)]
        struct Req {
            model: String,
            input: serde_json::Value,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            #[serde(default)]
            embeddings: Vec<Vec<f64>>,
            #[serde(default)]
            embedding: Vec<f64>,
        }

        let input = if texts.len() == 1 {
            serde_json::Value::String(texts[0].clone())
        } else {
            serde_json::Value::Array(
                texts
                    .iter()
                    .map(|t| serde_json::Value::String(t.clone()))
                    .collect(),
            )
        };

        // Larger batches need more time (GPU usually still << 120s).
        let timeout_secs = (30 + texts.len() as u64 * 5).min(300);
        let resp = ureq::post(&format!("{}/api/embed", self.host))
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .send_json(Req {
                model: self.model.clone(),
                input,
            })
            .map_err(|e| PortalError::Other(format!("/api/embed: {e}")))?;
        let body: Resp = resp
            .into_json()
            .map_err(|e| PortalError::Other(format!("/api/embed parse: {e}")))?;

        let mut rows: Vec<Vec<f64>> = body.embeddings;
        if rows.is_empty() && !body.embedding.is_empty() {
            rows.push(body.embedding);
        }
        if rows.is_empty() {
            return Err(PortalError::Other("/api/embed empty".into()));
        }
        if rows.len() != texts.len() {
            return Err(PortalError::Other(format!(
                "/api/embed expected {} vectors, got {}",
                texts.len(),
                rows.len()
            )));
        }
        if let Some(first) = rows.first() {
            if !first.is_empty() {
                *self.dim.lock().unwrap() = Some(first.len());
            }
        }
        rows.into_iter()
            .map(|v| {
                if v.is_empty() {
                    Err(PortalError::Other(
                        "ollama returned empty embedding row".into(),
                    ))
                } else {
                    Ok(f64_embedding_to_bytes(&v))
                }
            })
            .collect::<Result<Vec<_>>>()
    }

    fn embed_legacy(&self, text: &str) -> Result<Vec<f64>> {
        #[derive(serde::Serialize)]
        struct Req {
            model: String,
            prompt: String,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            embedding: Vec<f64>,
        }
        let resp = ureq::post(&format!("{}/api/embeddings", self.host))
            .timeout(std::time::Duration::from_secs(120))
            .send_json(Req {
                model: self.model.clone(),
                prompt: text.to_string(),
            })
            .map_err(|e| PortalError::Other(format!("/api/embeddings: {e}")))?;
        let body: Resp = resp
            .into_json()
            .map_err(|e| PortalError::Other(format!("/api/embeddings parse: {e}")))?;
        Ok(body.embedding)
    }
}

const FNV_OFFSET: u32 = 2166136261;
const FNV_PRIME: u32 = 16777619;

fn fnv1a(seed: u32, bytes: &[u8]) -> u32 {
    let mut h = seed;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            cur.push(ch.to_ascii_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
#[allow(clippy::chunks_exact_to_as_chunks)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_ollama_tag_colon() {
        assert_eq!(
            sanitize_embedder_id("ollama/embeddinggemma:latest"),
            "ollama/embeddinggemma/latest"
        );
        assert_eq!(
            OllamaEmbedder::new("http://localhost:11434", "embeddinggemma:latest").id(),
            "ollama/embeddinggemma/latest"
        );
        assert!(sanitize_embedder_id("hash-fnv1a.dim-128.v1")
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c)));
        assert!(sanitize_embedder_id("").len() <= 128 && !sanitize_embedder_id("").is_empty());
    }

    #[test]
    fn hash_embed_batch_matches_single() {
        let e = HashEmbedder::default();
        let texts = vec![
            "session search wormdb".into(),
            "unrelated pasta recipe".into(),
        ];
        let batch = e.embed_batch(&texts).unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0], e.embed(&texts[0]).unwrap());
        assert_eq!(batch[1], e.embed(&texts[1]).unwrap());
    }

    #[test]
    fn hash_is_deterministic_and_normalized() {
        let e = HashEmbedder::default();
        let a = e.embed("hello wormdb sidecar").unwrap();
        let b = e.embed("hello wormdb sidecar").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), HASH_EMBED_DIM * 4);
        // unit length
        let mut norm = 0f32;
        for chunk in a.chunks_exact(4) {
            let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            norm += v * v;
        }
        assert!((norm.sqrt() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn similar_text_has_higher_cosine_than_unrelated() {
        let e = HashEmbedder::default();
        let a = e
            .embed("global session search wormdb vector index")
            .unwrap();
        let b = e.embed("session search with wormdb vectors").unwrap();
        let c = e.embed("cooking pasta recipes italian food").unwrap();
        let ab = cosine(&a, &b);
        let ac = cosine(&a, &c);
        assert!(ab > ac, "ab={ab} ac={ac}");
    }

    fn cosine(a: &[u8], b: &[u8]) -> f32 {
        let mut dot = 0f32;
        for (ca, cb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            let va = f32::from_le_bytes([ca[0], ca[1], ca[2], ca[3]]);
            let vb = f32::from_le_bytes([cb[0], cb[1], cb[2], cb[3]]);
            dot += va * vb;
        }
        dot
    }
}
