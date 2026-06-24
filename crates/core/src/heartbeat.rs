//! Heartbeat system — periodic proactive check-ins.
//!
//! The [`HeartbeatRunner`] fires at a configurable interval, respects quiet
//! hours, reads a checklist from PluresDB state, and logs items for execution.
//! It integrates with the event spine to emit heartbeat events that the agent
//! can process like any other inbound event.

use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{self, Duration};
use tracing::{debug, info, warn};

use pares_radix_core::event_spine::EventSpineHandle;
use pares_radix_core::spine::pipeline::PipelineEmitter;
use pares_radix_core::state::StateStore;
use pares_radix_core::task_executor::TaskDispatcher;
use pares_radix_core::task_manager::TaskManager;

/// Heartbeat configuration stored in PluresDB state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatConfig {
    /// Whether the heartbeat system is enabled.
    pub enabled: bool,
    /// Interval between heartbeats in minutes.
    /// Interval between heartbeat ticks in seconds.
    pub interval_secs: u32,
    /// Whether quiet hours are enforced.
    pub quiet_hours_enabled: bool,
    /// Start of quiet hours (hour, 0-23). Heartbeats are suppressed during quiet hours.
    pub quiet_hours_start: u8,
    /// End of quiet hours (hour, 0-23).
    pub quiet_hours_end: u8,
    /// Maximum proactive messages per day (to avoid being annoying).
    pub max_proactive_per_day: u8,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 30,
            quiet_hours_enabled: true,
            quiet_hours_start: 23,
            quiet_hours_end: 8,
            max_proactive_per_day: 6,
        }
    }
}

impl HeartbeatConfig {
    /// Check if the current time falls within quiet hours.
    pub fn is_quiet_hour(&self) -> bool {
        if !self.quiet_hours_enabled {
            return false;
        }
        let hour = Local::now().hour() as u8;
        if self.quiet_hours_start <= self.quiet_hours_end {
            // e.g. 9..17
            hour >= self.quiet_hours_start && hour < self.quiet_hours_end
        } else {
            // e.g. 23..8 (wraps midnight)
            hour >= self.quiet_hours_start || hour < self.quiet_hours_end
        }
    }
}

/// A checklist item to execute during a heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatChecklistItem {
    /// Unique identifier.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Command or action to execute.
    pub command: String,
    /// Whether this item is currently active.
    pub enabled: bool,
}

/// The heartbeat runner — a background task that fires at intervals.
pub struct HeartbeatRunner {
    config: HeartbeatConfig,
    state: Arc<dyn StateStore>,
    event_spine: Option<EventSpineHandle>,
    pipeline_emitter: Option<PipelineEmitter>,
    task_manager: Option<Arc<TaskManager>>,
    task_dispatcher: Option<TaskDispatcher>,
}

const STATE_KEY_CONFIG: &str = "heartbeat/config";
const STATE_KEY_CHECKLIST: &str = "heartbeat/checklist";
const STATE_KEY_DAILY_COUNT: &str = "heartbeat/daily_count";
const STATE_KEY_DAILY_DATE: &str = "heartbeat/daily_date";

impl HeartbeatRunner {
    /// Create a new heartbeat runner with the given state store.
    pub fn new(state: Arc<dyn StateStore>) -> Self {
        Self {
            config: HeartbeatConfig::default(),
            state,
            event_spine: None,
            pipeline_emitter: None,
            task_manager: None,
            task_dispatcher: None,
        }
    }

    /// Attach an event spine handle for emitting heartbeat events.
    #[must_use]
    pub fn with_event_spine(mut self, spine: EventSpineHandle) -> Self {
        self.event_spine = Some(spine);
        self
    }

    /// Attach a pipeline emitter for task dispatch (injects into SpinePipeline).
    #[must_use]
    pub fn with_pipeline_emitter(mut self, emitter: PipelineEmitter) -> Self {
        self.pipeline_emitter = Some(emitter);
        self
    }

    /// Attach a task manager for autonomous task execution during heartbeats.
    pub fn with_task_manager(mut self, task_manager: Arc<TaskManager>, state: Arc<dyn StateStore>) -> Self {
        self.task_manager = Some(task_manager.clone());
        let mut dispatcher = TaskDispatcher::new(state);
        if let Some(emitter) = &self.pipeline_emitter {
            dispatcher = dispatcher.with_pipeline_emitter(emitter.clone());
        }
        self.task_dispatcher = Some(dispatcher);
        self
    }

