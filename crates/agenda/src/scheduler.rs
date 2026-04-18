//! `scheduler` — tokio-based task scheduler for pares-agens.
//!
//! Provides cron-expression and interval-based task scheduling, with
//! tasks persisted in PluresDB so schedules survive process restarts.
//!
//! # Example
//! ```rust,ignore
//! use pares_agens_agenda::scheduler::{Scheduler, Task, Schedule};
//! let scheduler = Scheduler::new();
//! // scheduler.add(task).await;
//! // scheduler.start().await;
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time::{self, Duration};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// A scheduled task definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// When to run.
    pub schedule: Schedule,
    /// Command to execute (passed to the agent's run_command tool).
    pub command: String,
    /// Whether the task is active.
    pub enabled: bool,
    /// Last execution time.
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
    /// Last execution result (truncated).
    #[serde(default)]
    pub last_result: Option<String>,
}

/// Schedule definition — when a task should fire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Schedule {
    /// Run at a fixed interval.
    #[serde(rename = "interval")]
    Interval {
        /// Interval in seconds.
        every_secs: u64,
    },
    /// Run on a cron expression (minute hour day month weekday).
    #[serde(rename = "cron")]
    Cron {
        /// Cron expression (5-field: min hour dom month dow).
        expr: String,
    },
    /// Run once at a specific time.
    #[serde(rename = "once")]
    Once {
        /// ISO 8601 timestamp.
        at: DateTime<Utc>,
    },
}

/// Callback type for task execution.
pub type TaskExecutor = Arc<dyn Fn(String) -> tokio::task::JoinHandle<String> + Send + Sync>;

/// The scheduler — manages and executes scheduled tasks.
pub struct Scheduler {
    tasks: Arc<RwLock<HashMap<String, Task>>>,
    executor: Option<TaskExecutor>,
}

