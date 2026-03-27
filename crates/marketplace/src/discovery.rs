//! Marketplace discovery — listing and searching for skills/extensions.
//!
//! [`MarketplaceClient`] wraps the marketplace HTTP API and provides
//! methods for listing all available skills and searching by name or
//! [`SkillCategory`].
//!
//! # Stub behaviour
//!
//! The current implementation does not make real network calls.  Instead
//! it operates against an in-memory catalogue so that the full pipeline
//! can be exercised and tested without external infrastructure.  A
//! production implementation would replace the catalogue lookup with
//! authenticated `reqwest` calls.

use crate::{MarketplaceError, SkillCategory, SkillMetadata};

// ── Client ────────────────────────────────────────────────────────────────────

/// Client for the Pares Agens skill marketplace.
///
/// Provides discovery APIs to browse and search the remote catalogue of
/// published skills and extensions.
#[derive(Debug)]
pub struct MarketplaceClient {
    /// Base URL of the marketplace API (e.g. `"https://marketplace.pares.ai"`).
    api_base: String,

    /// In-memory catalogue used when no real network connection is available.
    catalogue: Vec<SkillMetadata>,
}

impl MarketplaceClient {
    /// Create a new `MarketplaceClient` pointing at `api_base`.
    ///
    /// The client starts with an empty catalogue; use
    /// [`with_catalogue`](Self::with_catalogue) or seed it via
    /// [`register_skill`](Self::register_skill) for testing.
    #[must_use]
    pub fn new(api_base: &str) -> Self {
        Self {
            api_base: api_base.to_string(),
            catalogue: Vec::new(),
        }
    }

    /// Return a reference to the configured API base URL.
    #[must_use]
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Seed the client with a pre-built catalogue (useful for testing).
    #[must_use]
    pub fn with_catalogue(mut self, catalogue: Vec<SkillMetadata>) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Register a single skill into the in-memory catalogue.
    pub fn register_skill(&mut self, skill: SkillMetadata) {
        self.catalogue.push(skill);
    }

    /// Return **all** skills available in the marketplace.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::NetworkError`] when the remote API is
    /// unreachable (not triggered by the stub implementation).
    pub fn list_skills(&self) -> Result<Vec<SkillMetadata>, MarketplaceError> {
        Ok(self.catalogue.clone())
    }

