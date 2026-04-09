use std::sync::Arc;

use tracing::{debug, info, warn};

use pares_agens_praxis::db::{
    procedures::on_action,
    schema::{AgentContext, SessionType},
    store::PraxisStore,
};

use crate::{
    event::Event, optimization::OptimizationSafetyGate, procedure::ProcedureRegistry,
    source::EventSource,
};

// ── PraxisGate ────────────────────────────────────────────────────────────────

/// Checks praxis pre-action constraints before a procedure is executed.
///
/// Implementors can plug in any constraint store (the default
/// [`DefaultPraxisGate`] uses a seeded [`pares_agens_praxis::db::PraxisStore`]).
pub trait PraxisGate: Send + Sync {
    /// Return `Ok(())` when the action is permitted, or `Err(reason)` when a
    /// blocking constraint fires.
    fn check(&self, action: &str) -> Result<(), String>;
}

/// Default implementation backed by a seeded [`PraxisStore`].
pub struct DefaultPraxisGate {
    store: pares_agens_praxis::db::PraxisStore,
}

impl DefaultPraxisGate {
    /// Create a gate pre-loaded with the built-in seeded constraints.
    pub fn new() -> Self {
        Self {
            store: pares_agens_praxis::db::seed::default_store(),
        }
    }

    /// Create a gate backed by a custom [`PraxisStore`].
    pub fn with_store(store: pares_agens_praxis::db::PraxisStore) -> Self {
        Self { store }
    }
}

impl Default for DefaultPraxisGate {
    fn default() -> Self {
        Self::new()
    }
}

impl PraxisGate for DefaultPraxisGate {
    fn check(&self, action: &str) -> Result<(), String> {
        use pares_agens_praxis::db::procedures::on_action;
        use pares_agens_praxis::db::{AgentContext, SessionType};

        // The second argument is the resource target; executor-level checks are
        // action-only and do not have a specific target resource.
        let ctx = AgentContext::new(action, "", SessionType::Main);
        on_action(&self.store, &ctx)
            .map(|_| ())
            .map_err(|blocked| blocked.to_string())
    }
}

/// A no-op [`PraxisGate`] that always permits actions.  Useful in tests or
/// when constraint enforcement is not desired.
pub struct NoopPraxisGate;

impl PraxisGate for NoopPraxisGate {
    fn check(&self, _action: &str) -> Result<(), String> {
        Ok(())
    }
}

// ── Executor ──────────────────────────────────────────────────────────────────
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
    praxis_gate: Box<dyn PraxisGate>,
    /// Optional praxis store used to enforce pre-action constraints via
    /// [`on_action`].  When `None` the constraint check is skipped and all
    /// procedures are allowed to proceed (existing behaviour).
    praxis_store: Option<Arc<PraxisStore>>,
}

impl Executor {
    /// Create a new executor with the given procedure registry.
    pub fn new(registry: ProcedureRegistry) -> Self {
        Self {
            registry,
            safety_gate: OptimizationSafetyGate::new(),
            praxis_gate: Box::new(DefaultPraxisGate::new()),
            praxis_store: None,
        }
    }

    /// Create a new executor with custom safety gate.
    pub fn with_safety_gate(
        registry: ProcedureRegistry,
        safety_gate: OptimizationSafetyGate,
    ) -> Self {
        Self {
            registry,
            safety_gate,
            praxis_gate: Box::new(DefaultPraxisGate::new()),
            praxis_store: None,
        }
    }

    /// Create a new executor with a custom [`PraxisGate`].
    pub fn with_praxis_gate(registry: ProcedureRegistry, praxis_gate: Box<dyn PraxisGate>) -> Self {
        Self {
            registry,
            safety_gate: OptimizationSafetyGate::new(),
            praxis_gate,
            praxis_store: None,
        }
    }

    /// Create a new executor with a praxis constraint store.
    ///
    /// When a [`PraxisStore`] is provided, [`on_action`] is called before
    /// every procedure execution.  Procedures that violate an `Error`-severity
    /// constraint are blocked and a [`Event::ConstraintViolation`] is emitted
    /// in place of the normal follow-up events.
    pub fn with_praxis_store(mut self, store: Arc<PraxisStore>) -> Self {
        self.praxis_store = Some(store);
        self
    }

