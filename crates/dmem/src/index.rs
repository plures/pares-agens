//! Embedding index tiers — full-resolution vs compact/quantized.
//!
//! High-capacity devices maintain a [`FullIndex`] backed by float32 vectors.
//! Constrained devices use a [`QuantizedIndex`] where each vector is stored
//! as a binary signature (sign bits of the float32 components), greatly
//! reducing memory while preserving approximate nearest-neighbour results.
//!
//! Both implement [`EmbeddingIndex`] so the rest of the crate is agnostic to
//! the chosen backend.

use crate::error::DmemError;

// ── EmbeddingIndex (trait) ────────────────────────────────────────────────────

/// Trait for a local embedding index.
///
/// Implementors maintain a mapping from memory IDs to embedding vectors (or
/// their compact representations) and answer approximate nearest-neighbour
/// queries.
pub trait EmbeddingIndex: Send + Sync {
    /// Insert or update the embedding for a memory entry.
    fn upsert(&mut self, id: &str, embedding: &[f32]) -> Result<(), DmemError>;

    /// Remove an entry from the index.
    fn remove(&mut self, id: &str);

    /// Return the `top_k` most similar entry IDs for `query`, sorted by
    /// descending similarity.
    ///
    /// Returns an empty `Vec` if the index is empty.
    fn query(&self, query: &[f32], top_k: usize) -> Vec<ScoredEntry>;

    /// The number of entries in the index.
    fn len(&self) -> usize;

    /// `true` if the index holds no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── ScoredEntry ───────────────────────────────────────────────────────────────

/// A search result with its cosine-similarity score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredEntry {
    /// Memory entry ID.
    pub id: String,
    /// Cosine similarity in `[−1, 1]`; higher is more similar.
    pub score: f32,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

/// Binarise a float vector: 1 bit per component (sign bit).
fn binarise(v: &[f32]) -> Vec<u8> {
    let byte_len = v.len().div_ceil(8);
    let mut bits = vec![0u8; byte_len];
    for (i, &x) in v.iter().enumerate() {
        if x >= 0.0 {
            bits[i / 8] |= 1 << (i % 8);
        }
    }
    bits
}

/// Approximate cosine similarity via Hamming distance on binary signatures.
///
/// Only the first `dim` bits of the byte vectors are considered to avoid
/// counting padding bits beyond the actual embedding dimension.
fn binary_similarity(a: &[u8], b: &[u8], dim: usize) -> f32 {
    let mut matching: u32 = 0;
    for bit_idx in 0..dim {
        let byte_idx = bit_idx / 8;
        let bit_pos = bit_idx % 8;
        let a_bit = (a.get(byte_idx).copied().unwrap_or(0) >> bit_pos) & 1;
        let b_bit = (b.get(byte_idx).copied().unwrap_or(0) >> bit_pos) & 1;
        if a_bit == b_bit {
            matching += 1;
        }
    }
    // Rescale from [0, dim] matching bits to [-1, 1]
    2.0 * matching as f32 / (dim as f32).max(1.0) - 1.0
}

// ── FullIndex ─────────────────────────────────────────────────────────────────

/// Full-resolution float32 embedding index.
///
/// Linear scan, suitable for up to ~100k entries on desktop-class hardware.
/// For larger corpora a real HNSW or IVF index (e.g. `usearch`) should be
/// plugged in behind the same trait.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::index::{FullIndex, EmbeddingIndex};
///
/// let mut idx = FullIndex::new();
/// idx.upsert("a", &[1.0, 0.0]).unwrap();
/// idx.upsert("b", &[0.0, 1.0]).unwrap();
/// let results = idx.query(&[1.0, 0.1], 1);
/// assert_eq!(results[0].id, "a");
/// ```
#[derive(Debug, Default)]
pub struct FullIndex {
    entries: Vec<(String, Vec<f32>)>,
}

impl FullIndex {
    /// Create an empty full-resolution index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl EmbeddingIndex for FullIndex {
    fn upsert(&mut self, id: &str, embedding: &[f32]) -> Result<(), DmemError> {
        if let Some(pos) = self.entries.iter().position(|(eid, _)| eid == id) {
            self.entries[pos].1 = embedding.to_vec();
        } else {
            self.entries.push((id.to_owned(), embedding.to_vec()));
        }
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        self.entries.retain(|(eid, _)| eid != id);
    }

