use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{event::Event, procedure::Procedure};

/// An async action invoked when a named timer fires.
#[async_trait]
pub trait TimerAction: Send + Sync {
    /// Execute the timer action and return any follow-up events.
    async fn execute(&self) -> Vec<Event>;
}

/// Built-in `on_timer` procedure.
///
/// When a [`Event::Timer`] fires:
/// 1. **Lookup** — find the registered action for that timer name.
/// 2. **Execute** — run the action and collect follow-up events.
/// 3. **Reschedule** — if the timer is recurring, emit a new [`Event::Timer`]
///    so the event loop schedules the next tick.
pub struct OnTimer {
    handlers: Arc<RwLock<HashMap<String, Arc<dyn TimerAction>>>>,
}

impl OnTimer {
    /// Create a new `OnTimer` procedure with no registered handlers.
    pub fn new() -> Self {
        Self {
            handlers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an action for the given timer name.
    ///
    /// If a handler for that name already exists it is replaced.
    pub fn register(&self, name: impl Into<String>, action: Arc<dyn TimerAction>) {
        self.handlers
            .write()
            .expect("timer handler lock poisoned")
            .insert(name.into(), action);
    }
}

impl Default for OnTimer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Procedure for OnTimer {
    fn name(&self) -> &str {
        "on_timer"
    }

    fn handles(&self) -> &str {
        "timer"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let Event::Timer {
            id,
            name,
            recurring,
        } = event
        else {
            return vec![];
        };

        info!(id, name, recurring, "on_timer: timer fired");

        // ── Step 1: Lookup ────────────────────────────────────────────────────
        let action = {
            let guard = self.handlers.read().expect("timer handler lock poisoned");
            guard.get(name.as_str()).cloned()
        };

        let Some(action) = action else {
            warn!(name, "on_timer: no handler registered for timer");
            return vec![];
        };

        // ── Step 2: Execute ───────────────────────────────────────────────────
        let mut follow_ups = action.execute().await;

        // ── Step 3: Reschedule if recurring ───────────────────────────────────
        if *recurring {
            info!(name, "on_timer: rescheduling recurring timer");
            follow_ups.push(Event::Timer {
                id: Uuid::new_v4().to_string(),
                name: name.clone(),
                recurring: true,
            });
        }

        follow_ups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CountAction {
        invocations: Arc<Mutex<usize>>,
    }

    impl CountAction {
        fn new(counter: Arc<Mutex<usize>>) -> Self {
            Self {
                invocations: counter,
            }
        }
    }

    #[async_trait]
    impl TimerAction for CountAction {
        async fn execute(&self) -> Vec<Event> {
            *self.invocations.lock().unwrap() += 1;
            vec![]
        }
    }

    struct EmitAction(Vec<Event>);

    #[async_trait]
    impl TimerAction for EmitAction {
        async fn execute(&self) -> Vec<Event> {
            self.0.clone()
        }
    }

    fn timer(name: &str, recurring: bool) -> Event {
        Event::Timer {
            id: "t1".into(),
            name: name.into(),
            recurring,
        }
    }

    #[tokio::test]
    async fn invokes_registered_handler() {
        let counter = Arc::new(Mutex::new(0usize));
        let on_timer = OnTimer::new();
        on_timer.register("daily", Arc::new(CountAction::new(counter.clone())));

        on_timer.execute(&timer("daily", false)).await;

        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn no_follow_up_for_non_recurring_timer() {
        let on_timer = OnTimer::new();
        on_timer.register("once", Arc::new(EmitAction(vec![])));

        let follow_ups = on_timer.execute(&timer("once", false)).await;

        assert!(
            follow_ups.is_empty(),
            "non-recurring timer should not emit a reschedule event"
        );
    }

    #[tokio::test]
    async fn reschedules_recurring_timer() {
        let on_timer = OnTimer::new();
        on_timer.register("tick", Arc::new(EmitAction(vec![])));

        let follow_ups = on_timer.execute(&timer("tick", true)).await;

        assert_eq!(
            follow_ups.len(),
            1,
            "recurring timer must emit one reschedule event"
        );
        if let Event::Timer {
            name, recurring, ..
        } = &follow_ups[0]
        {
            assert_eq!(name, "tick");
            assert!(recurring);
        } else {
            panic!("expected Timer reschedule event");
        }
    }

    #[tokio::test]
    async fn handler_follow_ups_included_before_reschedule() {
        let msg = Event::Message {
            id: "x".into(),
            channel: "c".into(),
            sender: "timer".into(),
            content: "summary ready".into(),
        };
        let on_timer = OnTimer::new();
        on_timer.register("summary", Arc::new(EmitAction(vec![msg.clone()])));

        let follow_ups = on_timer.execute(&timer("summary", true)).await;

        assert_eq!(follow_ups.len(), 2);
        assert_eq!(follow_ups[0], msg);
        assert!(matches!(follow_ups[1], Event::Timer { .. }));
    }

    #[tokio::test]
    async fn returns_empty_for_unknown_timer() {
        let on_timer = OnTimer::new();
        let follow_ups = on_timer.execute(&timer("unknown", false)).await;
        assert!(follow_ups.is_empty());
    }

    #[tokio::test]
    async fn ignores_non_timer_events() {
        let on_timer = OnTimer::new();
        let msg = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "hi".into(),
        };
        let follow_ups = on_timer.execute(&msg).await;
        assert!(follow_ups.is_empty());
    }
}