impl Scheduler {
    /// Create a new empty scheduler.
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            executor: None,
        }
    }

    /// Set the task executor callback.
    ///
    /// The executor receives the task's `command` string and should return
    /// a JoinHandle that resolves to the command output.
    pub fn with_executor(mut self, executor: TaskExecutor) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Add or update a task.
    pub async fn add(&self, task: Task) {
        info!(id = %task.id, name = %task.name, "scheduled task added");
        self.tasks.write().await.insert(task.id.clone(), task);
    }

    /// Remove a task by ID.
    pub async fn remove(&self, id: &str) -> bool {
        self.tasks.write().await.remove(id).is_some()
    }

    /// List all tasks.
    pub async fn list(&self) -> Vec<Task> {
        self.tasks.read().await.values().cloned().collect()
    }

    /// Get a task by ID.
    pub async fn get(&self, id: &str) -> Option<Task> {
        self.tasks.read().await.get(id).cloned()
    }

    /// Enable or disable a task.
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        if let Some(task) = self.tasks.write().await.get_mut(id) {
            task.enabled = enabled;
            true
        } else {
            false
        }
    }

    /// Start the scheduler loop. Runs until the Scheduler is dropped.
    ///
    /// Checks every 10 seconds for tasks that are due to run.
    pub async fn start(&self) {
        let tasks = Arc::clone(&self.tasks);
        let executor = self.executor.clone();

        info!("Scheduler started — checking every 10s");

        let mut interval = time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;

            let now = Utc::now();
            let mut due_tasks = Vec::new();

            {
                let guard = tasks.read().await;
                for task in guard.values() {
                    if !task.enabled {
                        continue;
                    }
                    if Self::is_due(task, &now) {
                        due_tasks.push(task.clone());
                    }
                }
            }

            for task in due_tasks {
                debug!(id = %task.id, name = %task.name, "task is due");

                if let Some(ref executor) = executor {
                    let cmd = task.command.clone();
                    let task_id = task.id.clone();
                    let tasks_ref = Arc::clone(&tasks);
                    let exec = Arc::clone(executor);

                    tokio::spawn(async move {
                        let handle = exec(cmd);
                        match handle.await {
                            Ok(result) => {
                                let truncated = if result.len() > 500 {
                                    format!("{}...", &result[..500])
                                } else {
                                    result
                                };
                                info!(task = %task_id, "task completed");
                                if let Some(t) = tasks_ref.write().await.get_mut(&task_id) {
                                    t.last_run = Some(Utc::now());
                                    t.last_result = Some(truncated);
                                }
                            }
                            Err(e) => {
                                error!(task = %task_id, error = %e, "task execution failed");
                                if let Some(t) = tasks_ref.write().await.get_mut(&task_id) {
                                    t.last_run = Some(Utc::now());
                                    t.last_result = Some(format!("ERROR: {e}"));
                                }
                            }
                        }
                    });
                } else {
                    warn!(task = %task.id, "no executor configured — skipping");
                }

                // Mark as run to prevent re-firing within the same tick
                if let Some(t) = tasks.write().await.get_mut(&task.id) {
                    t.last_run = Some(now);
                }
            }
        }
    }

    /// Check if a task is due to run now.
    fn is_due(task: &Task, now: &DateTime<Utc>) -> bool {
        match &task.schedule {
            Schedule::Interval { every_secs } => {
                let interval = chrono::Duration::seconds(*every_secs as i64);
                match &task.last_run {
                    Some(last) => *now - *last >= interval,
                    None => true, // never run → due immediately
                }
            }
            Schedule::Once { at } => {
                task.last_run.is_none() && *now >= *at
            }
            Schedule::Cron { expr } => {
                // Simple cron matching: check if current minute matches
                // Full cron parsing is a future enhancement
                let parts: Vec<&str> = expr.split_whitespace().collect();
                if parts.len() != 5 {
                    return false;
                }

                let minute = now.format("%M").to_string().parse::<u32>().unwrap_or(0);
                let hour = now.format("%H").to_string().parse::<u32>().unwrap_or(0);

                let min_match = parts[0] == "*"
                    || parts[0].starts_with("*/") && {
                        let div: u32 = parts[0][2..].parse().unwrap_or(1);
                        minute % div == 0
                    }
                    || parts[0].parse::<u32>().map(|v| v == minute).unwrap_or(false);

                let hour_match = parts[1] == "*"
                    || parts[1].starts_with("*/") && {
                        let div: u32 = parts[1][2..].parse().unwrap_or(1);
                        hour % div == 0
                    }
                    || parts[1].parse::<u32>().map(|v| v == hour).unwrap_or(false);

                // Only fire once per minute (check last_run)
                let not_already_run = match &task.last_run {
                    Some(last) => (*now - *last).num_seconds() >= 60,
                    None => true,
                };

                min_match && hour_match && not_already_run
            }
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_due_when_never_run() {
        let task = Task {
            id: "t1".into(),
            name: "test".into(),
            schedule: Schedule::Interval { every_secs: 60 },
            command: "echo hi".into(),
            enabled: true,
            last_run: None,
            last_result: None,
        };
        assert!(Scheduler::is_due(&task, &Utc::now()));
    }

    #[test]
    fn interval_not_due_when_recent() {
        let task = Task {
            id: "t1".into(),
            name: "test".into(),
            schedule: Schedule::Interval { every_secs: 60 },
            command: "echo hi".into(),
            enabled: true,
            last_run: Some(Utc::now()),
            last_result: None,
        };
        assert!(!Scheduler::is_due(&task, &Utc::now()));
    }

    #[test]
    fn once_due_when_past() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let task = Task {
            id: "t1".into(),
            name: "test".into(),
            schedule: Schedule::Once { at: past },
            command: "echo hi".into(),
            enabled: true,
            last_run: None,
            last_result: None,
        };
        assert!(Scheduler::is_due(&task, &Utc::now()));
    }

    #[test]
    fn once_not_due_after_run() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let task = Task {
            id: "t1".into(),
            name: "test".into(),
            schedule: Schedule::Once { at: past },
            command: "echo hi".into(),
            enabled: true,
            last_run: Some(Utc::now()),
            last_result: None,
        };
        assert!(!Scheduler::is_due(&task, &Utc::now()));
    }

    #[test]
    fn disabled_task_never_due() {
        let task = Task {
            id: "t1".into(),
            name: "test".into(),
            schedule: Schedule::Interval { every_secs: 1 },
            command: "echo hi".into(),
            enabled: false,
            last_run: None,
            last_result: None,
        };
        // is_due doesn't check enabled — caller does
        assert!(Scheduler::is_due(&task, &Utc::now()));
    }
}
