use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::Error;

/// Dimensionality of BAAI/bge-small-en-v1.5 embeddings.
pub const EMBEDDING_DIM: usize = 384;

/// Trait for text embedding providers.
///
/// In production this will be backed by a PluresDB embedding pipeline running
/// BAAI/bge-small-en-v1.5. In tests, use [`MockEmbedder`].
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Produce a unit-normalised embedding vector for `text`.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, Error>;

    /// Number of dimensions returned by this provider.
    fn dimensions(&self) -> usize;
}

/// Deterministic mock embedder for unit and integration tests.
///
/// Uses character bigram frequencies to produce stable 384-dimensional vectors.
/// Two texts that share many character bigrams will have higher cosine similarity,
/// providing meaningful relevance ordering in tests without a real model.
pub struct MockEmbedder;

#[async_trait]
impl EmbeddingProvider for MockEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        let bytes: Vec<u8> = text.to_lowercase().bytes().collect();

        // Character bigrams — primary signal
        for window in bytes.windows(2) {
            let idx = (window[0] as usize)
                .wrapping_mul(31)
                .wrapping_add(window[1] as usize)
                % EMBEDDING_DIM;
            v[idx] += 1.0;
        }
        // Single bytes — secondary signal
        for &b in &bytes {
            v[b as usize % EMBEDDING_DIM] += 0.5;
        }

        // L2 normalise
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        Ok(v)
    }

    fn dimensions(&self) -> usize {
        EMBEDDING_DIM
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible embedder
// ---------------------------------------------------------------------------

/// OpenAI-compatible embedding client (works with any OpenAI-compatible endpoint).
pub struct OpenAiEmbedder {
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiEmbedder {
    /// Create a new OpenAI-compatible embedding client.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Debug, Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        let url = format!("{}/v1/embeddings", self.base_url);
        let mut req = self
            .client
            .post(&url)
            .json(&EmbeddingRequest {
                model: &self.model,
                input: text,
            });

        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Embed(format!("embeddings request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Embed(format!(
                "embeddings request failed: {status} {body}"
            )));
        }

        let parsed: EmbeddingResponse = resp
            .json()
            .await
            .map_err(|e| Error::Embed(format!("invalid embeddings response: {e}")))?;

        let mut embedding = parsed
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| Error::Embed("embeddings response missing data".into()))?;

        // L2 normalise for cosine similarity.
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            embedding.iter_mut().for_each(|x| *x /= norm);
        }

        Ok(embedding)
    }

    fn dimensions(&self) -> usize {
        EMBEDDING_DIM
    }
}
