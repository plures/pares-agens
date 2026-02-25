use async_trait::async_trait;

use crate::{event::Event, procedure::Procedure};

/// Built-in `on_timer` procedure.
///
/// Stub implementation: logs the timer name and returns no follow-up events.
/// A real implementation would look up the handler in PluresDB, execute it,
/// and reschedule recurring timers.
pub struct OnTimer;

#[async_trait]
impl Procedure for OnTimer {
    fn name(&self) -> &str {
        "on_timer"
    }

    fn handles(&self) -> &str {
        "timer"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        if let Event::Timer { id, name, recurring } = event {
            tracing::info!(id, name, recurring, "on_timer: timer fired");
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedure::Procedure;

    #[tokio::test]
    async fn on_timer_returns_no_follow_ups() {
        let handler = OnTimer;
        let event = Event::Timer {
            id: "t1".into(),
            name: "daily-summary".into(),
            recurring: true,
        };
        let result = handler.execute(&event).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn on_timer_ignores_non_timer_events() {
        let handler = OnTimer;
        let msg = Event::Message {
            id: "1".into(),
            channel: "stdin".into(),
            sender: "user".into(),
            content: "hi".into(),
        };
        let result = handler.execute(&msg).await;
        assert!(result.is_empty());
    }
}
