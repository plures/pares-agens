//! [`PraxisModule`] trait and [`CompletenessReport`].

use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};

// ---------------------------------------------------------------------------
// CompletenessReport
// ---------------------------------------------------------------------------

/// Coverage summary produced by [`PraxisModule::audit`].
///
/// Mirrors the "completeness audits" capability described in the `@plures/praxis`
/// specification.  A report tells you which rule categories are covered, which
/// are absent, and gives an overall percentage score.
#[derive(Debug, Clone)]
pub struct CompletenessReport {
    /// Name of the module that produced this report.
    pub module: String,
    /// Total number of rules registered in this module.
    pub total_rules: usize,
    /// Rule categories that have at least one rule registered.
    pub covered_categories: Vec<RuleCategory>,
    /// Rule categories that have *no* rules registered.
    pub missing_categories: Vec<RuleCategory>,
    /// Fraction of the three expected categories that are covered, as a
    /// percentage (0.0 – 100.0).
    pub completeness_pct: f32,
}

impl CompletenessReport {
    /// `true` when every expected category has at least one rule.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing_categories.is_empty()
    }
}

// ---------------------------------------------------------------------------
// PraxisModule trait
// ---------------------------------------------------------------------------

/// A domain grouping of [`Rule`]s with a built-in completeness audit.
///
/// Implement this trait for each of the four suggested modules:
/// - `agent-lifecycle`
/// - `task-routing`
/// - `coordination`
/// - `safety`
///
/// # Expectations DSL
///
/// The `expectations` method returns a human-readable list of what the module
/// *expects* to be true before any of its rules are evaluated.  These form
/// the module's "contract" and can be displayed in dashboards or logged during
/// startup.
pub trait PraxisModule: Send + Sync {
    /// Short, stable name for this module (e.g. `"safety"`).
    fn name(&self) -> &str;

    /// Returns all rules registered in this module as a borrowed slice.
    ///
    /// Returning `&[Box<dyn Rule>]` avoids a per-call allocation; the default
    /// helper methods (`evaluate_all`, `evaluate_category`, `audit`) iterate
    /// over this slice directly.
    fn rules(&self) -> &[Box<dyn Rule>];

    /// Human-readable preconditions ("expectations") the module assumes hold
    /// before any rule is evaluated.
    fn expectations(&self) -> Vec<String>;

    /// Evaluate every rule in the module against `ctx` and return
    /// `(rule_name, RuleResult)` pairs in registration order.
    fn evaluate_all(&self, ctx: &RuleContext) -> Vec<(String, RuleResult)> {
        self.rules()
            .iter()
            .map(|r| (r.name().to_string(), r.evaluate(ctx)))
            .collect()
    }

    /// Evaluate only the rules belonging to `category`.
    fn evaluate_category(
        &self,
        ctx: &RuleContext,
        category: RuleCategory,
    ) -> Vec<(String, RuleResult)> {
        self.rules()
            .iter()
            .filter(|r| r.category() == category)
            .map(|r| (r.name().to_string(), r.evaluate(ctx)))
            .collect()
    }

    /// Produce a [`CompletenessReport`] for this module.
    ///
    /// All three expected categories (Input / State / Data) are checked.
    fn audit(&self) -> CompletenessReport {
        let all_categories = [RuleCategory::Input, RuleCategory::State, RuleCategory::Data];
        let rules = self.rules();
        let total_rules = rules.len();

        let covered: Vec<RuleCategory> = all_categories
            .iter()
            .filter(|cat| rules.iter().any(|r| r.category() == **cat))
            .cloned()
            .collect();

        let missing: Vec<RuleCategory> = all_categories
            .iter()
            .filter(|cat| !rules.iter().any(|r| r.category() == **cat))
            .cloned()
            .collect();

        let completeness_pct = (covered.len() as f32 / all_categories.len() as f32) * 100.0;

        CompletenessReport {
            module: self.name().to_string(),
            total_rules,
            covered_categories: covered,
            missing_categories: missing,
            completeness_pct,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};
    use serde_json::json;

    // Minimal rule stubs for testing
    struct PassRule {
        name: &'static str,
        cat: RuleCategory,
    }
    impl Rule for PassRule {
        fn name(&self) -> &str {
            self.name
        }
        fn category(&self) -> RuleCategory {
            self.cat.clone()
        }
        fn evaluate(&self, _ctx: &RuleContext) -> RuleResult {
            RuleResult::Pass
        }
    }

    struct MinimalModule {
        rules: Vec<Box<dyn Rule>>,
    }
    impl PraxisModule for MinimalModule {
        fn name(&self) -> &str {
            "minimal"
        }
        fn rules(&self) -> &[Box<dyn Rule>] {
            &self.rules
        }
        fn expectations(&self) -> Vec<String> {
            vec![]
        }
    }

    #[test]
    fn audit_complete_when_all_categories_covered() {
        let module = MinimalModule {
            rules: vec![
                Box::new(PassRule {
                    name: "r1",
                    cat: RuleCategory::Input,
                }),
                Box::new(PassRule {
                    name: "r2",
                    cat: RuleCategory::State,
                }),
                Box::new(PassRule {
                    name: "r3",
                    cat: RuleCategory::Data,
                }),
            ],
        };
        let report = module.audit();
        assert!(report.is_complete());
        assert_eq!(report.completeness_pct, 100.0);
        assert_eq!(report.total_rules, 3);
        assert!(report.missing_categories.is_empty());
    }

    #[test]
    fn audit_incomplete_when_category_missing() {
        let module = MinimalModule {
            rules: vec![
                Box::new(PassRule {
                    name: "r1",
                    cat: RuleCategory::Input,
                }),
                Box::new(PassRule {
                    name: "r2",
                    cat: RuleCategory::State,
                }),
                // No Data rule
            ],
        };
        let report = module.audit();
        assert!(!report.is_complete());
        assert!(report.missing_categories.contains(&RuleCategory::Data));
        assert!((report.completeness_pct - 66.666_67).abs() < 0.01);
    }

    #[test]
    fn evaluate_all_returns_result_per_rule() {
        let module = MinimalModule {
            rules: vec![
                Box::new(PassRule {
                    name: "r1",
                    cat: RuleCategory::Input,
                }),
                Box::new(PassRule {
                    name: "r2",
                    cat: RuleCategory::State,
                }),
            ],
        };
        let ctx = RuleContext::new("test", json!({}));
        let results = module.evaluate_all(&ctx);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(_, r)| *r == RuleResult::Pass));
    }

    #[test]
    fn evaluate_category_filters_correctly() {
        let module = MinimalModule {
            rules: vec![
                Box::new(PassRule {
                    name: "r_input",
                    cat: RuleCategory::Input,
                }),
                Box::new(PassRule {
                    name: "r_state",
                    cat: RuleCategory::State,
                }),
            ],
        };
        let ctx = RuleContext::new("test", json!({}));
        let input_results = module.evaluate_category(&ctx, RuleCategory::Input);
        assert_eq!(input_results.len(), 1);
        assert_eq!(input_results[0].0, "r_input");
    }
}
