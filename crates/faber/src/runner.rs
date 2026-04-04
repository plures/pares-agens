//! CI pipeline execution engine for Faber.

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    pipeline::{Pipeline, StepKind},
    FaberError,
};

// ── ToolDispatcher ────────────────────────────────────────────────────────────

/// Dispatches an [`AgentTool`](StepKind::AgentTool) step to the appropriate
/// handler.
///
/// Implement this trait to route tool invocations through your agent runtime.
/// When no dispatcher is configured on a [`CiRunner`] the step is recorded as
/// a passed stub.
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// Invoke `tool` with the given `args` and return the JSON result.
    ///
    /// # Errors
    ///
    /// Returns a [`FaberError`] if the tool invocation fails.
    async fn dispatch(
        &self,
        tool: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, FaberError>;
}

// ── EventBus ─────────────────────────────────────────────────────────────────

/// Emits events for [`EmitEvent`](StepKind::EmitEvent) steps.
///
/// Implement this trait to forward pipeline events to your event bus.
/// When no bus is configured on a [`CiRunner`] the step is recorded as a
/// passed stub.
#[async_trait]
pub trait EventBus: Send + Sync {
    /// Emit a single event `payload`.
    ///
    /// # Errors
    ///
    /// Returns a [`FaberError`] if the event cannot be emitted.
    async fn emit(&self, payload: &serde_json::Value) -> Result<(), FaberError>;
}

// ── StepStatus ────────────────────────────────────────────────────────────────

/// Outcome of executing a single pipeline step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The step completed without errors.
    Passed,
    /// The step produced an error.
    Failed,
    /// The step was skipped because an earlier `fail_fast` step failed.
    Skipped,
}

// ── StepResult ────────────────────────────────────────────────────────────────

/// The recorded outcome of a single step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Name of the step (matches [`Step::name`]).
    pub step_name: String,

    /// Whether the step passed, failed, or was skipped.
    pub status: StepStatus,

    /// Optional human-readable output or error message.
    pub output: Option<String>,
}

// ── RunStatus ─────────────────────────────────────────────────────────────────

/// Overall status of a pipeline run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// All steps passed.
    Success,
    /// One or more steps failed.
    Failure,
    /// The run was cancelled before completing.
    Cancelled,
}

// ── RunReport ─────────────────────────────────────────────────────────────────

/// Full report for a single pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    /// Unique identifier for this run.
    pub run_id: String,

    /// Name of the pipeline that was run.
    pub pipeline_name: String,

    /// UTC timestamp when the run started.
    pub started_at: DateTime<Utc>,

    /// UTC timestamp when the run finished.
    pub finished_at: DateTime<Utc>,

    /// Overall run outcome.
    pub status: RunStatus,

    /// Per-step results in execution order.
    pub step_results: Vec<StepResult>,
}

impl RunReport {
    /// Return `true` when all steps passed.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status == RunStatus::Success
    }
}

// ── CiRunner ─────────────────────────────────────────────────────────────────

/// Executes [`Pipeline`]s and produces [`RunReport`]s.
///
/// Shell steps are spawned as real subprocesses via [`tokio::process::Command`]
/// using `sh -c` on Unix or `cmd /C` on Windows.  Stdout and stderr are
/// captured and stored in the [`StepResult`].  A non-zero exit code causes the
/// step to be recorded as [`StepStatus::Failed`].
///
/// `AgentTool` steps are routed through an optional [`ToolDispatcher`].
/// `EmitEvent` steps are routed through an optional [`EventBus`].
/// When no dispatcher/bus is wired up the step is recorded as a passed stub.
///
/// # Example
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// use pares_agens_faber::pipeline::{Pipeline, Step, StepKind};
/// use pares_agens_faber::runner::CiRunner;
///
/// let pipeline = Pipeline::new("demo", vec![
///     Step::new("hello", StepKind::Shell { command: "echo hello".to_string() }),
/// ]).unwrap();
///
/// let runner = CiRunner::new();
/// let report = runner.run(&pipeline).await.unwrap();
/// assert!(report.is_success());
/// # });
/// ```
pub struct CiRunner {
    tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    event_bus: Option<Arc<dyn EventBus>>,
}

impl fmt::Debug for CiRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CiRunner")
            .field("tool_dispatcher", &self.tool_dispatcher.is_some())
            .field("event_bus", &self.event_bus.is_some())
            .finish()
    }
}

