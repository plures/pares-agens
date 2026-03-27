//! Rating and review system for marketplace procedures.
//!
//! [`RatingStore`] maintains a collection of [`Review`] entries submitted by
//! users.  Aggregated scores are computed on demand by
//! [`RatingStore::aggregate`], which returns a [`RatingAggregate`] with the
//! mean score and total review count.
//!
//! # Stub behaviour
//!
//! Reviews are stored in memory.  A production implementation would persist
//! them to PluresDB and synchronise aggregates with the remote marketplace
//! catalogue.

use crate::MarketplaceError;
use serde::{Deserialize, Serialize};

// ── Star rating ───────────────────────────────────────────────────────────────

/// A 1–5 star rating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StarRating(u8);

impl StarRating {
    /// Create a new [`StarRating`].
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InvalidMetadata`] when `stars` is outside
    /// the range 1–5.
    pub fn new(stars: u8) -> Result<Self, MarketplaceError> {
        if stars == 0 || stars > 5 {
            return Err(MarketplaceError::InvalidMetadata(format!(
                "star rating must be 1–5, got {stars}"
            )));
        }
        Ok(Self(stars))
    }

    /// Return the raw star value (1–5).
    #[must_use]
    pub fn value(self) -> u8 {
        self.0
    }
}

// ── Review ────────────────────────────────────────────────────────────────────

/// A user review for an installed procedure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    /// The procedure being reviewed (e.g. `"pares/rust-helper"`).
    pub procedure_id: String,

    /// The reviewer's handle or identifier.
    pub reviewer: String,

    /// Star rating (1–5).
    pub rating: StarRating,

    /// Optional written review text.
    pub comment: Option<String>,

    /// ISO 8601 timestamp of when the review was submitted.
    pub submitted_at: String,
}

// ── RatingAggregate ───────────────────────────────────────────────────────────

/// Aggregated rating statistics for a single procedure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatingAggregate {
    /// The procedure this aggregate applies to.
    pub procedure_id: String,

    /// Mean star rating across all reviews (0.0 when there are no reviews).
    pub mean_score: f64,

    /// Total number of reviews.
    pub review_count: usize,
}

// ── RatingStore ───────────────────────────────────────────────────────────────

/// In-memory store for procedure reviews and aggregated ratings.
#[derive(Debug, Default)]
pub struct RatingStore {
    reviews: Vec<Review>,
}

impl RatingStore {
    /// Create a new, empty [`RatingStore`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Submit a [`Review`] for a procedure.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InvalidMetadata`] when `procedure_id` or
    /// `reviewer` is empty.
    pub fn submit(&mut self, review: Review) -> Result<(), MarketplaceError> {
        if review.procedure_id.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "procedure_id must not be empty".to_string(),
            ));
        }
        if review.reviewer.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "reviewer must not be empty".to_string(),
            ));
        }
        self.reviews.push(review);
        Ok(())
    }

    /// Return all reviews for `procedure_id`.
    #[must_use]
    pub fn reviews_for(&self, procedure_id: &str) -> Vec<&Review> {
        self.reviews
            .iter()
            .filter(|r| r.procedure_id == procedure_id)
            .collect()
    }

    /// Return a [`RatingAggregate`] for `procedure_id`.
    ///
    /// `mean_score` is `0.0` when there are no reviews.
    #[must_use]
    pub fn aggregate(&self, procedure_id: &str) -> RatingAggregate {
        let relevant: Vec<_> = self.reviews_for(procedure_id);
        let review_count = relevant.len();
        let mean_score = if review_count == 0 {
            0.0
        } else {
            let total: u64 = relevant.iter().map(|r| u64::from(r.rating.value())).sum();
            total as f64 / review_count as f64
        };
        RatingAggregate {
            procedure_id: procedure_id.to_string(),
            mean_score,
            review_count,
        }
    }

    /// Return the total number of reviews across all procedures.
    #[must_use]
    pub fn total_reviews(&self) -> usize {
        self.reviews.len()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Produce a placeholder submission timestamp.
///
/// A production implementation would use the system clock (`chrono::Utc::now`).
#[must_use]
pub fn review_timestamp() -> String {
    "2026-01-01T00:00:00Z".to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_review(procedure_id: &str, reviewer: &str, stars: u8) -> Review {
        Review {
            procedure_id: procedure_id.to_string(),
            reviewer: reviewer.to_string(),
            rating: StarRating::new(stars).unwrap(),
            comment: None,
            submitted_at: review_timestamp(),
        }
    }

    // ── StarRating ────────────────────────────────────────────────────────────

    #[test]
    fn star_rating_accepts_one_to_five() {
        for s in 1u8..=5 {
            assert!(StarRating::new(s).is_ok());
        }
    }

    #[test]
    fn star_rating_rejects_zero() {
        assert!(matches!(
            StarRating::new(0),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn star_rating_rejects_six() {
        assert!(matches!(
            StarRating::new(6),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn star_rating_value_roundtrips() {
        let r = StarRating::new(4).unwrap();
        assert_eq!(r.value(), 4);
    }

    // ── RatingStore::submit ───────────────────────────────────────────────────

    #[test]
    fn submit_accepts_valid_review() {
        let mut store = RatingStore::new();
        let review = make_review("pares/rust-helper", "alice", 5);
        assert!(store.submit(review).is_ok());
        assert_eq!(store.total_reviews(), 1);
    }

    #[test]
    fn submit_rejects_empty_procedure_id() {
        let mut store = RatingStore::new();
        let review = make_review("", "alice", 5);
        assert!(matches!(
            store.submit(review),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn submit_rejects_empty_reviewer() {
        let mut store = RatingStore::new();
        let review = make_review("pares/rust-helper", "", 5);
        assert!(matches!(
            store.submit(review),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    // ── RatingStore::reviews_for ──────────────────────────────────────────────

    #[test]
    fn reviews_for_returns_matching_entries() {
        let mut store = RatingStore::new();
        store
            .submit(make_review("pares/rust-helper", "alice", 5))
            .unwrap();
        store
            .submit(make_review("pares/rust-helper", "bob", 3))
            .unwrap();
        store
            .submit(make_review("pares/essay-writer", "carol", 4))
            .unwrap();
        let reviews = store.reviews_for("pares/rust-helper");
        assert_eq!(reviews.len(), 2);
    }

    #[test]
    fn reviews_for_returns_empty_for_unknown_procedure() {
        let store = RatingStore::new();
        assert!(store.reviews_for("unknown/proc").is_empty());
    }

    // ── RatingStore::aggregate ────────────────────────────────────────────────

    #[test]
    fn aggregate_mean_is_zero_with_no_reviews() {
        let store = RatingStore::new();
        let agg = store.aggregate("pares/rust-helper");
        assert_eq!(agg.mean_score, 0.0);
        assert_eq!(agg.review_count, 0);
    }

    #[test]
    fn aggregate_computes_correct_mean() {
        let mut store = RatingStore::new();
        store
            .submit(make_review("pares/rust-helper", "alice", 4))
            .unwrap();
        store
            .submit(make_review("pares/rust-helper", "bob", 2))
            .unwrap();
        let agg = store.aggregate("pares/rust-helper");
        assert!((agg.mean_score - 3.0).abs() < f64::EPSILON);
        assert_eq!(agg.review_count, 2);
    }

    #[test]
    fn aggregate_only_counts_reviews_for_that_procedure() {
        let mut store = RatingStore::new();
        store
            .submit(make_review("pares/rust-helper", "alice", 5))
            .unwrap();
        store
            .submit(make_review("pares/essay-writer", "bob", 1))
            .unwrap();
        let agg = store.aggregate("pares/rust-helper");
        assert_eq!(agg.review_count, 1);
        assert!((agg.mean_score - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn aggregate_procedure_id_matches_input() {
        let store = RatingStore::new();
        let agg = store.aggregate("pares/foo");
        assert_eq!(agg.procedure_id, "pares/foo");
    }
}