    fn query(&self, query: &[f32], top_k: usize) -> Vec<ScoredEntry> {
        let mut scored: Vec<ScoredEntry> = self
            .entries
            .iter()
            .map(|(id, emb)| ScoredEntry {
                id: id.clone(),
                score: cosine_similarity(query, emb),
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

// ── QuantizedIndex ────────────────────────────────────────────────────────────

/// Compact binary-quantized embedding index for constrained devices.
///
/// Stores each embedding as a bit-vector (one bit per dimension), dramatically
/// reducing memory usage at the cost of some recall accuracy.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::index::{QuantizedIndex, EmbeddingIndex};
///
/// let mut idx = QuantizedIndex::new(2);
/// idx.upsert("a", &[1.0, 0.0]).unwrap();
/// idx.upsert("b", &[0.0, 1.0]).unwrap();
/// let results = idx.query(&[1.0, 0.1], 1);
/// assert_eq!(results[0].id, "a");
/// ```
#[derive(Debug)]
pub struct QuantizedIndex {
    dim: usize,
    entries: Vec<(String, Vec<u8>)>,
}

impl QuantizedIndex {
    /// Create an empty quantized index for `dim`-dimensional embeddings.
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            entries: Vec::new(),
        }
    }
}

impl EmbeddingIndex for QuantizedIndex {
    fn upsert(&mut self, id: &str, embedding: &[f32]) -> Result<(), DmemError> {
        let bits = binarise(embedding);
        if let Some(pos) = self.entries.iter().position(|(eid, _)| eid == id) {
            self.entries[pos].1 = bits;
        } else {
            self.entries.push((id.to_owned(), bits));
        }
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        self.entries.retain(|(eid, _)| eid != id);
    }

    fn query(&self, query: &[f32], top_k: usize) -> Vec<ScoredEntry> {
        let query_bits = binarise(query);
        let mut scored: Vec<ScoredEntry> = self
            .entries
            .iter()
            .map(|(id, bits)| ScoredEntry {
                id: id.clone(),
                score: binary_similarity(&query_bits, bits, self.dim),
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_index_upsert_and_query() {
        let mut idx = FullIndex::new();
        idx.upsert("a", &[1.0, 0.0]).unwrap();
        idx.upsert("b", &[0.0, 1.0]).unwrap();
        let r = idx.query(&[1.0, 0.1], 1);
        assert_eq!(r[0].id, "a");
        assert!(r[0].score > 0.9);
    }

    #[test]
    fn full_index_upsert_updates_existing() {
        let mut idx = FullIndex::new();
        idx.upsert("a", &[1.0, 0.0]).unwrap();
        idx.upsert("a", &[0.0, 1.0]).unwrap();
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn full_index_remove() {
        let mut idx = FullIndex::new();
        idx.upsert("a", &[1.0, 0.0]).unwrap();
        idx.remove("a");
        assert!(idx.is_empty());
    }

    #[test]
    fn quantized_index_basic() {
        let mut idx = QuantizedIndex::new(2);
        idx.upsert("a", &[1.0, 0.0]).unwrap();
        idx.upsert("b", &[0.0, 1.0]).unwrap();
        let r = idx.query(&[1.0, 0.1], 1);
        assert_eq!(r[0].id, "a");
    }

    #[test]
    fn quantized_index_remove() {
        let mut idx = QuantizedIndex::new(2);
        idx.upsert("x", &[1.0, 1.0]).unwrap();
        idx.remove("x");
        assert!(idx.is_empty());
    }

    #[test]
    fn full_index_top_k_bounded() {
        let mut idx = FullIndex::new();
        for i in 0..10 {
            idx.upsert(&format!("e{i}"), &[i as f32, 0.0]).unwrap();
        }
        let r = idx.query(&[5.0, 0.0], 3);
        assert_eq!(r.len(), 3);
    }
}
