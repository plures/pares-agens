use tracing::{debug, info, warn};

use crate::{event::Event, procedure::ProcedureRegistry, source::EventSource};

/// Drives the reactive event loop.
///
/// ```text
/// loop {
///     let events = source.poll_events().await;
///     for event in events {
///         executor.dispatch(event).await;
///     }
/// }
/// ```
pub struct Executor {
    registry: ProcedureRegistry,
}

impl Executor {
    /// Create a new executor with the given procedure registry.
    pub fn new(registry: ProcedureRegistry) -> Self {
        Self { registry }
    }

    /// Dispatch a single event to every matching procedure and return all
    /// emitted follow-up events.
    pub async fn dispatch(&self, event: &Event) -> Vec<Event> {
        let kind = event.kind();
        let mut follow_ups: Vec<Event> = Vec::new();

        let handlers: Vec<&dyn crate::procedure::Procedure> =
            self.registry.matching(kind).collect();

        if handlers.is_empty() {
            debug!(kind, "no procedures registered for event");
            return follow_ups;
        }

        for handler in handlers {
            info!(procedure = handler.name(), kind, "executing procedure");
            let emitted = handler.execute(event).await;
            follow_ups.extend(emitted);
        }

        follow_ups
    }

    /// Run the event loop until the source returns no events for one poll or
    /// `max_iterations` ticks have been processed (0 = unlimited).
    ///
    /// This is intentionally kept simple for the initial implementation;
    /// production usage will add cancellation tokens and back-off.
    pub async fn run(&self, source: &dyn EventSource, max_iterations: usize) {
        let mut iterations = 0usize;
        loop {
            let events = source.poll_events().await;

            if events.is_empty() {
                debug!("no events, stopping loop");
                break;
            }

            for event in events {
                // Process the initial event and any follow-up events it emits.
                let mut pending = vec![event];
                while let Some(current) = pending.pop() {
                    let follow_ups = self.dispatch(&current).await;
                    pending.extend(follow_ups);
                }
            }

            iterations += 1;
            if max_iterations > 0 && iterations >= max_iterations {
                warn!(
                    iterations,
                    "reached max_iterations, stopping event loop"
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::{
        event::Event,
        procedure::{Procedure, ProcedureRegistry},
        source::EventSource,
    };
    use std::sync::{Arc, Mutex};

    /// Procedure that records every event it handles.
    struct RecordingProcedure {
        handled: Arc<Mutex<Vec<Event>>>,
    }

    #[async_trait]
    impl Procedure for RecordingProcedure {
        fn name(&self) -> &str {
            "recording"
        }

        fn handles(&self) -> &str {
            "message"
        }

        async fn execute(&self, event: &Event) -> Vec<Event> {
            self.handled.lock().unwrap().push(event.clone());
            vec![]
        }
    }

    fn make_message(content: &str) -> Event {
        Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "alice".into(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn dispatch_routes_to_matching_procedure() {
        let handled = Arc::new(Mutex::new(vec![]));
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(RecordingProcedure {
            handled: handled.clone(),
        }));
        let executor = Executor::new(registry);

        let event = make_message("hello");
        executor.dispatch(&event).await;

        let seen = handled.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], event);
    }

    #[tokio::test]
    async fn dispatch_ignores_unregistered_kinds() {
        let registry = ProcedureRegistry::new();
        let executor = Executor::new(registry);

        let timer = Event::Timer {
            id: "t1".into(),
            name: "daily".into(),
            recurring: false,
        };
        // Should not panic; just returns empty.
        let follow_ups = executor.dispatch(&timer).await;
        assert!(follow_ups.is_empty());
    }

    struct FiniteSource {
        events: Mutex<Vec<Vec<Event>>>,
    }

    #[async_trait]
    impl EventSource for FiniteSource {
        async fn poll_events(&self) -> Vec<Event> {
            self.events.lock().unwrap().pop().unwrap_or_default()
        }
    }

    #[tokio::test]
    async fn run_stops_when_source_is_empty() {
        let source = FiniteSource {
            events: Mutex::new(vec![vec![make_message("a")]]),
        };
        let registry = ProcedureRegistry::new();
        let executor = Executor::new(registry);

        // Should complete without hanging.
        executor.run(&source, 0).await;
    }
}
