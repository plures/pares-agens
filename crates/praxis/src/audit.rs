//! [`CompletenessAudit`] — workspace-level completeness scanner.
//!
//! Aggregates [`CompletenessReport`]s from every [`PraxisModule`] in the
//! workspace and surfaces a combined coverage score.  This is the "completeness
//! audits" capability from `@plures/praxis`.

use crate::module::{CompletenessReport, PraxisModule};

// ---------------------------------------------------------------------------
// CompletenessAudit
// ---------------------------------------------------------------------------

/// Aggregates completeness reports across all registered [`PraxisModule`]s.
///
/// Call [`CompletenessAudit::run`] after assembling all modules to get a
/// combined view of rule coverage across the entire agent mesh.
#[derive(Default)]
pub struct CompletenessAudit {
    modules: Vec<Box<dyn PraxisModule>>,
}

impl CompletenessAudit {
    /// Create an empty audit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a module to be included in the audit.
    pub fn register(&mut self, module: Box<dyn PraxisModule>) -> &mut Self {
        self.modules.push(module);
        self
    }

    /// Run the audit and return per-module reports.
    pub fn run(&self) -> AuditSummary {
        let reports: Vec<CompletenessReport> = self.modules.iter().map(|m| m.audit()).collect();

        let total_rules: usize = reports.iter().map(|r| r.total_rules).sum();
        let complete_modules = reports.iter().filter(|r| r.is_complete()).count();
        let overall_pct = if reports.is_empty() {
            0.0
        } else {
            reports.iter().map(|r| r.completeness_pct).sum::<f32>() / reports.len() as f32
        };

        AuditSummary {
            module_reports: reports,
            total_rules,
            complete_modules,
            overall_completeness_pct: overall_pct,
        }
    }
}

// ---------------------------------------------------------------------------
// AuditSummary
// ---------------------------------------------------------------------------

/// Combined output of a [`CompletenessAudit`] run.
#[derive(Debug)]
pub struct AuditSummary {
    /// Per-module completeness reports.
    pub module_reports: Vec<CompletenessReport>,
    /// Total number of rules across all modules.
    pub total_rules: usize,
    /// Number of modules where every expected category is covered.
    pub complete_modules: usize,
    /// Mean completeness percentage across all modules (0.0 – 100.0).
    pub overall_completeness_pct: f32,
}

impl AuditSummary {
    /// `true` when every registered module is complete.
    #[must_use]
    pub fn all_complete(&self) -> bool {
        self.complete_modules == self.module_reports.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};

    struct AlwaysPassRule {
        name: &'static str,
        cat: RuleCategory,
    }
    impl Rule for AlwaysPassRule {
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

    struct FullModule {
        rules: Vec<Box<dyn Rule>>,
    }
    impl PraxisModule for FullModule {
        fn name(&self) -> &str {
            "full"
        }
        fn rules(&self) -> &[Box<dyn Rule>] {
            &self.rules
        }
        fn expectations(&self) -> Vec<String> {
            vec![]
        }
    }

    fn full_module() -> Box<dyn PraxisModule> {
        Box::new(FullModule {
            rules: vec![
                Box::new(AlwaysPassRule {
                    name: "i1",
                    cat: RuleCategory::Input,
                }),
                Box::new(AlwaysPassRule {
                    name: "s1",
                    cat: RuleCategory::State,
                }),
                Box::new(AlwaysPassRule {
                    name: "d1",
                    cat: RuleCategory::Data,
                }),
            ],
        })
    }

    struct PartialModule;
    impl PraxisModule for PartialModule {
        fn name(&self) -> &str {
            "partial"
        }
        fn rules(&self) -> &[Box<dyn Rule>] {
            &[]
        }
        fn expectations(&self) -> Vec<String> {
            vec![]
        }
    }

    #[test]
    fn empty_audit_returns_zero_completeness() {
        let audit = CompletenessAudit::new();
        let summary = audit.run();
        assert_eq!(summary.overall_completeness_pct, 0.0);
        assert!(summary.all_complete());
    }

    #[test]
    fn full_module_reports_100_pct() {
        let mut audit = CompletenessAudit::new();
        audit.register(full_module());
        let summary = audit.run();
        assert!(summary.all_complete());
        assert_eq!(summary.overall_completeness_pct, 100.0);
        assert_eq!(summary.total_rules, 3);
    }

    #[test]
    fn partial_module_reduces_completeness() {
        let mut audit = CompletenessAudit::new();
        audit.register(full_module());
        audit.register(Box::new(PartialModule));
        let summary = audit.run();
        assert!(!summary.all_complete());
        assert_eq!(summary.complete_modules, 1);
        assert!((summary.overall_completeness_pct - 50.0).abs() < 0.01);
    }
}
