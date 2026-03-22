//! Mutation operators applied to an [`ExperimentTarget`](crate::ExperimentTarget).
//!
//! A [`MutationOperator`] describes *what change* was made to the target in a
//! single experiment.  The operator is serialised into the ledger entry diff so
//! every change is fully auditable.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── MutationOperator ──────────────────────────────────────────────────────────

/// A typed mutation applied to an experiment target.
///
/// Operators are intentionally simple and composable — the hypothesis engine
/// selects and parametrises them; the sandbox applies them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MutationOperator {
    /// Replace the value of a named parameter / configuration key.
    SetParameter {
        /// Dot-separated key path (e.g. `"model.temperature"`).
        key: String,
        /// New value (JSON-encoded scalar).
        value: serde_json::Value,
        /// Previous value, for revert.
        previous: serde_json::Value,
    },

    /// Scale a numeric parameter by a multiplicative factor.
    ScaleParameter {
        /// Dot-separated key path.
        key: String,
        /// Multiplicative factor (e.g. `2.0` doubles the value).
        factor: f64,
        /// Value before scaling (for revert).
        previous: f64,
    },

    /// Reorder steps in a procedure.
    ReorderSteps {
        /// New ordering expressed as zero-based indices into the original step list.
        new_order: Vec<usize>,
    },

    /// Insert a new step into a procedure at the given position.
    InsertStep {
        /// Zero-based insertion index.
        position: usize,
        /// Step specification (JSON object).
        step: serde_json::Value,
    },

    /// Remove a step from a procedure.
    RemoveStep {
        /// Zero-based index of the step to remove.
        position: usize,
        /// Step specification preserved for revert.
        removed_step: serde_json::Value,
    },

    /// Append or replace a block of text in a source file.
    PatchText {
        /// Unified diff string.
        diff: String,
    },

    /// Replace every occurrence of one string with another in the target.
    FindReplace {
        /// Text to find (literal, not regex).
        find: String,
        /// Replacement text.
        replace: String,
    },

    /// Apply a batch of key-value overrides to a hyperparameter set.
    HyperparamOverride {
        /// Key → new value.
        overrides: HashMap<String, serde_json::Value>,
        /// Key → previous value (for revert).
        previous: HashMap<String, serde_json::Value>,
    },
}

impl MutationOperator {
    /// Return a short human-readable description of the mutation.
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::SetParameter { key, value, .. } => {
                format!("set {key} = {value}")
            }
            Self::ScaleParameter { key, factor, .. } => {
                format!("scale {key} × {factor}")
            }
            Self::ReorderSteps { new_order } => {
                format!("reorder steps → {:?}", new_order)
            }
            Self::InsertStep { position, .. } => {
                format!("insert step at position {position}")
            }
            Self::RemoveStep { position, .. } => {
                format!("remove step at position {position}")
            }
            Self::PatchText { diff } => {
                let preview = &diff[..diff.len().min(60)];
                format!("patch text: {preview}…")
            }
            Self::FindReplace { find, replace } => {
                format!("find {find:?} → replace {replace:?}")
            }
            Self::HyperparamOverride { overrides, .. } => {
                let keys: Vec<&String> = overrides.keys().collect();
                format!("hyperparam overrides: {keys:?}")
            }
        }
    }

    /// Serialise this mutation to a [`serde_json::Value`] for ledger storage.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialisation fails (practically
    /// never, since all variants use JSON-serialisable types).
    pub fn to_diff(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

// ── MutationSet ───────────────────────────────────────────────────────────────

/// An ordered collection of mutations applied atomically in one experiment.
///
/// Typically a single experiment applies one mutation, but compound mutations
/// (e.g. "set learning rate AND set batch size") are supported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationSet {
    /// The individual mutations, applied in order.
    pub operators: Vec<MutationOperator>,
}

impl MutationSet {
    /// Create a `MutationSet` from a single operator.
    #[must_use]
    pub fn single(op: MutationOperator) -> Self {
        Self { operators: vec![op] }
    }

    /// Return a combined description of all mutations.
    #[must_use]
    pub fn description(&self) -> String {
        self.operators
            .iter()
            .map(MutationOperator::description)
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Serialise to a JSON diff value.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialisation fails.
    pub fn to_diff(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_parameter_description() {
        let op = MutationOperator::SetParameter {
            key: "model.temperature".into(),
            value: json!(0.7),
            previous: json!(1.0),
        };
        assert!(op.description().contains("model.temperature"));
    }

    #[test]
    fn scale_parameter_description() {
        let op = MutationOperator::ScaleParameter {
            key: "batch_size".into(),
            factor: 2.0,
            previous: 32.0,
        };
        assert!(op.description().contains("batch_size"));
        assert!(op.description().contains('×'));
    }

    #[test]
    fn find_replace_description() {
        let op = MutationOperator::FindReplace {
            find: "foo".into(),
            replace: "bar".into(),
        };
        assert!(op.description().contains("foo"));
        assert!(op.description().contains("bar"));
    }

    #[test]
    fn operator_to_diff_roundtrips() {
        let op = MutationOperator::SetParameter {
            key: "lr".into(),
            value: json!(0.001),
            previous: json!(0.01),
        };
        let diff = op.to_diff().unwrap();
        let back: MutationOperator = serde_json::from_value(diff).unwrap();
        assert_eq!(op, back);
    }

    #[test]
    fn mutation_set_single_description() {
        let set = MutationSet::single(MutationOperator::SetParameter {
            key: "lr".into(),
            value: json!(0.001),
            previous: json!(0.01),
        });
        assert!(!set.description().is_empty());
    }

    #[test]
    fn mutation_set_multiple_descriptions_joined() {
        let set = MutationSet {
            operators: vec![
                MutationOperator::SetParameter {
                    key: "lr".into(),
                    value: json!(0.001),
                    previous: json!(0.01),
                },
                MutationOperator::ScaleParameter {
                    key: "batch".into(),
                    factor: 2.0,
                    previous: 32.0,
                },
            ],
        };
        let desc = set.description();
        assert!(desc.contains("; "));
    }

    #[test]
    fn hyperparam_override_roundtrips() {
        let mut overrides = HashMap::new();
        overrides.insert("lr".into(), json!(0.001));
        let mut previous = HashMap::new();
        previous.insert("lr".into(), json!(0.01));
        let op = MutationOperator::HyperparamOverride { overrides, previous };
        let diff = op.to_diff().unwrap();
        let back: MutationOperator = serde_json::from_value(diff).unwrap();
        assert_eq!(op, back);
    }
}