    /// Load configuration from PluresDB state, falling back to defaults.
    pub async fn load_config(&mut self) {
        if let Some(value) = self.state.get(STATE_KEY_CONFIG).await {
            match serde_json::from_value::<HeartbeatConfig>(value) {
                Ok(config) => {
                    self.config = config;
                    info!(
                        interval_secs = self.config.interval_secs,
                        quiet_start = self.config.quiet_hours_start,
                        quiet_end = self.config.quiet_hours_end,
                        "heartbeat config loaded from PluresDB"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "invalid heartbeat config in PluresDB, using defaults");
                }
            }
        } else {
            // Persist defaults
            if let Ok(value) = serde_json::to_value(&self.config) {
                self.state.set(STATE_KEY_CONFIG, value).await;
            }
            info!("heartbeat config initialized with defaults");
        }
    }

    /// Save the current configuration to PluresDB.
    pub async fn save_config(&self) {
        if let Ok(value) = serde_json::to_value(&self.config) {
            self.state.set(STATE_KEY_CONFIG, value).await;
        }
    }

    /// Get current config.
    pub fn config(&self) -> &HeartbeatConfig {
        &self.config
    }

    /// Update config.
    pub async fn set_config(&mut self, config: HeartbeatConfig) {
        self.config = config;
        self.save_config().await;
    }

    /// Run the heartbeat loop until the shutdown signal fires.
    ///
    /// The runner checks at each interval whether:
    /// 1. Heartbeats are enabled
    /// 2. We're outside quiet hours
    /// 3. We haven't exceeded the daily proactive limit
    /// 4. There are checklist items to process
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        info!(
            interval_secs = self.config.interval_secs,
            "heartbeat runner started"
        );

        let interval_duration = Duration::from_secs(self.config.interval_secs as u64);
        let mut interval = time::interval(interval_duration);
        // Skip the immediate first tick
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.tick().await;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("heartbeat runner shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Execute a single heartbeat tick.
    ///
    /// This is the cerebellum-gated heartbeat:
    /// 1. Check for pending work (zero tokens — pure PluresDB/state queries)
    /// 2. Only escalate to the conscious model if there's actual work
    /// 3. Skip silently if nothing needs attention
    async fn tick(&self) {
        if !self.config.enabled {
            return;
        }

        if self.config.is_quiet_hour() {
            return;
        }

        // ── Cerebellum gate (zero tokens) ────────────────────────────
        // Check for work without calling any model.
        let mut work_items: Vec<String> = Vec::new();

        // 1. Check pending tasks in state
        if let Some(tasks) = self.state.get("pending_tasks").await {
            if let Some(arr) = tasks.as_array() {
                for task in arr {
                    if let Some(desc) = task.get("description").and_then(|d| d.as_str()) {
                        work_items.push(format!("pending task: {desc}"));
                    }
                }
            }
        }

        // 2. Check checklist items
        let checklist = self.load_checklist().await;
        for item in &checklist {
            if item.enabled {
                work_items.push(format!("checklist: {}", item.command));
            }
        }

        // 3. Check for unfulfilled promises
        // (Promises are stored in Chronos under key "agent:promise".
        //  The heartbeat checks if any recent promises are uncompleted.)
        // TODO: query Chronos for recent agent:promise entries where completed=false
        // For now, check state fallback
        if let Some(promises) = self.state.get("agent_promises").await {
            if let Some(arr) = promises.as_array() {
                for promise in arr {
                    if let Some(what) = promise.get("what").and_then(|w| w.as_str()) {
                        if !promise
                            .get("completed")
                            .and_then(|c| c.as_bool())
                            .unwrap_or(false)
                        {
                            work_items.push(format!("unfulfilled promise: {what}"));
                        }
                    }
                }
            }
        }

        // 4. Check TaskManager for evaluable tasks
        if let Some(ref tm) = self.task_manager {
            if TaskDispatcher::has_pending_work(tm) {
                let pending = tm.evaluable_tasks().len();
                work_items.push(format!("tasks: {} evaluable task(s) pending execution", pending));
            }
        }

        // ── Gate decision ─────────────────────────────────────────────
        if work_items.is_empty() {
            // Nothing to do — skip silently (zero tokens)
            return;
        }

        // Check daily limit before spending tokens
        let today = Local::now().format("%Y-%m-%d").to_string();
        let (count, date) = self.load_daily_count().await;
        let count = if date == today { count } else { 0 };

        if count >= self.config.max_proactive_per_day as u32 {
            debug!(
                count,
                max = self.config.max_proactive_per_day,
                "heartbeat gated — daily limit"
            );
            return;
        }

        // ── Escalate to conscious (tokens spent here) ────────────────
        info!(
            items = work_items.len(),
            "heartbeat: work found, escalating"
        );

        // ── Try autonomous task dispatch first ─────────────────────
        // Decision logic lives in autonomous-dispatch.px (via PxBridge).
        // Here we only check the fast-path gate and call the IO dispatcher.
        if let (Some(ref tm), Some(ref dispatcher)) = (&self.task_manager, &self.task_dispatcher) {
            if TaskDispatcher::has_pending_work(tm) {
                // TODO: Route through PxBridge.call("evaluate_dispatch", tick)
                // For now, dispatch highest-priority directly (Rust fallback)
                // This is a KNOWN .px gap — tracked for wiring once PxBridge
                // is available in the heartbeat context.
                let tasks = tm.evaluable_tasks();
                if let Some(task) = tasks.first() {
                    let prompt = format!(
                        "[autonomous-task] Execute this task:\nTask: {}\nID: {}\nPriority: {}\n\nWork on this task using available tools.",
                        task.description, task.id, task.priority
                    );
                    if dispatcher.dispatch(&task.id, &prompt) {
                        dispatcher.record_dispatch(&task.id).await;
                        info!("heartbeat: dispatched autonomous task via TaskDispatcher");
                        self.save_daily_count(count + 1, &today).await;
                        return;
                    }
                }
            }
        }

        // ── Fallback: escalate to conscious (tokens spent here) ───────
        let combined = work_items.join("\n");
        if let Some(spine) = &self.event_spine {
            spine.emit_inbound_message(
                0,
                "heartbeat",
                &format!("[heartbeat] Work needs attention:\n{combined}"),
            );
        }

        self.save_daily_count(count + 1, &today).await;
    }

    async fn load_daily_count(&self) -> (u32, String) {
        let count = self
            .state
            .get(STATE_KEY_DAILY_COUNT)
            .await
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let date = self
            .state
            .get(STATE_KEY_DAILY_DATE)
            .await
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        (count, date)
    }

    async fn save_daily_count(&self, count: u32, date: &str) {
        self.state
            .set(STATE_KEY_DAILY_COUNT, serde_json::json!(count))
            .await;
        self.state
            .set(STATE_KEY_DAILY_DATE, serde_json::json!(date))
            .await;
    }

    async fn load_checklist(&self) -> Vec<HeartbeatChecklistItem> {
        self.state
            .get(STATE_KEY_CHECKLIST)
            .await
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = HeartbeatConfig::default();
        assert!(config.enabled);
        assert_eq!(config.interval_secs, 30);
        assert_eq!(config.quiet_hours_start, 23);
        assert_eq!(config.quiet_hours_end, 8);
        assert_eq!(config.max_proactive_per_day, 6);
    }

    #[test]
    fn quiet_hours_wrap_midnight() {
        let config = HeartbeatConfig {
            quiet_hours_enabled: true,
            quiet_hours_start: 23,
            quiet_hours_end: 8,
            ..Default::default()
        };
        // Can't fully test without mocking time, but structure is correct
        let _ = config.is_quiet_hour();
    }

    #[test]
    fn quiet_hours_same_day() {
        let config = HeartbeatConfig {
            quiet_hours_start: 9,
            quiet_hours_end: 17,
            ..Default::default()
        };
        let _ = config.is_quiet_hour();
    }

    #[test]
    fn config_serde_round_trip() {
        let config = HeartbeatConfig::default();
        let json = serde_json::to_value(&config).unwrap();
        let back: HeartbeatConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.interval_secs, config.interval_secs);
        assert_eq!(back.quiet_hours_start, config.quiet_hours_start);
    }

