//! Execution sandbox — isolated experiment runner with timeout enforcement.
//!
//! [`ExecutionSandbox`] is a trait for running an experiment and returning its
//! raw output.  [`DryRunSandbox`] is the built-in no-op implementation used
//! for testing and offline evaluation.  A real Rust-native sandbox would
//! spawn a subprocess, apply the mutation, run the target, capture stdout/stderr,
//! and revert the mutation — all within the configured timeout.

use crate::AutoresearchError;
use serde::{Deserialize, Serialize};

// ── SandboxOutput ─────────────────────────────────────────────────────────────

/// Raw output produced by an experiment execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxOutput {
    /// Combined stdout/stderr captured from the execution.
    pub stdout: String,
    /// Exit code of the command/process, if applicable.
    pub exit_code: Option<i32>,
    /// Whether the execution was terminated due to timeout.
    pub timed_out: bool,
    /// Elapsed wall-clock time in seconds.
    pub elapsed_secs: f64,
}

impl SandboxOutput {
    /// Return `true` when the execution completed successfully (exit code 0,
    /// no timeout).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.exit_code.is_none_or(|c| c == 0)
    }
}

// ── ExecutionSandbox trait ────────────────────────────────────────────────────

/// Pluggable execution backend for running a single experiment.
pub trait ExecutionSandbox: Send + Sync {
    /// Apply `mutation_diff` to the target and execute it, returning the raw
    /// output.
    ///
    /// Implementations are expected to:
    /// 1. Apply the mutation to the target.
    /// 2. Execute the target (respecting `timeout_secs`).
    /// 3. Capture output.
    /// 4. Revert the mutation regardless of success/failure.
    ///
    /// # Errors
    ///
    /// Returns [`AutoresearchError::SandboxError`] for unrecoverable sandbox
    /// failures (e.g. unable to apply mutation, revert failed).
    fn execute(
        &self,
        target_label: &str,
        mutation_diff: &serde_json::Value,
        timeout_secs: f64,
    ) -> Result<SandboxOutput, AutoresearchError>;
}

// ── DryRunSandbox ─────────────────────────────────────────────────────────────

/// A no-op sandbox that simulates execution without touching the filesystem.
///
/// Used for testing, benchmarking, and offline hypothesis validation.
/// `DryRunSandbox` always reports success and returns a configurable fake
/// stdout string.
pub struct DryRunSandbox {
    /// Fake stdout returned by every execution.
    pub fake_output: String,
    /// Simulated elapsed time (seconds).
    pub fake_elapsed_secs: f64,
}

impl Default for DryRunSandbox {
    fn default() -> Self {
        Self {
            fake_output: "val_bpb: 1.234".into(),
            fake_elapsed_secs: 1.0,
        }
    }
}

impl ExecutionSandbox for DryRunSandbox {
    fn execute(
        &self,
        _target_label: &str,
        _mutation_diff: &serde_json::Value,
        _timeout_secs: f64,
    ) -> Result<SandboxOutput, AutoresearchError> {
        Ok(SandboxOutput {
            stdout: self.fake_output.clone(),
            exit_code: Some(0),
            timed_out: false,
            elapsed_secs: self.fake_elapsed_secs,
        })
    }
}

// ── FailingSandbox ────────────────────────────────────────────────────────────

/// A sandbox that always fails — useful for testing error paths.
pub struct FailingSandbox {
    /// Error message to surface.
    pub error_message: String,
}

impl ExecutionSandbox for FailingSandbox {
    fn execute(
        &self,
        _target_label: &str,
        _mutation_diff: &serde_json::Value,
        _timeout_secs: f64,
    ) -> Result<SandboxOutput, AutoresearchError> {
        Err(AutoresearchError::SandboxError(self.error_message.clone()))
    }
}

/// A sandbox that simulates a timeout.
pub struct TimeoutSandbox {
    /// Simulated elapsed time (should exceed the configured timeout).
    pub elapsed_secs: f64,
}

impl ExecutionSandbox for TimeoutSandbox {
    fn execute(
        &self,
        _target_label: &str,
        _mutation_diff: &serde_json::Value,
        _timeout_secs: f64,
    ) -> Result<SandboxOutput, AutoresearchError> {
        Ok(SandboxOutput {
            stdout: String::new(),
            exit_code: None,
            timed_out: true,
            elapsed_secs: self.elapsed_secs,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sandbox_output_succeeded_on_exit_zero() {
        let out = SandboxOutput {
            stdout: "ok".into(),
            exit_code: Some(0),
            timed_out: false,
            elapsed_secs: 1.0,
        };
        assert!(out.succeeded());
    }

    #[test]
    fn sandbox_output_fails_on_nonzero_exit() {
        let out = SandboxOutput {
            stdout: String::new(),
            exit_code: Some(1),
            timed_out: false,
            elapsed_secs: 1.0,
        };
        assert!(!out.succeeded());
    }

    #[test]
    fn sandbox_output_fails_on_timeout() {
        let out = SandboxOutput {
            stdout: String::new(),
            exit_code: None,
            timed_out: true,
            elapsed_secs: 300.0,
        };
        assert!(!out.succeeded());
    }

    #[test]
    fn dry_run_sandbox_returns_fake_output() {
        let sandbox = DryRunSandbox::default();
        let out = sandbox.execute("procedure:test", &json!({}), 60.0).unwrap();
        assert!(out.succeeded());
        assert_eq!(out.stdout, "val_bpb: 1.234");
    }

    #[test]
    fn failing_sandbox_returns_error() {
        let sandbox = FailingSandbox {
            error_message: "disk full".into(),
        };
        let err = sandbox
            .execute("file:main.rs", &json!({}), 60.0)
            .unwrap_err();
        assert!(matches!(err, AutoresearchError::SandboxError(_)));
    }

    #[test]
    fn timeout_sandbox_reports_timeout() {
        let sandbox = TimeoutSandbox {
            elapsed_secs: 400.0,
        };
        let out = sandbox
            .execute("command:cargo test", &json!({}), 300.0)
            .unwrap();
        assert!(out.timed_out);
        assert!(!out.succeeded());
    }
}
