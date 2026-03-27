use async_trait::async_trait;
use pares_agens_core::{
    event::Event,
    executor::Executor,
    procedure::{Procedure, ProcedureRegistry},
    source::EventSource,
};
use std::sync::Mutex;

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
async fn all_five_event_kinds_are_constructible() {
    let events = [
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
    ];

    let kinds = [
        "message",
        "timer",
        "state_change",
        "model_response",
        "tool_result",
    ];
    for (event, expected_kind) in events.iter().zip(kinds.iter()) {
        assert_eq!(event.kind(), *expected_kind);
    }
}