    /// Get a reference to the safety gate for external access.
    pub fn safety_gate(&self) -> &OptimizationSafetyGate {
        &self.safety_gate
    }

    /// Get a reference to the praxis store, if configured.
    pub fn praxis_store(&self) -> Option<&PraxisStore> {
        self.praxis_store.as_deref()
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
            info!(
                procedure = procedure_name,
                kind, "executing procedure with safety check"
            );

            // Apply store-based praxis constraint check (emits ConstraintViolation)
            if let Some(store) = &self.praxis_store {
                // Use the procedure name as the action type; the target resource is
                // not known at the executor level so an empty string is passed.
                let ctx = AgentContext::new(procedure_name, "", SessionType::Main);
                if let Err(blocked) = on_action(store, &ctx) {
                    warn!(
                        procedure = procedure_name,
                        "procedure execution blocked by praxis store constraint"
                    );
                    let fix = blocked
                        .violations
                        .iter()
                        .map(|v| v.constraint.fix.as_str())
                        .collect::<Vec<_>>()
                        .join("; ");
                    follow_ups.push(Event::ConstraintViolation {
                        procedure: procedure_name.to_string(),
                        event_kind: kind.to_string(),
                        message: blocked.to_string(),
                        fix,
                    });
                    continue;
                }
            }

            // Apply praxis pre-action constraint check
            let action = format!("execute_procedure:{}", procedure_name);
            if let Err(reason) = self.praxis_gate.check(&action) {
                warn!(
                    procedure = procedure_name,
                    reason = %reason,
                    "procedure execution blocked by praxis constraint"
                );
                follow_ups.push(Event::PreActionConstraint { action, reason });
                continue;
            }

            // Apply optimization safety check
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
                warn!(iterations, "reached max_iterations, stopping event loop");
                break;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event::Event,
        procedure::{Procedure, ProcedureRegistry},
        source::EventSource,
    };
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    // ── helpers ───────────────────────────────────────────────────────────────

    /// An `EventSource` that returns a fixed list of events on every call.
    struct StaticEventSource {
        events: Vec<Event>,
    }

    #[async_trait]
    impl EventSource for StaticEventSource {
        async fn poll_events(&self) -> Vec<Event> {
            self.events.clone()
        }
    }

    /// A `Procedure` that increments a shared counter on each execution and
    /// returns a pre-configured list of follow-up events.
    struct CountingProcedure {
        name: &'static str,
        handles: &'static str,
        counter: Arc<AtomicUsize>,
        follow_ups: Vec<Event>,
    }

