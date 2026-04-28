//! LoRA adapter packaging and marketplace distribution.
//!
//! [`Marketplace`] provides the full lifecycle for distributing LoRA adapters:
//! packaging adapter weights with metadata and benchmarks, creating marketplace
//! listings via Polar.sh integration (stub), and searching published adapters.
//!
//! # Stub behaviour
//!
//! The current implementation does not make real network calls to Polar.sh nor
//! perform actual filesystem I/O.  It maintains an in-memory catalogue of
//! published listings so the full pipeline can be exercised and tested without
//! external infrastructure.  A production implementation would replace the
//! stub weight encoding with real file reads and the `create_listing` stub
//! with authenticated Polar.sh API calls.

use crate::MarketplaceError;
use crate::lora_types::LoRAAdapter;
use serde::{Deserialize, Serialize};

// ── Adapter metadata ──────────────────────────────────────────────────────────

/// Metadata describing a packaged LoRA adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterMetadata {
    /// Human-readable name for this adapter.
    pub name: String,

    /// Semantic version string (e.g. `"1.0.0"`).
    pub version: String,

    /// Identifier for the base model this adapter was trained on.
    pub base_model: String,

    /// Short description of the adapter's capabilities.
    pub description: String,

    /// Author or publisher of the adapter.
    pub author: String,

    /// The LoRA rank used when training this adapter.
    pub lora_rank: u16,

    /// Number of training epochs completed.
    pub epochs_trained: u32,
}

// ── Benchmark results ─────────────────────────────────────────────────────────

/// Performance benchmark results for a trained LoRA adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResults {
    /// Perplexity on the evaluation split (lower is better).
    pub perplexity: f32,

    /// Accuracy on classification tasks in the evaluation set (0.0–1.0).
    pub accuracy: f32,

    /// Number of examples used for benchmarking.
    pub num_examples: usize,
}

// ── Adapter package ───────────────────────────────────────────────────────────

/// A distributable package containing adapter weights, metadata, benchmarks,
/// and sample outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterPackage {
    /// Raw adapter weight bytes.
    pub weights: Vec<u8>,

    /// Metadata describing this adapter.
    pub metadata: AdapterMetadata,

    /// Benchmark results for this adapter.
    pub benchmarks: BenchmarkResults,

    /// Sample model outputs demonstrating this adapter's capabilities.
    pub sample_outputs: Vec<String>,
}

// ── Marketplace listing ───────────────────────────────────────────────────────

/// A marketplace listing for a LoRA adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceListing {
    /// Display title for the listing.
    pub title: String,

    /// Detailed description of what the adapter does.
    pub description: String,

    /// Price in cents (e.g. `500` = $5.00). Use `0` for free adapters.
    pub price: u64,

    /// Skills or domains covered by this adapter (e.g. `["rust", "systems"]`).
    pub skills: Vec<String>,

    /// Primary natural language of the adapter's training data.
    pub language: String,
}

// ── Stub constants ────────────────────────────────────────────────────────────

/// Placeholder base model identifier used when none is known.
const UNKNOWN_BASE_MODEL: &str = "unknown";

/// Placeholder author used when none is known.
const UNKNOWN_AUTHOR: &str = "unknown";

/// Stub perplexity returned when real evaluation is not available.
const STUB_PERPLEXITY: f32 = 10.0;

/// Stub accuracy returned when real evaluation is not available.
const STUB_ACCURACY: f32 = 0.85;

/// Stub number of evaluation examples used when real evaluation is not available.
const STUB_NUM_EXAMPLES: usize = 100;

// ── Marketplace ───────────────────────────────────────────────────────────────

/// Manages LoRA adapter packaging and marketplace listings via Polar.sh.
///
/// In production the `create_listing` method would submit the package to
/// the Polar.sh product API and return the resulting product ID.  The
/// current stub maintains an in-memory catalogue so the full workflow can
/// be exercised without external infrastructure.
#[derive(Debug, Default)]
pub struct Marketplace {
    /// Published listings indexed by their unique listing ID.
    listings: Vec<(String, MarketplaceListing)>,

    /// Internal counter used to generate deterministic listing IDs.
    next_id: u64,
}

