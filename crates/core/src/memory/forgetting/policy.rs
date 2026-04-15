//! Retention policies — per-category rules that govern memory lifetime.
//!
//! A [`RetentionPolicy`] maps each [`MemoryCategory`] to a [`RetentionRule`].
//! Rules are consulted by [`super::engine::ForgettingEngine`] during dry-runs
//! and live purge passes to decide which entries are eligible for removal.

use std::collections::HashMap;

use crate::memory::entry::MemoryCategory;

// ---------------------------------------------------------------------------
// RetentionRule
// ---------------------------------------------------------------------------

/// Governs how long (and how many) memories in a category should be kept.
///
/// Both fields are optional.  When both are `Some`, **either** limit being
/// exceeded marks an entry as eligible for purge.
#[derive(Debug, Clone, PartialEq)]
pub struct RetentionRule {
    /// Maximum age (in days) before an entry is eligible for purge.
    ///
    /// `None` means no age-based expiry.
    pub max_age_days: Option<u64>,
    /// Maximum number of entries to retain for this category.
    ///
    /// When the store holds more than this many entries in the category,
    /// the oldest excess entries are eligible for purge.
    ///
    /// `None` means no count-based limit.
    pub max_count: Option<usize>,
}

impl RetentionRule {
    /// No restrictions — keep memories forever.
    pub fn keep_forever() -> Self {
        Self {
            max_age_days: None,
            max_count: None,
        }
    }

    /// Expire entries older than `days` days; no count limit.
    pub fn expire_after(days: u64) -> Self {
        Self {
            max_age_days: Some(days),
            max_count: None,
        }
    }

    /// Retain at most `count` entries per category; no age limit.
    pub fn limit_count(count: usize) -> Self {
        Self {
            max_age_days: None,
            max_count: Some(count),
        }
    }

    /// Combine both an age limit and a count limit.
    pub fn expire_and_limit(days: u64, count: usize) -> Self {
        Self {
            max_age_days: Some(days),
            max_count: Some(count),
        }
    }
}

impl Default for RetentionRule {
    fn default() -> Self {
        Self::keep_forever()
    }
}

// ---------------------------------------------------------------------------
// RetentionPolicy
// ---------------------------------------------------------------------------

/// Maps every [`MemoryCategory`] to a [`RetentionRule`].
///
/// Categories without an explicit mapping fall through to [`Self::default_rule`].
///
/// # Example
///
/// ```rust
/// use pares_agens_core::memory::{entry::MemoryCategory, forgetting::policy::{RetentionPolicy, RetentionRule}};
///
/// let mut policy = RetentionPolicy::new();
/// policy
///     .set_rule(MemoryCategory::Conversation, RetentionRule::expire_after(30))
///     .set_rule(MemoryCategory::CodePattern, RetentionRule::limit_count(200));
/// ```
#[derive(Debug, Clone, Default)]
pub struct RetentionPolicy {
    rules: HashMap<MemoryCategory, RetentionRule>,
    /// Rule applied to categories that are not explicitly mapped.
    pub default_rule: RetentionRule,
}

impl RetentionPolicy {
    /// Create a new empty policy (defaults to keep-forever for all categories).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the rule for a specific category.  Returns `&mut self` for chaining.
    pub fn set_rule(&mut self, category: MemoryCategory, rule: RetentionRule) -> &mut Self {
        self.rules.insert(category, rule);
        self
    }

    /// Retrieve the rule for `category`, falling back to [`Self::default_rule`].
    #[must_use]
    pub fn rule_for(&self, category: &MemoryCategory) -> &RetentionRule {
        self.rules.get(category).unwrap_or(&self.default_rule)
    }

