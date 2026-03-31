use async_trait::async_trait;
use pares_agens_core::{
    event::Event,
    executor::{Executor, NoopPraxisGate, PraxisGate},
    procedure::{Procedure, ProcedureRegistry},
    source::EventSource,
};
use pares_agens_praxis::db::{
    schema::{Condition, Constraint, Severity},
    seed::default_store,
    store::PraxisStore,
};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Stub procedures used by integration tests
// ---------------------------------------------------------------------------

/// Simple echo procedure: returns a Message event with the same content and
/// `sender = "agent"`.  Used in place of the full `OnMessage` pipeline so
/// that integration tests do not need real model/memory/tool dependencies.
struct EchoMessage;

#[async_trait]
impl Procedure for EchoMessage {
    fn name(&self) -> &str {
        "echo_message"
    }
    fn handles(&self) -> &str {
        "message"
    }
    async fn execute(&self, event: &Event) -> Vec<Event> {
        if let Event::Message {
            id,
            channel,
            content,
            sender,
            ..
        } = event
        {
            // Only echo user-originated messages to avoid an infinite loop
            if sender == "user" {
                vec![Event::Message {
                    id: format!("{id}-response"),
                    channel: channel.clone(),
                    sender: "agent".into(),
                    content: content.clone(),
                }]
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    }
}

/// No-op timer procedure: fires and returns no follow-up events.
struct NoopTimer;

#[async_trait]
impl Procedure for NoopTimer {
    fn name(&self) -> &str {
        "noop_timer"
    }
    fn handles(&self) -> &str {
        "timer"
    }
    async fn execute(&self, _: &Event) -> Vec<Event> {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn msg(content: &str) -> Event {
    Event::Message {
        id: "1".into(),
        channel: "test".into(),
        sender: "user".into(),
        content: content.into(),
    }
}

fn timer(name: &str) -> Event {
    Event::Timer {
        id: "t1".into(),
        name: name.into(),
        recurring: false,
    }
}

/// A source that yields a fixed list of batches, then returns empty.
struct BatchSource {
    batches: Mutex<Vec<Vec<Event>>>,
}

impl BatchSource {
    fn new(batches: Vec<Vec<Event>>) -> Self {
        // Reverse so we can pop() in FIFO order.
        let mut b = batches;
        b.reverse();
        Self {
            batches: Mutex::new(b),
        }
    }
}

#[async_trait]
impl EventSource for BatchSource {
    async fn poll_events(&self) -> Vec<Event> {
        self.batches.lock().unwrap().pop().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn on_message_echoes_via_executor() {
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(EchoMessage));
    let executor = Executor::new(registry);

    let follow_ups = executor.dispatch(&msg("hello world")).await;

    assert_eq!(
        follow_ups.len(),
        1,
        "on_message should emit exactly one echo"
    );
    if let Event::Message {
        content, sender, ..
    } = &follow_ups[0]
    {
        assert_eq!(content, "hello world");
        assert_eq!(sender, "agent");
    } else {
        panic!("expected Message echo");
    }
}

#[tokio::test]
async fn on_timer_dispatches_cleanly() {
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(NoopTimer));
    let executor = Executor::new(registry);

    let follow_ups = executor.dispatch(&timer("daily-summary")).await;
    assert!(follow_ups.is_empty(), "on_timer stub emits no follow-ups");
}

#[tokio::test]
async fn event_loop_processes_multiple_batches() {
    let source = BatchSource::new(vec![vec![msg("first")], vec![msg("second"), timer("tick")]]);

    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(EchoMessage));
    registry.register(Box::new(NoopTimer));
    let executor = Executor::new(registry);

    // max_iterations = 0 → runs until source is empty
    executor.run(&source, 0).await;
    // If we get here the loop terminated correctly.
}

#[tokio::test]
async fn event_loop_respects_max_iterations() {
    // Source always returns one event; the loop must stop at max_iterations.
    struct InfiniteSource;

    #[async_trait]
    impl EventSource for InfiniteSource {
        async fn poll_events(&self) -> Vec<Event> {
            vec![msg("tick")]
        }
    }

    let registry = ProcedureRegistry::new();
    let executor = Executor::new(registry);

    executor.run(&InfiniteSource, 3).await;
    // Reaches here means max_iterations was respected.
}

#[tokio::test]
async fn registry_only_routes_matching_kinds() {
    // Register only EchoMessage; timer events should produce no output.
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(EchoMessage));
    let executor = Executor::new(registry);

    let follow_ups = executor.dispatch(&timer("orphan")).await;
    assert!(
        follow_ups.is_empty(),
        "unregistered event kind should produce no follow-ups"
    );
}

#[tokio::test]
async fn all_event_kinds_are_constructible() {
    let events: Vec<Event> = vec![
        Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "hi".into(),
        },
        Event::Timer {
            id: "t".into(),
            name: "daily".into(),
            recurring: true,
        },
        Event::StateChange {
            key: "mood".into(),
            old_value: None,
            new_value: serde_json::json!("happy"),
        },
        Event::ModelResponse {
            request_id: "r".into(),
            model: "qwen3".into(),
            content: "ok".into(),
        },
        Event::ToolResult {
            tool_call_id: "tc".into(),
            tool_name: "search".into(),
            content: "{}".into(),
            is_error: false,
        },
        Event::PreActionConstraint {
            action: "execute_procedure:foo".into(),
            reason: "constraint violated".into(),
        },
        Event::ConstraintViolation {
            procedure: "p".into(),
            event_kind: "message".into(),
            message: "blocked".into(),
            fix: "fix it".into(),
        },
    ];

    let expected_kinds = [
        "message",
        "timer",
        "state_change",
        "model_response",
        "tool_result",
        "pre_action_constraint",
        "constraint_violation",
    ];
    for (event, expected_kind) in events.iter().zip(expected_kinds.iter()) {
        assert_eq!(event.kind(), *expected_kind);
    }
}

// ── PraxisGate integration tests ─────────────────────────────────────────────

/// A blocking gate that rejects every action.
struct BlockAllGate;

impl PraxisGate for BlockAllGate {
    fn check(&self, action: &str) -> Result<(), String> {
        Err(format!("blocked by test: {action}"))
    }
}

// ---------------------------------------------------------------------------
// Praxis constraint gate tests
// ---------------------------------------------------------------------------

/// Build a PraxisStore containing exactly one blocking constraint that fires
/// when the action type equals `"blocked_procedure"`.
fn store_blocking_procedure() -> Arc<PraxisStore> {
    let mut store = PraxisStore::new();
    store.upsert_constraint(Constraint {
        id: "T-0001".into(),
        description: "Test: block_procedure is always blocked.".into(),
        when: Condition::ActionStartsWith {
            prefix: "blocked_procedure".into(),
        },
        require: Condition::Not {
            condition: Box::new(Condition::Always),
        }, // require: !Always → always fails when `when` triggers
        fix: "Do not use blocked_procedure.".into(),
        evidence: vec![],
        severity: Severity::Error,
    });
    Arc::new(store)
}

/// Procedure whose name starts with `"blocked_procedure"` — triggers the
/// test constraint above.
struct BlockedProc;

#[async_trait]
impl Procedure for BlockedProc {
    fn name(&self) -> &str {
        "blocked_procedure"
    }
    fn handles(&self) -> &str {
        "message"
    }
    async fn execute(&self, _: &Event) -> Vec<Event> {
        // Should never be reached — constraint should block it.
        vec![Event::Message {
            id: "should-not-appear".into(),
            channel: "test".into(),
            sender: "agent".into(),
            content: "should not appear".into(),
        }]
    }
}

#[tokio::test]
async fn noop_praxis_gate_permits_all_procedures() {
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(EchoMessage));
    let executor = Executor::with_praxis_gate(registry, Box::new(NoopPraxisGate));

