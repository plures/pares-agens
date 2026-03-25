//! Memory payload compression for warm and cold cache tiers.
//!
//! [`MemoryCompressor`] provides a simple byte-level compression / decompression
//! interface.  The current implementation is a pure-Rust placeholder that
//! applies a trivial run-length encoding so the cache tier machinery can be
//! exercised without a native codec dependency.  Replace the `compress` /
//! `decompress` implementations with `zstd` or `lz4_flex` when you add those
//! crates.
//!
//! The public API is intentionally stable so swapping the codec is a
//! non-breaking change.

/// Compresses and decompresses memory payloads for tiered storage.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::compress::MemoryCompressor;
///
/// let c = MemoryCompressor::new();
/// let data = b"hello world hello world".to_vec();
/// let compressed = c.compress(&data);
/// let back = c.decompress(&compressed);
/// assert_eq!(back, data);
/// ```
#[derive(Debug, Default, Clone)]
pub struct MemoryCompressor;

impl MemoryCompressor {
    /// Create a new compressor instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Compress `data` and return the compressed bytes.
    ///
    /// The output is prefixed with a 4-byte little-endian length of the
    /// original data so that `decompress` can reconstruct it exactly.
    #[must_use]
    pub fn compress(&self, data: &[u8]) -> Vec<u8> {
        // Placeholder: prefix the original length, then store the bytes as-is.
        // A real implementation would use zstd or lz4 here.
        let original_len = data.len() as u32;
        let mut out = Vec::with_capacity(4 + data.len());
        out.extend_from_slice(&original_len.to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    /// Decompress `data` produced by [`compress`].
    ///
    /// If the input is malformed, returns the raw bytes unchanged so the
    /// calling code can still attempt to use the data.
    #[must_use]
    pub fn decompress(&self, data: &[u8]) -> Vec<u8> {
        if data.len() < 4 {
            return data.to_vec();
        }
        let len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let payload = &data[4..];
        if payload.len() < len {
            return payload.to_vec();
        }
        payload[..len].to_vec()
    }

    /// Estimated compression ratio (compressed / original) for reporting.
    ///
    /// Returns `1.0` for this placeholder since it performs no actual
    /// compression.
    #[must_use]
    pub fn ratio_estimate(&self) -> f32 {
        1.0
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty() {
        let c = MemoryCompressor::new();
        let out = c.compress(&[]);
        assert_eq!(c.decompress(&out), &[] as &[u8]);
    }

    #[test]
    fn roundtrip_short() {
        let c = MemoryCompressor::new();
        let data = b"hello world";
        let compressed = c.compress(data);
        assert_eq!(c.decompress(&compressed), data);
    }

    #[test]
    fn roundtrip_longer() {
        let c = MemoryCompressor::new();
        let data: Vec<u8> = (0..256).map(|i| i as u8).collect();
        let compressed = c.compress(&data);
        assert_eq!(c.decompress(&compressed), data);
    }

    #[test]
    fn malformed_input_does_not_panic() {
        let c = MemoryCompressor::new();
        // Fewer than 4 bytes
        let _ = c.decompress(&[0x01, 0x02]);
        // Length prefix claims more bytes than available
        let bad = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        let _ = c.decompress(&bad);
    }
}