    /// Return a sensible default policy for production use:
    ///
    /// | Category | Rule |
    /// |----------|------|
    /// | Conversation | 30 days, max 1 000 |
    /// | Preference | keep forever |
    /// | Decision | keep forever |
    /// | CodePattern | 180 days, max 500 |
    /// | ErrorFix | 90 days, max 200 |
    /// | UiInteraction | 7 days |
    /// | AppState | 7 days |
    /// | ScreenCapture | 14 days |
    /// | AutomationTrace | 30 days |
    /// | BuildResult | 14 days, max 100 |
    /// | DemoCheckpoint | 90 days |
    pub fn default_production() -> Self {
        let mut p = Self::new();
        p.set_rule(
            MemoryCategory::Conversation,
            RetentionRule::expire_and_limit(30, 1_000),
        )
        .set_rule(MemoryCategory::Preference, RetentionRule::keep_forever())
        .set_rule(MemoryCategory::Decision, RetentionRule::keep_forever())
        .set_rule(MemoryCategory::Procedure, RetentionRule::keep_forever())
        .set_rule(
            MemoryCategory::Fact,
            RetentionRule::expire_and_limit(365, 1_000),
        )
        .set_rule(
            MemoryCategory::CodePattern,
            RetentionRule::expire_and_limit(180, 500),
        )
        .set_rule(
            MemoryCategory::ErrorFix,
            RetentionRule::expire_and_limit(90, 200),
        )
        .set_rule(
            MemoryCategory::UiInteraction,
            RetentionRule::expire_after(7),
        )
        .set_rule(MemoryCategory::AppState, RetentionRule::expire_after(7))
        .set_rule(
            MemoryCategory::ScreenCapture,
            RetentionRule::expire_after(14),
        )
        .set_rule(
            MemoryCategory::AutomationTrace,
            RetentionRule::expire_after(30),
        )
        .set_rule(
            MemoryCategory::BuildResult,
            RetentionRule::expire_and_limit(14, 100),
        )
        .set_rule(
            MemoryCategory::DemoCheckpoint,
            RetentionRule::expire_after(90),
        );
        p
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rule_is_keep_forever() {
        let r = RetentionRule::default();
        assert!(r.max_age_days.is_none());
        assert!(r.max_count.is_none());
    }

    #[test]
    fn expire_after_sets_age() {
        let r = RetentionRule::expire_after(7);
        assert_eq!(r.max_age_days, Some(7));
        assert!(r.max_count.is_none());
    }

    #[test]
    fn limit_count_sets_count() {
        let r = RetentionRule::limit_count(50);
        assert!(r.max_age_days.is_none());
        assert_eq!(r.max_count, Some(50));
    }

    #[test]
    fn expire_and_limit_sets_both() {
        let r = RetentionRule::expire_and_limit(14, 100);
        assert_eq!(r.max_age_days, Some(14));
        assert_eq!(r.max_count, Some(100));
    }

    #[test]
    fn policy_falls_back_to_default() {
        let policy = RetentionPolicy::new();
        let rule = policy.rule_for(&MemoryCategory::Conversation);
        assert_eq!(*rule, RetentionRule::keep_forever());
    }

    #[test]
    fn policy_returns_category_specific_rule() {
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            RetentionRule::expire_after(30),
        );
        let rule = policy.rule_for(&MemoryCategory::Conversation);
        assert_eq!(rule.max_age_days, Some(30));
    }

    #[test]
    fn default_production_policy_has_known_rules() {
        let policy = RetentionPolicy::default_production();
        assert_eq!(
            policy.rule_for(&MemoryCategory::Conversation).max_age_days,
            Some(30)
        );
        assert!(policy
            .rule_for(&MemoryCategory::Preference)
            .max_age_days
            .is_none());
        assert_eq!(
            policy.rule_for(&MemoryCategory::BuildResult).max_count,
            Some(100)
        );
    }

    #[test]
    fn policy_chaining_works() {
        let mut policy = RetentionPolicy::new();
        policy
            .set_rule(MemoryCategory::Conversation, RetentionRule::expire_after(7))
            .set_rule(MemoryCategory::CodePattern, RetentionRule::limit_count(10));
        assert_eq!(
            policy.rule_for(&MemoryCategory::Conversation).max_age_days,
            Some(7)
        );
        assert_eq!(
            policy.rule_for(&MemoryCategory::CodePattern).max_count,
            Some(10)
        );
    }
}
