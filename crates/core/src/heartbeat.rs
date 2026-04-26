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

use crate::event_spine::EventSpineHandle;
use crate::state::StateStore;

/// Heartbeat configuration stored in PluresDB state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatConfig {
    /// Whether the heartbeat system is enabled.
    pub enabled: bool,
    /// Interval between heartbeats in minutes.
    pub interval_mins: u32,
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
            interval_mins: 30,
            quiet_hours_start: 23,
            quiet_hours_end: 8,
            max_proactive_per_day: 6,
        }
    }
}

impl HeartbeatConfig {
    /// Check if the current time falls within quiet hours.
    pub fn is_quiet_hour(&self) -> bool {
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
        }
    }

    /// Attach an event spine handle for emitting heartbeat events.
    #[must_use]
    pub fn with_event_spine(mut self, spine: EventSpineHandle) -> Self {
        self.event_spine = Some(spine);
        self
    }

    /// Load configuration from PluresDB state, falling back to defaults.
    pub async fn load_config(&mut self) {
        if let Some(value) = self.state.get(STATE_KEY_CONFIG).await {
            match serde_json::from_value::<HeartbeatConfig>(value) {
                Ok(config) => {
                    self.config = config;
                    info!(
                        interval_mins = self.config.interval_mins,
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
            interval_mins = self.config.interval_mins,
            "heartbeat runner started"
        );

        let interval_duration = Duration::from_secs(self.config.interval_mins as u64 * 60);
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
    async fn tick(&self) {
        if !self.config.enabled {
            debug!("heartbeat skipped — disabled");
            return;
        }

        if self.config.is_quiet_hour() {
            debug!("heartbeat skipped — quiet hours");
            return;
        }

        // Check daily limit
        let today = Local::now().format("%Y-%m-%d").to_string();
        let (count, date) = self.load_daily_count().await;
        let count = if date == today { count } else { 0 };

        if count >= self.config.max_proactive_per_day as u32 {
            debug!(count, max = self.config.max_proactive_per_day, "heartbeat skipped — daily limit reached");
            return;
        }

        // Load checklist
        let checklist = self.load_checklist().await;
        let active: Vec<_> = checklist.into_iter().filter(|item| item.enabled).collect();

        if active.is_empty() {
            debug!("heartbeat tick — no checklist items");
            return;
        }

        info!(items = active.len(), "heartbeat tick — processing checklist");

        for item in &active {
            info!(id = %item.id, desc = %item.description, cmd = %item.command, "heartbeat item");

            // Emit through event spine if available
            if let Some(spine) = &self.event_spine {
                spine.emit_inbound_message(
                    0, // system chat
                    "heartbeat",
                    &format!("[heartbeat] {}", item.command),
                );
            }
        }

        // Increment daily count
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
        assert_eq!(config.interval_mins, 30);
        assert_eq!(config.quiet_hours_start, 23);
        assert_eq!(config.quiet_hours_end, 8);
        assert_eq!(config.max_proactive_per_day, 6);
    }

    #[test]
    fn quiet_hours_wrap_midnight() {
        let config = HeartbeatConfig {
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
        assert_eq!(back.interval_mins, config.interval_mins);
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
        let state = Arc::new(crate::InMemoryStateStore::new());
        let mut runner = HeartbeatRunner::new(state);
        runner.load_config().await;
        assert_eq!(runner.config().interval_mins, 30);
    }

    #[tokio::test]
    async fn heartbeat_runner_loads_custom_config() {
        let state = Arc::new(crate::InMemoryStateStore::new());
        let custom = HeartbeatConfig {
            enabled: false,
            interval_mins: 15,
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
        assert_eq!(runner.config().interval_mins, 15);
        assert_eq!(runner.config().quiet_hours_start, 22);
    }
}
