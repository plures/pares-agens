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

    // -----------------------------------------------------------------------
    // RetentionRule constructors — verify exact field values
    // -----------------------------------------------------------------------

    #[test]
    fn default_rule_is_keep_forever() {
        let r = RetentionRule::default();
        assert!(r.max_age_days.is_none());
        assert!(r.max_count.is_none());
    }

    #[test]
    fn keep_forever_returns_none_none() {
        let r = RetentionRule::keep_forever();
        assert_eq!(r.max_age_days, None);
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn expire_after_sets_age_only() {
        let r = RetentionRule::expire_after(7);
        assert_eq!(r.max_age_days, Some(7));
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn expire_after_preserves_exact_value() {
        let r = RetentionRule::expire_after(365);
        assert_eq!(r.max_age_days, Some(365));
        assert_ne!(r.max_age_days, Some(0));
        assert_ne!(r.max_age_days, Some(1));
    }

    #[test]
    fn limit_count_sets_count_only() {
        let r = RetentionRule::limit_count(50);
        assert_eq!(r.max_age_days, None);
        assert_eq!(r.max_count, Some(50));
    }

    #[test]
    fn limit_count_preserves_exact_value() {
        let r = RetentionRule::limit_count(200);
        assert_eq!(r.max_count, Some(200));
        assert_ne!(r.max_count, Some(0));
        assert_ne!(r.max_count, Some(1));
    }

    #[test]
    fn expire_and_limit_sets_both() {
        let r = RetentionRule::expire_and_limit(14, 100);
        assert_eq!(r.max_age_days, Some(14));
        assert_eq!(r.max_count, Some(100));
    }

    #[test]
    fn expire_and_limit_does_not_swap_fields() {
        let r = RetentionRule::expire_and_limit(90, 500);
        // Ensure days goes to max_age_days, count goes to max_count
        assert_eq!(r.max_age_days, Some(90));
        assert_eq!(r.max_count, Some(500));
        assert_ne!(r.max_age_days, Some(500));
        assert_ne!(r.max_count, Some(90));
    }

    #[test]
    fn expire_after_zero_days_is_valid() {
        let r = RetentionRule::expire_after(0);
        assert_eq!(r.max_age_days, Some(0));
    }

    #[test]
    fn limit_count_zero_is_valid() {
        let r = RetentionRule::limit_count(0);
        assert_eq!(r.max_count, Some(0));
    }

    // -----------------------------------------------------------------------
    // RetentionRule equality and cloning
    // -----------------------------------------------------------------------

    #[test]
    fn retention_rule_equality() {
        assert_eq!(RetentionRule::keep_forever(), RetentionRule::default());
        assert_ne!(
            RetentionRule::expire_after(7),
            RetentionRule::keep_forever()
        );
        assert_ne!(
            RetentionRule::limit_count(10),
            RetentionRule::keep_forever()
        );
        assert_ne!(
            RetentionRule::expire_after(7),
            RetentionRule::expire_after(8)
        );
        assert_ne!(
            RetentionRule::limit_count(10),
            RetentionRule::limit_count(11)
        );
    }

    #[test]
    fn retention_rule_clone() {
        let r = RetentionRule::expire_and_limit(30, 1000);
        let r2 = r.clone();
        assert_eq!(r, r2);
        assert_eq!(r2.max_age_days, Some(30));
        assert_eq!(r2.max_count, Some(1000));
    }

    // -----------------------------------------------------------------------
    // RetentionPolicy — fallback, insertion, overwrite
    // -----------------------------------------------------------------------

    #[test]
    fn policy_new_equals_default() {
        let p1 = RetentionPolicy::new();
        let p2 = RetentionPolicy::default();
        // Both should have empty rules and keep-forever default
        assert_eq!(p1.default_rule, RetentionRule::keep_forever());
        assert_eq!(p2.default_rule, RetentionRule::keep_forever());
    }

    #[test]
    fn policy_falls_back_to_default_rule() {
        let policy = RetentionPolicy::new();
        // No explicit rule for Conversation → falls back
        let rule = policy.rule_for(&MemoryCategory::Conversation);
        assert_eq!(*rule, RetentionRule::keep_forever());
    }

    #[test]
    fn policy_falls_back_to_custom_default_rule() {
        let mut policy = RetentionPolicy::new();
        policy.default_rule = RetentionRule::expire_after(365);
        // Unmapped category should use custom default
        let rule = policy.rule_for(&MemoryCategory::ScreenCapture);
        assert_eq!(rule.max_age_days, Some(365));
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
        assert_eq!(rule.max_count, None);
    }

    #[test]
    fn policy_set_rule_overwrites_previous() {
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            RetentionRule::expire_after(30),
        );
        policy.set_rule(
            MemoryCategory::Conversation,
            RetentionRule::expire_after(60),
        );
        let rule = policy.rule_for(&MemoryCategory::Conversation);
        assert_eq!(rule.max_age_days, Some(60));
    }

    #[test]
    fn policy_different_categories_independent() {
        let mut policy = RetentionPolicy::new();
        policy.set_rule(MemoryCategory::Conversation, RetentionRule::expire_after(7));
        policy.set_rule(MemoryCategory::CodePattern, RetentionRule::limit_count(100));
        // Conversation should NOT have CodePattern's rule
        assert_eq!(
            policy.rule_for(&MemoryCategory::Conversation).max_count,
            None
        );
        assert_eq!(
            policy.rule_for(&MemoryCategory::CodePattern).max_age_days,
            None
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

    // -----------------------------------------------------------------------
    // default_production — verify EVERY category's rule
    // -----------------------------------------------------------------------

    #[test]
    fn production_conversation_30_days_1000_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::Conversation);
        assert_eq!(r.max_age_days, Some(30));
        assert_eq!(r.max_count, Some(1_000));
    }

    #[test]
    fn production_preference_keep_forever() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::Preference);
        assert_eq!(r.max_age_days, None);
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_decision_keep_forever() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::Decision);
        assert_eq!(r.max_age_days, None);
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_procedure_keep_forever() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::Procedure);
        assert_eq!(r.max_age_days, None);
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_fact_365_days_1000_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::Fact);
        assert_eq!(r.max_age_days, Some(365));
        assert_eq!(r.max_count, Some(1_000));
    }

    #[test]
    fn production_code_pattern_180_days_500_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::CodePattern);
        assert_eq!(r.max_age_days, Some(180));
        assert_eq!(r.max_count, Some(500));
    }

    #[test]
    fn production_error_fix_90_days_200_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::ErrorFix);
        assert_eq!(r.max_age_days, Some(90));
        assert_eq!(r.max_count, Some(200));
    }

    #[test]
    fn production_ui_interaction_7_days_no_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::UiInteraction);
        assert_eq!(r.max_age_days, Some(7));
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_app_state_7_days_no_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::AppState);
        assert_eq!(r.max_age_days, Some(7));
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_screen_capture_14_days_no_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::ScreenCapture);
        assert_eq!(r.max_age_days, Some(14));
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_automation_trace_30_days_no_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::AutomationTrace);
        assert_eq!(r.max_age_days, Some(30));
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_build_result_14_days_100_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::BuildResult);
        assert_eq!(r.max_age_days, Some(14));
        assert_eq!(r.max_count, Some(100));
    }

    #[test]
    fn production_demo_checkpoint_90_days_no_count() {
        let p = RetentionPolicy::default_production();
        let r = p.rule_for(&MemoryCategory::DemoCheckpoint);
        assert_eq!(r.max_age_days, Some(90));
        assert_eq!(r.max_count, None);
    }

    #[test]
    fn production_unmapped_category_falls_to_default() {
        let p = RetentionPolicy::default_production();
        // Correction is not explicitly mapped in default_production
        let r = p.rule_for(&MemoryCategory::Correction);
        assert_eq!(*r, p.default_rule);
        assert_eq!(*r, RetentionRule::keep_forever());
    }

    // -----------------------------------------------------------------------
    // Policy clone preserves all rules
    // -----------------------------------------------------------------------

    #[test]
    fn policy_clone_preserves_rules() {
        let p = RetentionPolicy::default_production();
        let p2 = p.clone();
        assert_eq!(
            p2.rule_for(&MemoryCategory::Conversation).max_age_days,
            Some(30)
        );
        assert_eq!(p2.rule_for(&MemoryCategory::ErrorFix).max_count, Some(200));
    }
}