impl Default for CiRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl CiRunner {
    /// Create a new `CiRunner` with no dispatcher or event bus wired up.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_dispatcher: None,
            event_bus: None,
        }
    }

    /// Attach a [`ToolDispatcher`] that handles [`StepKind::AgentTool`] steps.
    #[must_use]
    pub fn with_tool_dispatcher(mut self, dispatcher: Arc<dyn ToolDispatcher>) -> Self {
        self.tool_dispatcher = Some(dispatcher);
        self
    }

    /// Attach an [`EventBus`] that handles [`StepKind::EmitEvent`] steps.
    #[must_use]
    pub fn with_event_bus(mut self, bus: Arc<dyn EventBus>) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Execute `pipeline` and return a [`RunReport`].
    ///
    /// Steps are executed in order.  If a step's `fail_fast` flag is `true`
    /// and it fails, subsequent steps are marked [`StepStatus::Skipped`] and
    /// the run is marked [`RunStatus::Failure`].
    ///
    /// # Errors
    ///
    /// Returns [`FaberError::InvalidPipeline`] if the pipeline has no steps
    /// (this is also enforced by [`Pipeline::new`]).
    pub async fn run(&self, pipeline: &Pipeline) -> Result<RunReport, FaberError> {
        if pipeline.steps().is_empty() {
            return Err(FaberError::InvalidPipeline(
                "pipeline contains no steps".to_string(),
            ));
        }

        let started_at = Utc::now();
        let mut step_results = Vec::with_capacity(pipeline.steps().len());
        let mut aborted = false;

        for step in pipeline.steps() {
            if aborted {
                step_results.push(StepResult {
                    step_name: step.name.clone(),
                    status: StepStatus::Skipped,
                    output: None,
                });
                continue;
            }

            let (status, output) = self.execute_step_kind(&step.kind).await;
            let failed = status == StepStatus::Failed;
            step_results.push(StepResult {
                step_name: step.name.clone(),
                status,
                output,
            });
            if failed && step.fail_fast {
                aborted = true;
            }
        }

        let overall_status =
            if aborted || step_results.iter().any(|r| r.status == StepStatus::Failed) {
                RunStatus::Failure
            } else {
                RunStatus::Success
            };

        Ok(RunReport {
            run_id: Uuid::new_v4().to_string(),
            pipeline_name: pipeline.name().to_string(),
            started_at,
            finished_at: Utc::now(),
            status: overall_status,
            step_results,
        })
    }

    /// Execute a single step kind and return `(status, optional_output)`.
    async fn execute_step_kind(&self, kind: &StepKind) -> (StepStatus, Option<String>) {
        match kind {
            StepKind::Shell { command } => match spawn_shell(command).await {
                Ok((true, out)) => (StepStatus::Passed, Some(out)),
                Ok((false, out)) => (StepStatus::Failed, Some(out)),
                Err(e) => (StepStatus::Failed, Some(e.to_string())),
            },
            StepKind::AgentTool { tool, args } => {
                if let Some(dispatcher) = &self.tool_dispatcher {
                    match dispatcher.dispatch(tool, args).await {
                        Ok(result) => (StepStatus::Passed, Some(result.to_string())),
                        Err(e) => (StepStatus::Failed, Some(e.to_string())),
                    }
                } else {
                    (
                        StepStatus::Passed,
                        Some(format!("[stub] tool={tool} args={args}")),
                    )
                }
            }
            StepKind::EmitEvent { payload } => {
                if let Some(bus) = &self.event_bus {
                    match bus.emit(payload).await {
                        Ok(()) => (StepStatus::Passed, Some(format!("[emitted] {payload}"))),
                        Err(e) => (StepStatus::Failed, Some(e.to_string())),
                    }
                } else {
                    (
                        StepStatus::Passed,
                        Some(format!("[stub] emit {payload}")),
                    )
                }
            }
        }
    }
}

// ── spawn_shell ───────────────────────────────────────────────────────────────