impl Marketplace {
    /// Create a new, empty `Marketplace`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Package a [`LoRAAdapter`] into a distributable [`AdapterPackage`].
    ///
    /// Reads the adapter path as the weight source (stub: encodes the path
    /// string as UTF-8 bytes so tests can run without real weight files).
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::PackagingFailed`] when
    /// `adapter.adapter_path` is empty.
    pub fn package_adapter(
        &self,
        adapter: &LoRAAdapter,
    ) -> Result<AdapterPackage, MarketplaceError> {
        if adapter.adapter_path.is_empty() {
            return Err(MarketplaceError::PackagingFailed(
                "adapter_path must not be empty".to_string(),
            ));
        }

        let metadata = AdapterMetadata {
            name: format!("lora-rank-{}", adapter.lora_rank),
            version: "1.0.0".to_string(),
            base_model: UNKNOWN_BASE_MODEL.to_string(),
            description: format!(
                "LoRA adapter with rank {} trained for {} epochs",
                adapter.lora_rank, adapter.epochs_trained
            ),
            author: UNKNOWN_AUTHOR.to_string(),
            lora_rank: adapter.lora_rank,
            epochs_trained: adapter.epochs_trained,
        };

        // Stub: encode the adapter path as the weight bytes so tests run
        // without real weight files on disk.
        let weights = adapter.adapter_path.as_bytes().to_vec();

        let benchmarks = BenchmarkResults {
            perplexity: STUB_PERPLEXITY,
            accuracy: STUB_ACCURACY,
            num_examples: STUB_NUM_EXAMPLES,
        };

        Ok(AdapterPackage {
            weights,
            metadata,
            benchmarks,
            sample_outputs: Vec::new(),
        })
    }

    /// Create a marketplace listing for an [`AdapterPackage`].
    ///
    /// Validates the listing fields and registers it in the internal catalogue.
    /// Returns the unique listing ID (Polar.sh product ID in production).
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InvalidMetadata`] when required listing
    /// fields (`title`, `description`, `language`) are empty.
    pub fn create_listing(
        &mut self,
        _package: AdapterPackage,
        listing: MarketplaceListing,
    ) -> Result<String, MarketplaceError> {
        if listing.title.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "listing title must not be empty".to_string(),
            ));
        }
        if listing.description.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "listing description must not be empty".to_string(),
            ));
        }
        if listing.language.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "listing language must not be empty".to_string(),
            ));
        }

        let id = format!("listing-{}", self.next_id);
        self.next_id += 1;
        self.listings.push((id.clone(), listing));
        Ok(id)
    }

    /// Search published adapter listings.
    ///
    /// Returns listings whose `title`, `description`, or `skills` contain
    /// `query` (case-insensitive).
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InvalidMetadata`] when `query` is empty.
    pub fn search_adapters(
        &self,
        query: &str,
    ) -> Result<Vec<MarketplaceListing>, MarketplaceError> {
        if query.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "search query must not be empty".to_string(),
            ));
        }

        let lower = query.to_lowercase();
        let results = self
            .listings
            .iter()
            .filter(|(_, l)| {
                l.title.to_lowercase().contains(&lower)
                    || l.description.to_lowercase().contains(&lower)
                    || l.skills.iter().any(|s| s.to_lowercase().contains(&lower))
            })
            .map(|(_, l)| l.clone())
            .collect();

        Ok(results)
    }

    /// Return the number of published listings.
    #[must_use]
    pub fn listing_count(&self) -> usize {
        self.listings.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lora_types::LoRAAdapter;

    fn sample_adapter() -> LoRAAdapter {
        LoRAAdapter {
            adapter_path: "lora-rank16-adapter".to_string(),
            lora_rank: 16,
            epochs_trained: 3,
        }
    }

    fn sample_listing(title: &str) -> MarketplaceListing {
        MarketplaceListing {
            title: title.to_string(),
            description: "A helpful Rust coding adapter.".to_string(),
            price: 500,
            skills: vec!["rust".to_string(), "systems".to_string()],
            language: "en".to_string(),
        }
    }

    // ── package_adapter ───────────────────────────────────────────────────────

    #[test]
    fn package_adapter_succeeds_for_valid_adapter() {
        let marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        assert!(!pkg.weights.is_empty());
        assert_eq!(pkg.metadata.lora_rank, 16);
        assert_eq!(pkg.metadata.epochs_trained, 3);
    }

    #[test]
    fn package_adapter_rejects_empty_path() {
        let marketplace = Marketplace::new();
        let adapter = LoRAAdapter {
            adapter_path: String::new(),
            lora_rank: 16,
            epochs_trained: 3,
        };
        assert!(matches!(
            marketplace.package_adapter(&adapter),
            Err(MarketplaceError::PackagingFailed(_))
        ));
    }

    #[test]
    fn package_adapter_sets_benchmark_results() {
        let marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        assert!(pkg.benchmarks.perplexity > 0.0);
        assert!((0.0..=1.0).contains(&pkg.benchmarks.accuracy));
        assert!(pkg.benchmarks.num_examples > 0);
    }

    #[test]
    fn package_adapter_encodes_path_as_weights() {
        let marketplace = Marketplace::new();
        let adapter = sample_adapter();
        let pkg = marketplace.package_adapter(&adapter).unwrap();
        assert_eq!(pkg.weights, adapter.adapter_path.as_bytes());
    }

    #[test]
    fn package_adapter_metadata_includes_rank_in_name() {
        let marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        assert_eq!(pkg.metadata.name, "lora-rank-16");
    }

    // ── create_listing ────────────────────────────────────────────────────────

    #[test]
    fn create_listing_returns_unique_ids() {
        let mut marketplace = Marketplace::new();
        let adapter = sample_adapter();
        let pkg1 = marketplace.package_adapter(&adapter).unwrap();
        let pkg2 = marketplace.package_adapter(&adapter).unwrap();
        let id1 = marketplace
            .create_listing(pkg1, sample_listing("Rust Helper"))
            .unwrap();
        let id2 = marketplace
            .create_listing(pkg2, sample_listing("Rust Helper v2"))
            .unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn create_listing_rejects_empty_title() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        let mut listing = sample_listing("Rust Helper");
        listing.title = String::new();
        assert!(matches!(
            marketplace.create_listing(pkg, listing),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn create_listing_rejects_empty_description() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        let mut listing = sample_listing("Rust Helper");
        listing.description = String::new();
        assert!(matches!(
            marketplace.create_listing(pkg, listing),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn create_listing_rejects_empty_language() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        let mut listing = sample_listing("Rust Helper");
        listing.language = String::new();
        assert!(matches!(
            marketplace.create_listing(pkg, listing),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn create_listing_increments_catalogue() {
        let mut marketplace = Marketplace::new();
        let adapter = sample_adapter();
        let pkg = marketplace.package_adapter(&adapter).unwrap();
        marketplace
            .create_listing(pkg, sample_listing("Rust Helper"))
            .unwrap();
        assert_eq!(marketplace.listing_count(), 1);
    }

    #[test]
    fn create_listing_accepts_free_adapter() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        let mut listing = sample_listing("Free Rust Helper");
        listing.price = 0;
        assert!(marketplace.create_listing(pkg, listing).is_ok());
    }

    // ── search_adapters ───────────────────────────────────────────────────────

    #[test]
    fn search_adapters_rejects_empty_query() {
        let marketplace = Marketplace::new();
        assert!(matches!(
            marketplace.search_adapters(""),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn search_adapters_finds_by_title() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        marketplace
            .create_listing(pkg, sample_listing("Rust Helper"))
            .unwrap();
        let results = marketplace.search_adapters("Rust").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust Helper");
    }

    #[test]
    fn search_adapters_finds_by_description() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        marketplace
            .create_listing(pkg, sample_listing("Code Helper"))
            .unwrap();
        let results = marketplace.search_adapters("coding adapter").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_adapters_finds_by_skill() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        marketplace
            .create_listing(pkg, sample_listing("Systems Adapter"))
            .unwrap();
        let results = marketplace.search_adapters("systems").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_adapters_is_case_insensitive() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        marketplace
            .create_listing(pkg, sample_listing("Rust Helper"))
            .unwrap();
        let results = marketplace.search_adapters("RUST").unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn search_adapters_returns_empty_when_no_match() {
        let mut marketplace = Marketplace::new();
        let pkg = marketplace.package_adapter(&sample_adapter()).unwrap();
        marketplace
            .create_listing(pkg, sample_listing("Rust Helper"))
            .unwrap();
        let results = marketplace.search_adapters("python").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_adapters_returns_multiple_matches() {
        let mut marketplace = Marketplace::new();
        let adapter = sample_adapter();
        let pkg1 = marketplace.package_adapter(&adapter).unwrap();
        let pkg2 = marketplace.package_adapter(&adapter).unwrap();
        marketplace
            .create_listing(pkg1, sample_listing("Rust Async Helper"))
            .unwrap();
        marketplace
            .create_listing(pkg2, sample_listing("Rust Systems Helper"))
            .unwrap();
        let results = marketplace.search_adapters("rust").unwrap();
        assert_eq!(results.len(), 2);
    }
}
