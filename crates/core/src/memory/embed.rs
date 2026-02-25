use async_trait::async_trait;

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