    #[async_trait]
    impl Procedure for CountingProcedure {
        fn name(&self) -> &str {
            self.name
        }
        fn handles(&self) -> &str {
            self.handles
        }
        async fn execute(&self, _event: &Event) -> Vec<Event> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            self.follow_ups.clone()
        }
    }

    fn message_event() -> Event {
        Event::Message {
            id: "msg-1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: "hello".into(),
        }
    }

    fn timer_event() -> Event {
        Event::Timer {
            id: "tmr-1".into(),
            name: "test_timer".into(),
            recurring: false,
        }
    }

    // ── dispatch: no matching procedures ─────────────────────────────────────

    /// Regression: dispatching an event for which no procedure is registered
    /// must return an empty follow-up list and must not panic.
    #[tokio::test]
    async fn dispatch_returns_empty_when_no_matching_procedures() {
        let mut registry = ProcedureRegistry::new();
        // Only a "message" handler is registered; a "timer" event is dispatched.
        let counter = Arc::new(AtomicUsize::new(0));
        registry.register(Box::new(CountingProcedure {
            name: "handle_message",
            handles: "message",
            counter: counter.clone(),
            follow_ups: vec![],
        }));

        let executor = Executor::with_praxis_gate(registry, Box::new(NoopPraxisGate));
        let follow_ups = executor.dispatch(&timer_event()).await;

        assert!(
            follow_ups.is_empty(),
            "expected no follow-ups for unmatched event kind"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "message handler must not run for a timer event"
        );
    }

    // ── dispatch: safety gate — InsufficientData ──────────────────────────────

    /// Procedures whose resolved action string contains "experimental" (or
    /// "beta") are classified as `InsufficientData` by the safety gate.
    /// The handler must not run, telemetry must be recorded, and an evidence
    /// request must be generated.
    #[tokio::test]
    async fn dispatch_blocked_by_insufficient_data() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut registry = ProcedureRegistry::new();
        // "execute_procedure:experimental_optimizer" contains "experimental"
        // → safety gate returns InsufficientData.
        registry.register(Box::new(CountingProcedure {
            name: "experimental_optimizer",
            handles: "message",
            counter: counter.clone(),
            follow_ups: vec![],
        }));

        let executor = Executor::with_praxis_gate(registry, Box::new(NoopPraxisGate));
        let follow_ups = executor.dispatch(&message_event()).await;

        assert!(
            follow_ups.is_empty(),
            "blocked procedure must not produce follow-ups"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "handler must not execute when gate returns InsufficientData"
        );

        let telemetry = executor.safety_gate().get_telemetry(None);
        assert_eq!(telemetry.len(), 1, "one telemetry record expected");
        assert_eq!(
            telemetry[0].safety_status,
            crate::optimization::OptimizationSafety::InsufficientData
        );

        let evidence = executor.safety_gate().get_pending_evidence_requests();
        assert_eq!(
            evidence.len(),
            1,
            "an evidence request must be created for InsufficientData"
        );
        assert!(
            evidence[0].description.contains("experimental_optimizer"),
            "evidence description should mention the blocked procedure"
        );
    }

    // ── dispatch: safety gate — UnsafeSolution ────────────────────────────────

    /// Procedures whose resolved action string contains "delete" (or "remove")
    /// are classified as `UnsafeSolution` by the safety gate.
    /// The handler must not run, telemetry must be recorded, and no evidence
    /// request should be generated (evidence requests are only for
    /// `InsufficientData`).
    #[tokio::test]
    async fn dispatch_blocked_by_unsafe_solution() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut registry = ProcedureRegistry::new();
        // "execute_procedure:delete_records" contains "delete"
        // → safety gate returns UnsafeSolution.
        registry.register(Box::new(CountingProcedure {
            name: "delete_records",
            handles: "message",
            counter: counter.clone(),
            follow_ups: vec![],
        }));

        let executor = Executor::with_praxis_gate(registry, Box::new(NoopPraxisGate));
        let follow_ups = executor.dispatch(&message_event()).await;

        assert!(
            follow_ups.is_empty(),
            "blocked procedure must not produce follow-ups"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "handler must not execute when gate returns UnsafeSolution"
        );

        let telemetry = executor.safety_gate().get_telemetry(None);
        assert_eq!(telemetry.len(), 1, "one telemetry record expected");
        assert_eq!(
            telemetry[0].safety_status,
            crate::optimization::OptimizationSafety::UnsafeSolution
        );

        let evidence = executor.safety_gate().get_pending_evidence_requests();
        assert!(
            evidence.is_empty(),
            "UnsafeSolution must not generate an evidence request"
        );
    }

    // ── run: follow-ups processed within the same tick ────────────────────────

    /// When a procedure returns follow-up events, those events must be
    /// dispatched within the **same** iteration of the event loop — before
    /// the next `poll_events` call.
    #[tokio::test]
    async fn run_processes_follow_up_events_within_same_tick() {
        let message_counter = Arc::new(AtomicUsize::new(0));
        let timer_counter = Arc::new(AtomicUsize::new(0));

        let mut registry = ProcedureRegistry::new();

        // The "message" handler emits a timer event as a follow-up.
        registry.register(Box::new(CountingProcedure {
            name: "handle_message",
            handles: "message",
            counter: message_counter.clone(),
            follow_ups: vec![timer_event()],
        }));

        // The "timer" handler should be invoked as a consequence of the above.
        registry.register(Box::new(CountingProcedure {
            name: "handle_timer",
            handles: "timer",
            counter: timer_counter.clone(),
            follow_ups: vec![],
        }));

        let executor = Executor::with_praxis_gate(registry, Box::new(NoopPraxisGate));
        // StaticEventSource always returns the same events; max_iterations = 1
        // ensures we process exactly one tick and then stop.
        let source = StaticEventSource {
            events: vec![message_event()],
        };
        executor.run(&source, 1).await;

        assert_eq!(
            message_counter.load(Ordering::SeqCst),
            1,
            "message handler must run exactly once"
        );
        assert_eq!(
            timer_counter.load(Ordering::SeqCst),
            1,
            "timer follow-up handler must run within the same tick"
        );
    }
}
