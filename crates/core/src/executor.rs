use tracing::{debug, info, warn};

use crate::{
    event::Event, 
    optimization::OptimizationSafetyGate, 
    procedure::ProcedureRegistry, 
    source::EventSource
};

/// Drives the reactive event loop with optimization safety enforcement.
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
    safety_gate: OptimizationSafetyGate,
}

impl Executor {
    /// Create a new executor with the given procedure registry.
    pub fn new(registry: ProcedureRegistry) -> Self {
        Self { 
            registry,
            safety_gate: OptimizationSafetyGate::new(),
        }
    }

    /// Create a new executor with custom safety gate.
    pub fn with_safety_gate(registry: ProcedureRegistry, safety_gate: OptimizationSafetyGate) -> Self {
        Self {
            registry,
            safety_gate,
        }
    }

    /// Get a reference to the safety gate for external access.
    pub fn safety_gate(&self) -> &OptimizationSafetyGate {
        &self.safety_gate
    }

    /// Dispatch a single event to every matching procedure and return all
    /// emitted follow-up events with safety enforcement.
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
            let procedure_name = handler.name();
            info!(procedure = procedure_name, kind, "executing procedure with safety check");
            
            // Apply optimization safety check
            let action = format!("execute_procedure:{}", procedure_name);
            let safety = self.safety_gate.check_optimization_safety(&action);
            
            match safety {
                crate::optimization::OptimizationSafety::Ready => {
                    info!(procedure = procedure_name, "procedure execution permitted");
                    let emitted = handler.execute(event).await;
                    follow_ups.extend(emitted);
                }
                crate::optimization::OptimizationSafety::InsufficientData => {
                    let evidence_req = self.safety_gate.request_evidence(
                        format!("Insufficient data for procedure: {}", procedure_name),
                        vec!["safety_metrics".into(), "execution_context".into()],
                        action.clone(),
                    );
                    let telemetry = crate::optimization::OptimizationTelemetry::new(
                        &action,
                        safety.clone(),
                        Some(evidence_req.id.clone()),
                    );
                    self.safety_gate.record_telemetry(telemetry);
                    
                    warn!(
                        procedure = procedure_name,
                        evidence_request_id = %evidence_req.id,
                        "procedure execution blocked: insufficient data"
                    );
                }
                crate::optimization::OptimizationSafety::UnsafeSolution => {
                    let telemetry = crate::optimization::OptimizationTelemetry::new(
                        &action,
                        safety.clone(),
                        None,
                    );
                    self.safety_gate.record_telemetry(telemetry);
                    
                    warn!(
                        procedure = procedure_name,
                        "procedure execution blocked: unsafe solution"
                    );
                }
            }
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