    /// Search for skills whose name or description contains `query` (case-
    /// insensitive), optionally filtered by `category`.
    ///
    /// Returns the matching skills in the order they appear in the catalogue.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InvalidMetadata`] when `query` is empty.
    /// Returns [`MarketplaceError::NetworkError`] when the remote API is
    /// unreachable (not triggered by the stub implementation).
    pub fn search(
        &self,
        query: &str,
        category: Option<&SkillCategory>,
    ) -> Result<Vec<SkillMetadata>, MarketplaceError> {
        if query.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "search query must not be empty".to_string(),
            ));
        }

        let lower_query = query.to_lowercase();

        let results = self
            .catalogue
            .iter()
            .filter(|skill| {
                let text_match = skill.name.to_lowercase().contains(&lower_query)
                    || skill.description.to_lowercase().contains(&lower_query);

                let category_match = category
                    .map(|cat| skill.categories.iter().any(|c| category_matches(c, cat)))
                    .unwrap_or(true);

                text_match && category_match
            })
            .cloned()
            .collect();

        Ok(results)
    }

    /// Retrieve metadata for the skill identified by `id`.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::NotFound`] when no skill with the given
    /// `id` exists in the catalogue.
    /// Returns [`MarketplaceError::NetworkError`] when the remote API is
    /// unreachable (not triggered by the stub implementation).
    pub fn get_skill(&self, id: &str) -> Result<SkillMetadata, MarketplaceError> {
        self.catalogue
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| MarketplaceError::NotFound(id.to_string()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns `true` when `candidate` and `filter` represent the same skill
/// category variant (inner values are ignored for broader matching).
fn category_matches(candidate: &SkillCategory, filter: &SkillCategory) -> bool {
    matches!(
        (candidate, filter),
        (SkillCategory::Coding(_), SkillCategory::Coding(_))
            | (SkillCategory::Writing(_), SkillCategory::Writing(_))
            | (SkillCategory::Analysis(_), SkillCategory::Analysis(_))
            | (
                SkillCategory::DomainSpecific(_),
                SkillCategory::DomainSpecific(_)
            )
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::valid_metadata;

    fn rust_skill() -> SkillMetadata {
        valid_metadata()
    }

    fn writing_skill() -> SkillMetadata {
        SkillMetadata {
            id: "pares/essay-writer".to_string(),
            name: "Essay Writer".to_string(),
            version: "2.0.0".to_string(),
            description: "Drafts polished essays in various styles.".to_string(),
            author: "pares".to_string(),
            categories: vec![SkillCategory::Writing("essay".to_string())],
            checksum: "b".repeat(64),
            download_url: "https://marketplace.example.com/skills/essay-writer-2.0.0.tar.gz"
                .to_string(),
            signature: None,
        }
    }

    fn seeded_client() -> MarketplaceClient {
        MarketplaceClient::new("https://marketplace.example.com")
            .with_catalogue(vec![rust_skill(), writing_skill()])
    }

    // ── construction ─────────────────────────────────────────────────────────

    #[test]
    fn new_stores_api_base() {
        let client = MarketplaceClient::new("https://marketplace.example.com");
        assert_eq!(client.api_base(), "https://marketplace.example.com");
    }

    #[test]
    fn new_has_empty_catalogue() {
        let client = MarketplaceClient::new("https://marketplace.example.com");
        assert_eq!(client.list_skills().unwrap().len(), 0);
    }

    // ── list_skills ───────────────────────────────────────────────────────────

    #[test]
    fn list_skills_returns_all_entries() {
        let client = seeded_client();
        let skills = client.list_skills().unwrap();
        assert_eq!(skills.len(), 2);
    }

    #[test]
    fn register_skill_adds_to_catalogue() {
        let mut client = MarketplaceClient::new("https://marketplace.example.com");
        client.register_skill(rust_skill());
        assert_eq!(client.list_skills().unwrap().len(), 1);
    }

    // ── search ────────────────────────────────────────────────────────────────

    #[test]
    fn search_rejects_empty_query() {
        let client = seeded_client();
        assert!(matches!(
            client.search("", None),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn search_finds_by_name() {
        let client = seeded_client();
        let results = client.search("rust", None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "pares/rust-helper");
    }

    #[test]
    fn search_finds_by_description() {
        let client = seeded_client();
        let results = client.search("essays", None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "pares/essay-writer");
    }

    #[test]
    fn search_is_case_insensitive() {
        let client = seeded_client();
        let results = client.search("RUST", None).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_with_category_filter_narrows_results() {
        let client = seeded_client();
        let filter = SkillCategory::Writing("any".to_string());
        let results = client.search("er", Some(&filter)).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "pares/essay-writer");
    }

    #[test]
    fn search_returns_empty_when_no_match() {
        let client = seeded_client();
        let results = client.search("nonexistent-skill-xyz", None).unwrap();
        assert!(results.is_empty());
    }

    // ── get_skill ─────────────────────────────────────────────────────────────

    #[test]
    fn get_skill_returns_matching_entry() {
        let client = seeded_client();
        let skill = client.get_skill("pares/rust-helper").unwrap();
        assert_eq!(skill.name, "Rust Helper");
    }

    #[test]
    fn get_skill_returns_not_found_for_unknown_id() {
        let client = seeded_client();
        assert!(matches!(
            client.get_skill("unknown/skill"),
            Err(MarketplaceError::NotFound(_))
        ));
    }
}
