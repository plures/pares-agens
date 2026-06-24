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
// Unit tests for MockEmbedder
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }

    #[tokio::test]
    async fn mock_embedder_returns_correct_dimensions() {
        let embedder = MockEmbedder;
        let v = embedder.embed("hello world").await.unwrap();
        assert_eq!(v.len(), EMBEDDING_DIM);
        assert_eq!(embedder.dimensions(), EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn mock_embedder_deterministic() {
        let embedder = MockEmbedder;
        let v1 = embedder.embed("hello world").await.unwrap();
        let v2 = embedder.embed("hello world").await.unwrap();
        assert_eq!(v1, v2);
    }

    #[tokio::test]
    async fn mock_embedder_produces_unit_vector() {
        let embedder = MockEmbedder;
        let v = embedder.embed("test normalization").await.unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "vector not unit-normalized: norm={norm}"
        );
    }

    #[tokio::test]
    async fn mock_embedder_all_values_non_negative() {
        // The MockEmbedder only adds to indices (+=), so all values should be >= 0
        // before normalization. After normalization they stay >= 0.
        let embedder = MockEmbedder;
        let v = embedder.embed("testing positive values").await.unwrap();
        for (i, &val) in v.iter().enumerate() {
            assert!(val >= 0.0, "expected non-negative at index {i}, got {val}");
        }
    }

    #[tokio::test]
    async fn mock_embedder_similar_texts_higher_similarity() {
        let embedder = MockEmbedder;
        let v_cat = embedder.embed("the cat sat on the mat").await.unwrap();
        let v_cat2 = embedder.embed("the cat sat on the hat").await.unwrap();
        let v_unrelated = embedder.embed("quantum physics experiment").await.unwrap();

        let sim_similar = cosine_similarity(&v_cat, &v_cat2);
        let sim_different = cosine_similarity(&v_cat, &v_unrelated);
        assert!(
            sim_similar > sim_different,
            "similar texts should have higher cosine similarity: {sim_similar} vs {sim_different}"
        );
    }

    #[tokio::test]
    async fn mock_embedder_different_texts_different_vectors() {
        let embedder = MockEmbedder;
        let v1 = embedder.embed("hello world").await.unwrap();
        let v2 = embedder.embed("completely different text").await.unwrap();
        assert_ne!(v1, v2);
    }

    #[tokio::test]
    async fn mock_embedder_dimensions_matches_constant() {
        let embedder = MockEmbedder;
        assert_eq!(embedder.dimensions(), 384);
    }

    #[tokio::test]
    async fn mock_embedder_empty_text_still_valid() {
        // Empty string should still produce a valid vector (all zeros normalized = all zeros)
        let embedder = MockEmbedder;
        let v = embedder.embed("").await.unwrap();
        assert_eq!(v.len(), EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn mock_embedder_norm_computation_correct() {
        // Verify the norm computation: sum of squares then sqrt
        let embedder = MockEmbedder;
        let v = embedder.embed("norm test").await.unwrap();
        let norm_sq: f32 = v.iter().map(|x| x * x).sum();
        // For a unit vector, norm^2 should be ~1.0
        // For empty string it's 0.0
        if norm_sq > 0.0 {
            assert!(
                (norm_sq - 1.0).abs() < 1e-4,
                "norm squared should be ~1.0, got {norm_sq}"
            );
        }
    }

    #[tokio::test]
    async fn mock_embedder_bigram_contribution_matters() {
        // Test that the bigram window computation actually contributes to the vector.
        // "ab" and "ba" should hit different indices due to wrapping_mul ordering.
        let embedder = MockEmbedder;
        let v_ab = embedder.embed("ab").await.unwrap();
        let v_ba = embedder.embed("ba").await.unwrap();
        assert_ne!(
            v_ab, v_ba,
            "different bigrams should produce different vectors"
        );
    }

    #[tokio::test]
    async fn mock_embedder_single_char_contribution() {
        // Single characters should still produce non-zero vectors from the byte signal
        let embedder = MockEmbedder;
        let v = embedder.embed("x").await.unwrap();
        // Single char = no bigrams, but single-byte signal at index (b'x' % 384)
        let nonzero_count = v.iter().filter(|&&x| x != 0.0).count();
        assert!(
            nonzero_count > 0,
            "single char should produce at least one non-zero element"
        );
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
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
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
        let mut req = self.client.post(&url).json(&EmbeddingRequest {
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

// ---------------------------------------------------------------------------
// OpenAI embedder unit tests (wiremock)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod openai_tests {
    use super::*;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_embedding_response(embedding: Vec<f32>) -> serde_json::Value {
        serde_json::json!({
            "data": [{
                "embedding": embedding,
                "index": 0
            }],
            "model": "test-model",
            "usage": {"prompt_tokens": 5, "total_tokens": 5}
        })
    }

    #[tokio::test]
    async fn openai_embedder_success() {
        let server = MockServer::start().await;
        let embedding = vec![0.5; EMBEDDING_DIM];
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(make_embedding_response(embedding)),
            )
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(server.uri(), "test-model", Some("sk-test".into()));
        let result = embedder.embed("hello").await.unwrap();
        assert_eq!(result.len(), EMBEDDING_DIM);
        let norm: f32 = result.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[tokio::test]
    async fn openai_embedder_sends_auth_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(header_exists("authorization"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_embedding_response(vec![1.0; EMBEDDING_DIM])),
            )
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(server.uri(), "test-model", Some("sk-key".into()));
        let result = embedder.embed("test").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn openai_embedder_no_auth_when_none() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_embedding_response(vec![0.3; EMBEDDING_DIM])),
            )
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(server.uri(), "test-model", None);
        let result = embedder.embed("test").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn openai_embedder_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(server.uri(), "test-model", None);
        let result = embedder.embed("test").await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("500"),
            "error should contain status code: {err_msg}"
        );
    }

    #[tokio::test]
    async fn openai_embedder_empty_data_array() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data": [], "model": "test", "usage": {"prompt_tokens": 0, "total_tokens": 0}}),
            ))
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(server.uri(), "test-model", None);
        let result = embedder.embed("test").await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("missing data"));
    }

    #[tokio::test]
    async fn openai_embedder_invalid_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(server.uri(), "test-model", None);
        let result = embedder.embed("test").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn openai_embedder_normalizes_output() {
        let server = MockServer::start().await;
        let mut full = vec![0.0; EMBEDDING_DIM];
        full[0] = 3.0;
        full[1] = 4.0;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(make_embedding_response(full)))
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(server.uri(), "test-model", None);
        let result = embedder.embed("test").await.unwrap();
        assert!((result[0] - 0.6).abs() < 1e-5);
        assert!((result[1] - 0.8).abs() < 1e-5);
    }

    #[tokio::test]
    async fn openai_embedder_dimensions() {
        let embedder = OpenAiEmbedder::new("http://localhost:9999", "m", None);
        assert_eq!(embedder.dimensions(), EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn openai_embedder_trims_trailing_slash() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_embedding_response(vec![1.0; EMBEDDING_DIM])),
            )
            .mount(&server)
            .await;

        let embedder = OpenAiEmbedder::new(format!("{}/", server.uri()), "model", None);
        let result = embedder.embed("test").await;
        assert!(result.is_ok());
    }
}