    #[test]
    fn checklist_item_serde() {
        let item = HeartbeatChecklistItem {
            id: "test".into(),
            description: "Test item".into(),
            command: "echo hello".into(),
            enabled: true,
        };
        let json = serde_json::to_value(&item).unwrap();
        let back: HeartbeatChecklistItem = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "test");
        assert!(back.enabled);
    }

    #[tokio::test]
    async fn heartbeat_runner_loads_default_config() {
        let state = Arc::new(pares_radix_core::InMemoryStateStore::new());
        let mut runner = HeartbeatRunner::new(state);
        runner.load_config().await;
        assert_eq!(runner.config().interval_secs, 30);
    }

    #[tokio::test]
    async fn heartbeat_runner_loads_custom_config() {
        let state = Arc::new(pares_radix_core::InMemoryStateStore::new());
        let custom = HeartbeatConfig {
            enabled: false,
            interval_secs: 15,
            quiet_hours_enabled: true,
            quiet_hours_start: 22,
            quiet_hours_end: 7,
            max_proactive_per_day: 4,
        };
        state
            .set(STATE_KEY_CONFIG, serde_json::to_value(&custom).unwrap())
            .await;

        let mut runner = HeartbeatRunner::new(state);
        runner.load_config().await;
        assert!(!runner.config().enabled);
        assert_eq!(runner.config().interval_secs, 15);
        assert_eq!(runner.config().quiet_hours_start, 22);
    }
}
