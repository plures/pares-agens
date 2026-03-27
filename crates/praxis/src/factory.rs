//! [`RulesFactory`] — builds and owns rules grouped by [`RuleCategory`].
//!
//! The factory is the primary entry point for constructing a rule set.  Rules
//! are registered into one of three slots (`inputRules`, `stateRules`,
//! `dataRules`) and can be evaluated individually or all-at-once.

use std::collections::HashMap;

use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};

// ---------------------------------------------------------------------------
// RulesFactory
// ---------------------------------------------------------------------------

/// Builds and owns rules grouped by [`RuleCategory`].
///
/// # Usage
///
/// ```rust
/// use pares_agens_praxis::{RulesFactory, RuleCategory, RuleContext, RuleResult};
/// use pares_agens_praxis::rule::Rule;
/// use serde_json::json;
///
/// struct AlwaysPass;
/// impl Rule for AlwaysPass {
///     fn name(&self) -> &str { "always_pass" }
///     fn category(&self) -> RuleCategory { RuleCategory::Input }
///     fn evaluate(&self, _ctx: &RuleContext) -> RuleResult { RuleResult::Pass }
/// }
///
/// let mut factory = RulesFactory::new();
/// factory.register(Box::new(AlwaysPass));
///
/// let ctx = RuleContext::new("test", json!({}));
/// let results = factory.evaluate_all(&ctx);
/// assert_eq!(results.len(), 1);
/// ```
pub struct RulesFactory {
    rules: Vec<Box<dyn Rule>>,
}

impl Default for RulesFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl RulesFactory {
    /// Create an empty factory.
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Register a rule.  Rules are evaluated in registration order.
    pub fn register(&mut self, rule: Box<dyn Rule>) -> &mut Self {
        self.rules.push(rule);
        self
    }

    /// Number of rules currently registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Returns `true` when no rules have been registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Evaluate all rules against `ctx`.
    ///
    /// Returns `(rule_name, RuleResult)` pairs in registration order.
    pub fn evaluate_all(&self, ctx: &RuleContext) -> Vec<(String, RuleResult)> {
        self.rules
            .iter()
            .map(|r| (r.name().to_string(), r.evaluate(ctx)))
            .collect()
    }

    /// Evaluate only the rules belonging to `category`.
    pub fn evaluate_category(
        &self,
        ctx: &RuleContext,
        category: RuleCategory,
    ) -> Vec<(String, RuleResult)> {
        self.rules
            .iter()
            .filter(|r| r.category() == category)
            .map(|r| (r.name().to_string(), r.evaluate(ctx)))
            .collect()
    }

    /// Return `true` if at least one rule of each expected category is
    /// registered (`inputRules`, `stateRules`, `dataRules`).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        let all = [RuleCategory::Input, RuleCategory::State, RuleCategory::Data];
        all.iter()
            .all(|cat| self.rules.iter().any(|r| r.category() == *cat))
    }

    /// Count rules per category.  Returns a map from category to count.
    #[must_use]
    pub fn counts_by_category(&self) -> HashMap<RuleCategory, usize> {
        let mut map: HashMap<RuleCategory, usize> = HashMap::new();
        for r in &self.rules {
            *map.entry(r.category()).or_insert(0) += 1;
        }
        map
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct SimpleRule {
        name: &'static str,
        cat: RuleCategory,
        result: RuleResult,
    }

    impl Rule for SimpleRule {
        fn name(&self) -> &str {
            self.name
        }
        fn category(&self) -> RuleCategory {
            self.cat.clone()
        }
        fn evaluate(&self, _ctx: &RuleContext) -> RuleResult {
            self.result.clone()
        }
    }

    fn make_rule(name: &'static str, cat: RuleCategory, result: RuleResult) -> Box<dyn Rule> {
        Box::new(SimpleRule { name, cat, result })
    }

    #[test]
    fn factory_starts_empty() {
        let f = RulesFactory::new();
        assert!(f.is_empty());
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn register_and_len() {
        let mut f = RulesFactory::new();
        f.register(make_rule("r1", RuleCategory::Input, RuleResult::Pass));
        assert_eq!(f.len(), 1);
        assert!(!f.is_empty());
    }

    #[test]
    fn evaluate_all_returns_all_results() {
        let mut f = RulesFactory::new();
        f.register(make_rule("r1", RuleCategory::Input, RuleResult::Pass));
        f.register(make_rule(
            "r2",
            RuleCategory::State,
            RuleResult::Fail { reason: "x".into() },
        ));
        let ctx = RuleContext::new("test", json!({}));
        let results = f.evaluate_all(&ctx);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].1, RuleResult::Pass);
    }

    #[test]
    fn evaluate_category_filters_rules() {
        let mut f = RulesFactory::new();
        f.register(make_rule("r_input", RuleCategory::Input, RuleResult::Pass));
        f.register(make_rule("r_state", RuleCategory::State, RuleResult::Pass));
        let ctx = RuleContext::new("test", json!({}));
        let results = f.evaluate_category(&ctx, RuleCategory::Input);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "r_input");
    }

    #[test]
    fn is_complete_requires_all_categories() {
        let mut f = RulesFactory::new();
        f.register(make_rule("r1", RuleCategory::Input, RuleResult::Pass));
        assert!(!f.is_complete());
        f.register(make_rule("r2", RuleCategory::State, RuleResult::Pass));
        assert!(!f.is_complete());
        f.register(make_rule("r3", RuleCategory::Data, RuleResult::Pass));
        assert!(f.is_complete());
    }

    #[test]
    fn counts_by_category() {
        let mut f = RulesFactory::new();
        f.register(make_rule("r1", RuleCategory::Input, RuleResult::Pass));
        f.register(make_rule("r2", RuleCategory::Input, RuleResult::Pass));
        f.register(make_rule("r3", RuleCategory::State, RuleResult::Pass));
        let counts = f.counts_by_category();
        assert_eq!(counts[&RuleCategory::Input], 2);
        assert_eq!(counts[&RuleCategory::State], 1);
        assert_eq!(counts.get(&RuleCategory::Data), None);
    }
}
