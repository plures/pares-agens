//! Pipeline and step definitions for Faber CI.

use serde::{Deserialize, Serialize};

use crate::FaberError;

// ── StepKind ──────────────────────────────────────────────────────────────────

/// The kind of work a [`Step`] performs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepKind {
    /// Execute a shell command.
    Shell {
        /// The command string to run (interpreted by the runtime).
        command: String,
    },
    /// Invoke a named Pares agent tool or procedure.
    AgentTool {
        /// Name of the tool or procedure to invoke.
        tool: String,
        /// JSON-encoded arguments passed to the tool.
        args: serde_json::Value,
    },
    /// Emit an event into the Pares event bus.
    EmitEvent {
        /// Arbitrary event payload.
        payload: serde_json::Value,
    },
}

// ── Step ──────────────────────────────────────────────────────────────────────

/// A single unit of work inside a [`Pipeline`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Human-readable name for this step (must be non-empty and unique within
    /// its pipeline).
    pub name: String,

    /// What this step does.
    pub kind: StepKind,

    /// Whether a failure in this step should abort the entire pipeline
    /// (`true`) or be recorded and allow subsequent steps to run (`false`).
    pub fail_fast: bool,
}

impl Step {
    /// Create a new `Step` with `fail_fast = true`.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: StepKind) -> Self {
        Self {
            name: name.into(),
            kind,
            fail_fast: true,
        }
    }

    /// Set whether this step should abort the pipeline on failure.
    #[must_use]
    pub fn with_fail_fast(mut self, fail_fast: bool) -> Self {
        self.fail_fast = fail_fast;
        self
    }
}

// ── Pipeline ─────────────────────────────────────────────────────────────────

/// A named, ordered sequence of [`Step`]s forming a CI pipeline.
///
/// # Example
///
/// ```
/// use pares_agens_faber::pipeline::{Pipeline, Step, StepKind};
///
/// let pipeline = Pipeline::new("lint", vec![
///     Step::new("clippy", StepKind::Shell { command: "cargo clippy".to_string() }),
/// ]).unwrap();
/// assert_eq!(pipeline.name(), "lint");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pipeline {
    name: String,
    steps: Vec<Step>,
}

impl Pipeline {
    /// Create a new `Pipeline`.
    ///
    /// # Errors
    ///
    /// Returns [`FaberError::InvalidPipeline`] when `name` is empty or
    /// `steps` is empty.
    pub fn new(name: impl Into<String>, steps: Vec<Step>) -> Result<Self, FaberError> {
        let name = name.into();
        if name.is_empty() {
            return Err(FaberError::InvalidPipeline(
                "pipeline name must not be empty".to_string(),
            ));
        }
        if steps.is_empty() {
            return Err(FaberError::InvalidPipeline(
                "pipeline must contain at least one step".to_string(),
            ));
        }
        Ok(Self { name, steps })
    }

    /// Return the pipeline name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the ordered slice of steps.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_step(name: &str) -> Step {
        Step::new(
            name,
            StepKind::Shell {
                command: format!("echo {name}"),
            },
        )
    }

    #[test]
    fn pipeline_new_succeeds_with_valid_args() {
        let p = Pipeline::new("ci", vec![shell_step("build")]).unwrap();
        assert_eq!(p.name(), "ci");
        assert_eq!(p.steps().len(), 1);
    }

    #[test]
    fn pipeline_rejects_empty_name() {
        assert!(matches!(
            Pipeline::new("", vec![shell_step("s")]),
            Err(FaberError::InvalidPipeline(_))
        ));
    }

    #[test]
    fn pipeline_rejects_empty_steps() {
        assert!(matches!(
            Pipeline::new("p", vec![]),
            Err(FaberError::InvalidPipeline(_))
        ));
    }

    #[test]
    fn step_fail_fast_defaults_to_true() {
        let s = Step::new(
            "s",
            StepKind::Shell {
                command: "true".to_string(),
            },
        );
        assert!(s.fail_fast);
    }

    #[test]
    fn step_with_fail_fast_overrides() {
        let s = Step::new(
            "s",
            StepKind::Shell {
                command: "true".to_string(),
            },
        )
        .with_fail_fast(false);
        assert!(!s.fail_fast);
    }
}
