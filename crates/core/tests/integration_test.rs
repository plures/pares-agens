use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pares_agens_core::{
    event::Event,
    executor::Executor,
    handlers::{OnMessage, OnStateChange, OnTimer},
    memory::{Memory, MemoryCapture, MemoryClient},
    model::{ChatMessage, ModelCompletion, ModelClient, ToolDefinition, ToolDispatcher},
    procedure::{Procedure, ProcedureRegistry},
    source::EventSource,
};
use serde_json::json;

// ── Shared mocks ─────────────────────────────────────────────────────────────

struct SpyMemory {
    captures: Mutex<Vec<MemoryCapture>>,
}

impl SpyMemory {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            captures: Mutex::new(vec![]),
        })
    }

    fn captures(&self) -> Vec<MemoryCapture> {
        self.captures.lock().unwrap().clone()
    }
}

#[async_trait]
impl MemoryClient for SpyMemory {
    async fn recall(&self, _query: &str, _limit: usize) -> Vec<Memory> {
        vec![]
    }

    async fn capture(&self, item: MemoryCapture) {
        self.captures.lock().unwrap().push(item);
    }
}

struct ScriptedModel {
    response: String,
}

impl ScriptedModel {
    fn new(response: &str) -> Arc<Self> {
        Arc::new(Self {
            response: response.into(),
        })
    }
}

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> Result<ModelCompletion, String> {
        Ok(ModelCompletion {
            content: Some(self.response.clone()),
            tool_calls: vec![],
        })
    }
}

struct NoopTools;

#[async_trait]
impl ToolDispatcher for NoopTools {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        vec![]
    }

    async fn call_tool(&self, _name: &str, _arguments: serde_json::Value) -> String {
        String::new()
    }
}

/// A source that yields a fixed list of event batches, then returns empty.
struct BatchSource {
    batches: Mutex<Vec<Vec<Event>>>,
}

impl BatchSource {
    fn new(batches: Vec<Vec<Event>>) -> Self {
        let mut b = batches;
        b.reverse(); // so pop() yields FIFO
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

// ── Tests ─────────────────────────────────────────────────────────────────────

fn msg(content: &str) -> Event {
    Event::Message {
        id: "1".into(),
        channel: "test".into(),
        sender: "user".into(),
        content: content.into(),
    }
}

/// Core acceptance criterion: full message pipeline produces a response and
/// captures both turns in memory.
#[tokio::test]
async fn message_pipeline_recall_model_respond_capture() {
    let memory = SpyMemory::new();

    let on_message = OnMessage::new(
        memory.clone(),
        ScriptedModel::new("Hello from the model!"),
        Arc::new(NoopTools),
        "You are a helpful assistant.",
    );

    let follow_ups = on_message.execute(&msg("Say hello")).await;

    // Step 5 — response emitted
    assert_eq!(follow_ups.len(), 1, "one response event expected");
    match &follow_ups[0] {
        Event::Message { content, sender, .. } => {
            assert_eq!(content, "Hello from the model!");
            assert_eq!(sender, "agent");
        }
        other => panic!("expected Message, got {:?}", other),
    }

    // Step 6 — memory captured
    let captures = memory.captures();
    assert_eq!(captures.len(), 2, "user + assistant turns must be captured");
    assert_eq!(captures[0].role, "user");
    assert_eq!(captures[0].content, "Say hello");
    assert_eq!(captures[1].role, "assistant");
    assert_eq!(captures[1].content, "Hello from the model!");
}

/// Timer lookup and reschedule.
#[tokio::test]
async fn timer_fires_and_reschedules_recurring() {
    use pares_agens_core::handlers::on_timer::TimerAction;

    struct Noop;
    #[async_trait]
    impl TimerAction for Noop {
        async fn execute(&self) -> Vec<Event> {
            vec![]
        }
    }

    let on_timer = OnTimer::new();
    on_timer.register("heartbeat", Arc::new(Noop));

    let follow_ups = on_timer
        .execute(&Event::Timer {
            id: "t1".into(),
            name: "heartbeat".into(),
            recurring: true,
        })
        .await;

    assert_eq!(follow_ups.len(), 1, "recurring timer must emit a reschedule");
    assert!(matches!(follow_ups[0], Event::Timer { .. }));
}

/// State watcher fires and can emit events.
#[tokio::test]
async fn state_watcher_fires_on_key_match() {
    use pares_agens_core::handlers::on_state_change::WatcherAction;

    struct AlertAction;
    #[async_trait]
    impl WatcherAction for AlertAction {
        async fn on_change(&self, event: &Event) -> Vec<Event> {
            if let Event::StateChange { key, new_value, .. } = event {
                vec![Event::Message {
                    id: "alert".into(),
                    channel: "ops".into(),
                    sender: "watcher".into(),
                    content: format!("{key} changed to {new_value}"),
                }]
            } else {
                vec![]
            }
        }
    }

    let on_sc = OnStateChange::new();
    on_sc.watch("battery_level", Arc::new(AlertAction));

    let follow_ups = on_sc
        .execute(&Event::StateChange {
            key: "battery_level".into(),
            old_value: Some(json!(80)),
            new_value: json!(10),
        })
        .await;

    assert_eq!(follow_ups.len(), 1);
    if let Event::Message { content, .. } = &follow_ups[0] {
        assert!(content.contains("battery_level"), "content: {content}");
    } else {
        panic!("expected Message alert");
    }
}

/// The executor routes events to the correct handlers.
#[tokio::test]
async fn executor_routes_all_event_kinds() {
    let memory = SpyMemory::new();

    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(OnMessage::new(
        memory.clone(),
        ScriptedModel::new("ok"),
        Arc::new(NoopTools),
        "System.",
    )));
    let on_timer = OnTimer::new();
    registry.register(Box::new(on_timer));
    registry.register(Box::new(OnStateChange::new()));

    let executor = Executor::new(registry);

    // Message → expect response
    let follow_ups = executor.dispatch(&msg("hi")).await;
    assert_eq!(follow_ups.len(), 1);

    // Timer → no handlers registered, empty follow-ups
    let follow_ups = executor
        .dispatch(&Event::Timer {
            id: "t".into(),
            name: "unregistered".into(),
            recurring: false,
        })
        .await;
    assert!(follow_ups.is_empty());

    // StateChange → no watchers, empty follow-ups
    let follow_ups = executor
        .dispatch(&Event::StateChange {
            key: "k".into(),
            old_value: None,
            new_value: json!(1),
        })
        .await;
    assert!(follow_ups.is_empty());
}

/// Event loop processes multiple batches and terminates when source is
/// exhausted.
#[tokio::test]
async fn event_loop_processes_batches_to_completion() {
    let memory = SpyMemory::new();

    let mut registry = ProcedureRegistry::new();
    registry.register(Box::new(OnMessage::new(
        memory.clone(),
        ScriptedModel::new("response"),
        Arc::new(NoopTools),
        "System.",
    )));

    let source = BatchSource::new(vec![vec![msg("first")], vec![msg("second")]]);
    let executor = Executor::new(registry);

    executor.run(&source, 0).await;

    // Two messages × 2 captures each = 4 total memory captures
    assert_eq!(memory.captures().len(), 4);
}
