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
    /// **Follow-up events**: the `Vec<Event>` returned by each [`Procedure`]
    /// represents outbound/derived events (e.g. a response message, a timer
    /// reschedule).  The current implementation does **not** re-feed these
    /// events into the loop; handlers that need their follow-ups processed
    /// (e.g. a timer reschedule) should write them back to PluresDB so they
    /// are picked up by the next [`EventSource::poll_events`] call.
    ///
    /// [`Procedure`]: crate::procedure::Procedure
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