    let follow_ups = executor.dispatch(&msg("hello")).await;
    assert_eq!(follow_ups.len(), 1, "NoopPraxisGate should allow execution");
}

#[tokio::test]
async fn blocking_praxis_gate_emits_pre_action_constraint_event() {
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(EchoMessage));
    let executor = Executor::with_praxis_gate(registry, Box::new(BlockAllGate));

    let follow_ups = executor.dispatch(&msg("hello")).await;
    assert_eq!(
        follow_ups.len(),
        1,
        "blocking gate should emit exactly one PreActionConstraint event"
    );
    match &follow_ups[0] {
        Event::PreActionConstraint { action, reason } => {
            assert!(
                action.contains("echo_message"),
                "action should name the blocked procedure"
            );
            assert!(
                reason.contains("blocked by test"),
                "reason should contain gate message"
            );
        }
        other => panic!("expected PreActionConstraint, got {other:?}"),
    }
}

#[tokio::test]
async fn praxis_allows_safe_procedure() {
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(EchoMessage));
    // The default_store constraints (C-0002 through C-0008) gate on specific
    // action-type prefixes such as "write_", "delete_", or metadata fields like
    // "privilege_level".  The procedure name "echo_message" matches none of
    // those patterns, so on_action must return Ok and execution proceeds.
    let store = Arc::new(default_store());
    let executor = Executor::new(registry).with_praxis_store(store);

    let follow_ups = executor.dispatch(&msg("hello")).await;

    // Should get the normal echo back — no ConstraintViolation
    assert!(
        follow_ups
            .iter()
            .all(|e| e.kind() != "constraint_violation"),
        "safe procedure must not be blocked by praxis"
    );
    assert_eq!(follow_ups.len(), 1, "echo should still produce one reply");
}