/// Spawn `command` in a shell, capturing stdout + stderr.
///
/// Returns `Ok((success, combined_output))` where `success` is `true` when the
/// process exits with code 0.  On non-zero exit the combined output is prefixed
/// with `exit <code>\n`.
async fn spawn_shell(command: &str) -> Result<(bool, String), FaberError> {
    use tokio::process::Command;

    let output = if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", command]).output().await
    } else {
        Command::new("sh").args(["-c", command]).output().await
    }
    .map_err(FaberError::Io)?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut combined = String::new();
    if !stdout.is_empty() {
        combined.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }

    if output.status.success() {
        Ok((true, combined))
    } else {
        let code = output.status.code().unwrap_or(-1);
        Ok((false, format!("exit {code}\n{combined}")))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Pipeline, Step, StepKind};

    fn shell(name: &str) -> Step {
        Step::new(
            name,
            StepKind::Shell {
                command: format!("echo {name}"),
            },
        )
    }

    fn failing_shell(name: &str) -> Step {
        Step::new(
            name,
            StepKind::Shell {
                command: "exit 1".to_string(),
            },
        )
    }

    #[tokio::test]
    async fn successful_pipeline_produces_success_report() {
        let p = Pipeline::new("ci", vec![shell("build"), shell("test")]).unwrap();
        let report = CiRunner::new().run(&p).await.unwrap();
        assert!(report.is_success());
        assert_eq!(report.step_results.len(), 2);
        assert!(report
            .step_results
            .iter()
            .all(|r| r.status == StepStatus::Passed));
    }

    #[tokio::test]
    async fn shell_step_captures_stdout() {
        let p = Pipeline::new(
            "capture",
            vec![Step::new(
                "greet",
                StepKind::Shell {
                    command: "echo hello-world".to_string(),
                },
            )],
        )
        .unwrap();
        let report = CiRunner::new().run(&p).await.unwrap();
        assert!(report.is_success());
        let output = report.step_results[0].output.as_deref().unwrap_or("");
        assert!(output.contains("hello-world"), "stdout not captured: {output}");
    }

    #[tokio::test]
    async fn shell_step_fails_on_nonzero_exit() {
        let p = Pipeline::new(
            "fail",
            vec![Step::new(
                "bad",
                StepKind::Shell {
                    command: "exit 1".to_string(),
                },
            )],
        )
        .unwrap();
        let report = CiRunner::new().run(&p).await.unwrap();
        assert_eq!(report.status, RunStatus::Failure);
        assert_eq!(report.step_results[0].status, StepStatus::Failed);
        let output = report.step_results[0].output.as_deref().unwrap_or("");
        assert!(output.starts_with("exit 1"), "expected exit code in output: {output}");
    }

    #[tokio::test]
    async fn run_id_is_unique_across_runs() {
        let p = Pipeline::new("x", vec![shell("s")]).unwrap();
        let runner = CiRunner::new();
        let r1 = runner.run(&p).await.unwrap();
        let r2 = runner.run(&p).await.unwrap();
        assert_ne!(r1.run_id, r2.run_id);
    }

    #[tokio::test]
    async fn fail_fast_step_skips_subsequent_steps() {
        let p = Pipeline::new(
            "fail-fast",
            vec![
                shell("pass"),
                failing_shell("fail"),
                shell("skipped"),
            ],
        )
        .unwrap();
        let report = CiRunner::new().run(&p).await.unwrap();
        assert_eq!(report.status, RunStatus::Failure);
        assert_eq!(report.step_results[0].status, StepStatus::Passed);
        assert_eq!(report.step_results[1].status, StepStatus::Failed);
        assert_eq!(report.step_results[2].status, StepStatus::Skipped);
    }

    #[tokio::test]
    async fn non_fail_fast_step_allows_subsequent_steps() {
        let p = Pipeline::new(
            "no-fail-fast",
            vec![
                failing_shell("fail").with_fail_fast(false),
                shell("still-runs"),
            ],
        )
        .unwrap();
        let report = CiRunner::new().run(&p).await.unwrap();
        assert_eq!(report.status, RunStatus::Failure);
        assert_eq!(report.step_results[0].status, StepStatus::Failed);
        assert_eq!(report.step_results[1].status, StepStatus::Passed);
    }

    #[tokio::test]
    async fn agent_tool_stub_passes_without_dispatcher() {
        let p = Pipeline::new(
            "tool-run",
            vec![Step::new(
                "invoke",
                StepKind::AgentTool {
                    tool: "summarise".to_string(),
                    args: serde_json::json!({"text": "hello"}),
                },
            )],
        )
        .unwrap();
        let report = CiRunner::new().run(&p).await.unwrap();
        assert!(report.is_success());
        let output = report.step_results[0].output.as_deref().unwrap_or("");
        assert!(output.contains("[stub]"), "expected stub output: {output}");
    }

    #[tokio::test]
    async fn agent_tool_routed_through_dispatcher() {
        use std::sync::Mutex;

        struct RecordingDispatcher {
            calls: Mutex<Vec<String>>,
        }

        #[async_trait]
        impl ToolDispatcher for RecordingDispatcher {
            async fn dispatch(
                &self,
                tool: &str,
                _args: &serde_json::Value,
            ) -> Result<serde_json::Value, FaberError> {
                self.calls.lock().unwrap().push(tool.to_string());
                Ok(serde_json::json!({"ok": true}))
            }
        }

        let dispatcher = Arc::new(RecordingDispatcher {
            calls: Mutex::new(vec![]),
        });
        let runner = CiRunner::new().with_tool_dispatcher(dispatcher.clone());

        let p = Pipeline::new(
            "dispatch",
            vec![Step::new(
                "call-tool",
                StepKind::AgentTool {
                    tool: "my-tool".to_string(),
                    args: serde_json::json!({}),
                },
            )],
        )
        .unwrap();
        let report = runner.run(&p).await.unwrap();
        assert!(report.is_success());
        assert_eq!(dispatcher.calls.lock().unwrap().as_slice(), ["my-tool"]);
    }

    #[tokio::test]
    async fn failing_dispatcher_marks_step_failed() {
        struct FailingDispatcher;

        #[async_trait]
        impl ToolDispatcher for FailingDispatcher {
            async fn dispatch(
                &self,
                _tool: &str,
                _args: &serde_json::Value,
            ) -> Result<serde_json::Value, FaberError> {
                Err(FaberError::StepFailed {
                    step: "call-tool".to_string(),
                    reason: "tool unavailable".to_string(),
                })
            }
        }

        let runner = CiRunner::new().with_tool_dispatcher(Arc::new(FailingDispatcher));
        let p = Pipeline::new(
            "fail-tool",
            vec![Step::new(
                "call-tool",
                StepKind::AgentTool {
                    tool: "broken".to_string(),
                    args: serde_json::json!({}),
                },
            )],
        )
        .unwrap();
        let report = runner.run(&p).await.unwrap();
        assert_eq!(report.status, RunStatus::Failure);
        assert_eq!(report.step_results[0].status, StepStatus::Failed);
    }

    #[tokio::test]
    async fn emit_event_stub_passes_without_bus() {
        let p = Pipeline::new(
            "event-run",
            vec![Step::new(
                "notify",
                StepKind::EmitEvent {
                    payload: serde_json::json!({"msg": "done"}),
                },
            )],
        )
        .unwrap();
        let report = CiRunner::new().run(&p).await.unwrap();
        assert!(report.is_success());
        let output = report.step_results[0].output.as_deref().unwrap_or("");
        assert!(output.contains("[stub]"), "expected stub output: {output}");
    }

    #[tokio::test]
    async fn emit_event_routed_through_bus() {
        use std::sync::Mutex;

        struct RecordingBus {
            events: Mutex<Vec<serde_json::Value>>,
        }

        #[async_trait]
        impl EventBus for RecordingBus {
            async fn emit(&self, payload: &serde_json::Value) -> Result<(), FaberError> {
                self.events.lock().unwrap().push(payload.clone());
                Ok(())
            }
        }

        let bus = Arc::new(RecordingBus {
            events: Mutex::new(vec![]),
        });
        let runner = CiRunner::new().with_event_bus(bus.clone());

        let payload = serde_json::json!({"event": "build.done"});
        let p = Pipeline::new(
            "event-bus",
            vec![Step::new(
                "emit",
                StepKind::EmitEvent {
                    payload: payload.clone(),
                },
            )],
        )
        .unwrap();
        let report = runner.run(&p).await.unwrap();
        assert!(report.is_success());
        assert_eq!(bus.events.lock().unwrap()[0], payload);
    }

    #[tokio::test]
    async fn failing_bus_marks_step_failed() {
        struct FailingBus;

        #[async_trait]
        impl EventBus for FailingBus {
            async fn emit(&self, _payload: &serde_json::Value) -> Result<(), FaberError> {
                Err(FaberError::StepFailed {
                    step: "emit".to_string(),
                    reason: "bus offline".to_string(),
                })
            }
        }

        let runner = CiRunner::new().with_event_bus(Arc::new(FailingBus));
        let p = Pipeline::new(
            "fail-bus",
            vec![Step::new(
                "emit",
                StepKind::EmitEvent {
                    payload: serde_json::json!({}),
                },
            )],
        )
        .unwrap();
        let report = runner.run(&p).await.unwrap();
        assert_eq!(report.status, RunStatus::Failure);
        assert_eq!(report.step_results[0].status, StepStatus::Failed);
    }
}
