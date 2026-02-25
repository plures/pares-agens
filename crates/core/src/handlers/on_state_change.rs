use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tracing::info;

use crate::{event::Event, procedure::Procedure};

/// An action fired when a watched state key changes.
#[async_trait]
pub trait WatcherAction: Send + Sync {
    /// Called when the watched key changes.
    ///
    /// Receives the full [`Event::StateChange`] so implementations can inspect
    /// the old and new values.
    async fn on_change(&self, event: &Event) -> Vec<Event>;
}

/// A registered state watcher: a (key, action) pair.
struct StateWatcher {
    /// The exact PluresDB key to watch.
    key: String,
    action: Arc<dyn WatcherAction>,
}

/// Built-in `on_state_change` procedure.
///
/// When a [`Event::StateChange`] is received:
/// 1. **Match watchers** — find all watchers registered for the changed key.
/// 2. **Fire procedures** — execute each matching watcher action and collect
///    their emitted events.
pub struct OnStateChange {
    watchers: Arc<RwLock<Vec<StateWatcher>>>,
}

impl OnStateChange {
    /// Create a new `OnStateChange` procedure with no registered watchers.
    pub fn new() -> Self {
        Self {
            watchers: Arc::new(RwLock::new(vec![])),
        }
    }

    /// Register a watcher for the given key.
    ///
    /// Multiple watchers may be registered for the same key; all will fire.
    pub fn watch(&self, key: impl Into<String>, action: Arc<dyn WatcherAction>) {
        self.watchers
            .write()
            .expect("watcher lock poisoned")
            .push(StateWatcher {
                key: key.into(),
                action,
            });
    }
}

impl Default for OnStateChange {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Procedure for OnStateChange {
    fn name(&self) -> &str {
        "on_state_change"
    }

    fn handles(&self) -> &str {
        "state_change"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let Event::StateChange { key, .. } = event else {
            return vec![];
        };

        // ── Step 1: Match watchers ────────────────────────────────────────────
        let matching: Vec<Arc<dyn WatcherAction>> = {
            let guard = self.watchers.read().expect("watcher lock poisoned");
            guard
                .iter()
                .filter(|w| w.key == key.as_str())
                .map(|w| Arc::clone(&w.action))
                .collect()
        };

        info!(
            key,
            matches = matching.len(),
            "on_state_change: dispatching to watchers"
        );

        // ── Step 2: Fire procedures ───────────────────────────────────────────
        let mut follow_ups = Vec::new();
        for action in matching {
            follow_ups.extend(action.on_change(event).await);
        }

        follow_ups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingAction {
        received: Mutex<Vec<Event>>,
    }

    impl RecordingAction {
        fn new() -> Self {
            Self {
                received: Mutex::new(vec![]),
            }
        }

        fn received_count(&self) -> usize {
            self.received.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl WatcherAction for RecordingAction {
        async fn on_change(&self, event: &Event) -> Vec<Event> {
            self.received.lock().unwrap().push(event.clone());
            vec![]
        }
    }

    struct EmitAction(Vec<Event>);

    #[async_trait]
    impl WatcherAction for EmitAction {
        async fn on_change(&self, _event: &Event) -> Vec<Event> {
            self.0.clone()
        }
    }

    fn state_change(key: &str, new_value: serde_json::Value) -> Event {
        Event::StateChange {
            key: key.into(),
            old_value: None,
            new_value,
        }
    }

    #[tokio::test]
    async fn fires_watcher_for_matching_key() {
        let recorder = Arc::new(RecordingAction::new());
        let handler = OnStateChange::new();
        handler.watch("mood", recorder.clone());

        handler
            .execute(&state_change("mood", serde_json::json!("happy")))
            .await;

        assert_eq!(recorder.received_count(), 1);
    }

    #[tokio::test]
    async fn does_not_fire_for_non_matching_key() {
        let recorder = Arc::new(RecordingAction::new());
        let handler = OnStateChange::new();
        handler.watch("mood", recorder.clone());

        handler
            .execute(&state_change("status", serde_json::json!("online")))
            .await;

        assert_eq!(recorder.received_count(), 0);
    }

    #[tokio::test]
    async fn fires_multiple_watchers_for_same_key() {
        let r1 = Arc::new(RecordingAction::new());
        let r2 = Arc::new(RecordingAction::new());
        let handler = OnStateChange::new();
        handler.watch("key", r1.clone());
        handler.watch("key", r2.clone());

        handler
            .execute(&state_change("key", serde_json::json!(42)))
            .await;

        assert_eq!(r1.received_count(), 1);
        assert_eq!(r2.received_count(), 1);
    }

    #[tokio::test]
    async fn collects_follow_up_events_from_watchers() {
        let emit = Event::Message {
            id: "sc1".into(),
            channel: "alerts".into(),
            sender: "watcher".into(),
            content: "state changed!".into(),
        };
        let handler = OnStateChange::new();
        handler.watch("alarm", Arc::new(EmitAction(vec![emit.clone()])));

        let follow_ups = handler
            .execute(&state_change("alarm", serde_json::json!(true)))
            .await;

        assert_eq!(follow_ups.len(), 1);
        assert_eq!(follow_ups[0], emit);
    }

    #[tokio::test]
    async fn ignores_non_state_change_events() {
        let recorder = Arc::new(RecordingAction::new());
        let handler = OnStateChange::new();
        handler.watch("anything", recorder.clone());

        let msg = Event::Message {
            id: "m".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "hi".into(),
        };
        handler.execute(&msg).await;

        assert_eq!(recorder.received_count(), 0);
    }

    #[tokio::test]
    async fn returns_empty_when_no_watchers_registered() {
        let handler = OnStateChange::new();
        let follow_ups = handler
            .execute(&state_change("some_key", serde_json::json!("value")))
            .await;
        assert!(follow_ups.is_empty());
    }
}