#[tokio::test]
async fn praxis_blocks_violating_procedure_and_emits_event() {
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(BlockedProc));
    let store = store_blocking_procedure();
    let executor = Executor::new(registry).with_praxis_store(store);

    let follow_ups = executor.dispatch(&msg("trigger")).await;

    assert_eq!(
        follow_ups.len(),
        1,
        "exactly one ConstraintViolation event expected"
    );
    match &follow_ups[0] {
        Event::ConstraintViolation {
            procedure,
            event_kind,
            message,
            fix,
        } => {
            assert_eq!(procedure, "blocked_procedure");
            assert_eq!(event_kind, "message");
            assert!(
                message.contains("T-0001"),
                "message should reference the constraint id"
            );
            assert_eq!(fix, "Do not use blocked_procedure.");
        }
        other => panic!("expected ConstraintViolation, got {other:?}"),
    }
}

#[tokio::test]
async fn blocking_gate_does_not_execute_procedure() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let counter = Arc::new(AtomicUsize::new(0));

    struct CountingProcedure {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Procedure for CountingProcedure {
        fn name(&self) -> &str {
            "counting"
        }
        fn handles(&self) -> &str {
            "message"
        }
        async fn execute(&self, _: &Event) -> Vec<Event> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            vec![]
        }
    }

    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(CountingProcedure {
        counter: counter.clone(),
    }));
    let executor = Executor::with_praxis_gate(registry, Box::new(BlockAllGate));

    executor.dispatch(&msg("hello")).await;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "blocked procedure must not execute"
    );
}

#[tokio::test]
async fn praxis_blocked_procedure_does_not_execute() {
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(BlockedProc));
    let store = store_blocking_procedure();
    let executor = Executor::new(registry).with_praxis_store(store);

    let follow_ups = executor.dispatch(&msg("trigger")).await;

    // BlockedProc::execute returns a "should-not-appear" message — verify it
    // is NOT present (only the ConstraintViolation should be there).
    assert!(
        follow_ups.iter().all(|e| e.kind() != "message"),
        "blocked procedure must not produce normal follow-up events"
    );
}

#[tokio::test]
async fn praxis_without_store_executes_normally() {
    // Executor without a praxis store — existing behaviour must be preserved.
    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(EchoMessage));
    let executor = Executor::new(registry); // no praxis store

    let follow_ups = executor.dispatch(&msg("hello")).await;
    assert_eq!(follow_ups.len(), 1);
}
