//! Core-path E2E proof for the headroom compression seam in the **agent**.
//!
//! This is the C-VERIFY-001 gate. Unlike `headroom_e2e.rs` (which exercises the
//! `HeadroomActionHandler` / `.px` procedure layer in isolation), this suite
//! drives the *real public agent entrypoint* — [`Agent::handle_event`] with an
//! [`Event::Message`] — through a [`CapturingModelClient`] that records the
//! exact `messages` slice the agent hands to `ModelClient::complete`.
//!
//! It asserts the contract that matters in production:
//!
//! 1. With an enabled [`HeadroomHook`] and an over-threshold message payload,
//!    the messages the model actually receives have **strictly fewer tokens**
//!    than the uncompressed payload (real reduction, not a `success: true`
//!    stub), while message **count** and **roles** are preserved.
//! 2. The agent's own canonical conversation history is **unchanged** — proven
//!    by sending a second turn and observing that the history replayed to the
//!    model on turn 2 still carries the *full uncompressed* turn-1 content.
//!    (Compression is transient: it only ever touches the throwaway clone.)
//! 3. A **disabled** hook, a **below-threshold** payload, and **no hook at all**
//!    are all exact pass-throughs (the model sees the originals verbatim).
//!
//! The fine-grained per-message invariants on the seam itself (count parity,
//! `tool_call_id` preservation, and the canonical-vec-untouched guarantee
//! observed *directly* on the value `run_model_loop` returns) are covered by
//! the `run_model_loop_*` unit tests inside `agent.rs` — those can see the
//! private loop's return value, which an integration test cannot.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pares_agens_core::event::Event;
use pares_agens_core::headroom::HeadroomActionHandler;
use pares_agens_core::headroom_bridge::{count_message_tokens, HeadroomHook};
use pares_agens_core::model::{
    ChatMessage, ChatOptions, ModelClient, ModelCompletion, ToolDefinition, ToolDispatcher,
};
use pares_agens_core::state::{PluresDbStateStore, StateStore};
use pares_agens_core::{Agent, InMemory};
use serde_json::Value;

/// A `ModelClient` that records every `messages` slice it is asked to complete
/// and returns a fixed, tool-call-free assistant reply so `run_model_loop`
/// terminates in exactly one turn.
struct CapturingModelClient {
    /// Each entry is the full `messages` slice received for one `complete` call.
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

impl CapturingModelClient {
    fn new() -> (Self, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                seen: Arc::clone(&seen),
            },
            seen,
        )
    }
}

#[async_trait]
impl ModelClient for CapturingModelClient {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        _options: &ChatOptions,
    ) -> Result<ModelCompletion, String> {
        self.seen.lock().unwrap().push(messages.to_vec());
        Ok(ModelCompletion {
            // Direct text reply, no tool calls → loop ends after one turn.
            content: Some("ack".to_string()),
            tool_calls: vec![],
            logprobs: None,
        })
    }
}

/// A no-op tool dispatcher that advertises zero tools.
struct NoTools;

#[async_trait]
impl ToolDispatcher for NoTools {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    async fn call_tool(&self, _name: &str, _arguments: Value) -> String {
        String::new()
    }
}

/// Build an enabled headroom hook (min_tokens = 500) over a fresh in-memory
/// PluresDB-backed state store + real leaf-actor handler.
///
/// The handler is wired to the *same* CRDT store the state store wraps so that
/// observability writes land where the handler reads. `Arc<PluresDbStateStore>`
/// coerces to `Arc<dyn StateStore>` for the hook constructor.
fn enabled_hook(min_tokens: usize) -> HeadroomHook {
    let store = Arc::new(PluresDbStateStore::in_memory());
    let handler = Arc::new(HeadroomActionHandler::new(store.crdt_store()));
    HeadroomHook::new(store as Arc<dyn StateStore>, handler, min_tokens)
}

/// Build a *disabled* headroom hook (transparent pass-through).
fn disabled_hook() -> HeadroomHook {
    let store = Arc::new(PluresDbStateStore::in_memory());
    let handler = Arc::new(HeadroomActionHandler::new(store.crdt_store()));
    HeadroomHook::disabled(store as Arc<dyn StateStore>, handler)
}

/// A multi-thousand-token user message built from large prose + a big code
/// block, guaranteed to blow past the 500-token (chars/4) gate.
///
/// Each prose sentence is long, and there are many of them, so the prose
/// compressor's head+tail extractive trim has lots to elide; the code block
/// gives the code path a body to drop too.
fn over_threshold_user_content() -> String {
    let mut s = String::new();
    // ~120 distinct, long sentences of prose.
    for i in 0..120 {
        s.push_str(&format!(
            "This is sentence number {i} in a deliberately verbose paragraph that exists \
             purely to inflate the token count well beyond the compression threshold so the \
             headroom hook is forced to engage its extractive prose trimming heuristics. "
        ));
    }
    s.push('\n');
    // A large code block with many real signatures + bodies to elide.
    s.push_str("```rust\n");
    for i in 0..40 {
        s.push_str(&format!(
            "pub fn handler_{i}(input: &str, count: usize) -> Result<String, String> {{\n    \
             let mut acc = String::new();\n    for _ in 0..count {{ acc.push_str(input); }}\n    \
             if acc.is_empty() {{ return Err(\"empty\".into()); }}\n    Ok(acc)\n}}\n\n"
        ));
    }
    s.push_str("```\n");
    s
}

/// Reconstruct the *uncompressed* message list `run_model_loop` would build for
/// a single first turn (empty history): `[system, user(content)]`. Used to
/// compute the baseline token count the captured payload is compared against.
fn baseline_messages(system: &str, content: &str) -> Vec<ChatMessage> {
    vec![ChatMessage::system(system), ChatMessage::user(content)]
}

const TEST_SYSTEM_PROMPT: &str = "You are a test agent.";

fn message(channel: &str, content: &str) -> Event {
    Event::Message {
        id: "req-1".into(),
        channel: channel.into(),
        sender: "user".into(),
        content: content.into(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. ENABLED HOOK + OVER-THRESHOLD → real reduction on the agent path
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_path_compresses_messages_sent_to_model() {
    let (model, seen) = CapturingModelClient::new();
    let agent = Agent::new(Arc::new(InMemory::new()))
        .with_model(Arc::new(model), Arc::new(NoTools), TEST_SYSTEM_PROMPT.into())
        .with_headroom(enabled_hook(500));

    let content = over_threshold_user_content();
    let baseline = baseline_messages(TEST_SYSTEM_PROMPT, &content);
    let baseline_tokens = count_message_tokens(&baseline);
    // Sanity: the payload really is over the gate.
    assert!(
        baseline_tokens > 500,
        "test fixture must exceed the 500-token gate; got {baseline_tokens}"
    );

    let resp = agent.handle_event(message("chan-a", &content)).await;
    assert!(matches!(resp, Some(Event::ModelResponse { .. })));

    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1, "model should be called exactly once");
    let captured = &calls[0];

    // (b) message COUNT preserved: system + the single user message.
    assert_eq!(
        captured.len(),
        baseline.len(),
        "compression must not add/drop messages"
    );
    // (c) roles preserved, positionally.
    for (got, want) in captured.iter().zip(baseline.iter()) {
        assert_eq!(got.role, want.role, "role must be preserved");
    }
    assert_eq!(captured[0].role, "system");
    assert_eq!(captured[1].role, "user");

    // (a) REAL reduction: strictly fewer tokens than the uncompressed payload.
    let captured_tokens = count_message_tokens(captured);
    assert!(
        captured_tokens < baseline_tokens,
        "expected strict token reduction on the agent→model path: \
         captured {captured_tokens} tok is not < baseline {baseline_tokens} tok"
    );

    eprintln!(
        "AGENT-E2E reduction: baseline {baseline_tokens} tok -> captured {captured_tokens} tok \
         (saved {} tok, {:.1}%)",
        baseline_tokens - captured_tokens,
        100.0 * (baseline_tokens - captured_tokens) as f64 / baseline_tokens as f64
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Compression is TRANSIENT — the agent's *persisted* canonical history is
//    never mutated.
//
// Observed directly through the public API by attaching a turn store and
// reading back what the agent persisted. The compressor is (deliberately)
// idempotent, so the *captured* model payload alone cannot distinguish
// "compressed-in-place" from "compressed-transiently" — the persisted record
// can. The stored turn-1 user message must be the FULL uncompressed text,
// including a mid-prose sentence that transient compression would have elided.
// (The fine-grained view of the loop's in-memory return value is asserted in
// the `run_model_loop_*` unit tests in agent.rs.)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_persisted_history_is_unchanged_after_compression() {
    use pares_agens_core::memory::store::{InMemoryStore, MemoryStore};

    let turn_store = Arc::new(InMemoryStore::new());
    let (model, seen) = CapturingModelClient::new();
    let agent = Agent::new(Arc::new(InMemory::new()))
        .with_model(Arc::new(model), Arc::new(NoTools), TEST_SYSTEM_PROMPT.into())
        .with_turn_store(turn_store.clone() as Arc<dyn MemoryStore>)
        .with_headroom(enabled_hook(500));

    let big = over_threshold_user_content();

    // Single over-threshold turn → compressed in transit to the model.
    let _ = agent.handle_event(message("chan-hist", &big)).await;

    // The model genuinely received a compressed payload (transient effect).
    {
        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            count_message_tokens(&calls[0]) < count_message_tokens(&baseline_messages(
                TEST_SYSTEM_PROMPT,
                &big
            )),
            "sanity: the model payload should have been compressed"
        );
    }

    // The PERSISTED canonical record must be the FULL uncompressed user text.
    let turns = turn_store.recent_turns("chan-hist", 10).await.unwrap();
    let stored_user = turns
        .iter()
        .flat_map(|t| t.messages.iter())
        .find(|m| m.role == "user")
        .expect("turn-1 user message must be persisted");

    // A mid-prose sentence (sentence 60) lives only in the FULL text; the
    // extractive prose trim keeps just head+tail, so its presence proves the
    // persisted record was never compressed.
    let middle_marker = "This is sentence number 60 in a deliberately verbose paragraph";
    assert!(
        stored_user.content.contains(middle_marker),
        "persisted turn-1 user message must retain the FULL uncompressed content \
         (transient compression must not have mutated canonical history)"
    );
    // And it must equal the original content byte-for-byte.
    assert_eq!(
        stored_user.content, big,
        "persisted user content must be verbatim, not the compressed clone"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3a. DISABLED hook → exact pass-through (model sees originals verbatim).
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_disabled_hook_is_passthrough() {
    let (model, seen) = CapturingModelClient::new();
    let agent = Agent::new(Arc::new(InMemory::new()))
        .with_model(Arc::new(model), Arc::new(NoTools), TEST_SYSTEM_PROMPT.into())
        .with_headroom(disabled_hook());

    let content = over_threshold_user_content();
    let baseline = baseline_messages(TEST_SYSTEM_PROMPT, &content);
    let baseline_tokens = count_message_tokens(&baseline);

    let _ = agent.handle_event(message("chan-disabled", &content)).await;

    let calls = seen.lock().unwrap();
    let captured = &calls[0];
    assert_eq!(captured.len(), baseline.len());
    // Disabled hook must not touch a single byte, even over threshold.
    assert_eq!(
        count_message_tokens(captured),
        baseline_tokens,
        "disabled hook must be an exact pass-through"
    );
    assert_eq!(captured[1].content, content, "user content must be verbatim");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3b. NO hook at all (None path) → exact pass-through.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_without_headroom_is_passthrough() {
    let (model, seen) = CapturingModelClient::new();
    let agent = Agent::new(Arc::new(InMemory::new())).with_model(
        Arc::new(model),
        Arc::new(NoTools),
        TEST_SYSTEM_PROMPT.into(),
    );

    let content = over_threshold_user_content();
    let baseline = baseline_messages(TEST_SYSTEM_PROMPT, &content);

    let _ = agent.handle_event(message("chan-none", &content)).await;

    let calls = seen.lock().unwrap();
    let captured = &calls[0];
    assert_eq!(
        count_message_tokens(captured),
        count_message_tokens(&baseline),
        "no headroom hook → originals reach the model untouched"
    );
    assert_eq!(captured[1].content, content);
}

// ═══════════════════════════════════════════════════════════════════════════
// 3c. BELOW-THRESHOLD payload with an enabled hook → exact pass-through.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_below_threshold_is_passthrough() {
    let (model, seen) = CapturingModelClient::new();
    let agent = Agent::new(Arc::new(InMemory::new()))
        .with_model(Arc::new(model), Arc::new(NoTools), TEST_SYSTEM_PROMPT.into())
        .with_headroom(enabled_hook(500));

    // Tiny content → [system, user] aggregate is well under 500 tokens.
    let content = "hello there";
    let baseline = baseline_messages(TEST_SYSTEM_PROMPT, content);
    assert!(
        count_message_tokens(&baseline) <= 500,
        "below-threshold fixture must stay under the gate"
    );

    let _ = agent.handle_event(message("chan-small", content)).await;

    let calls = seen.lock().unwrap();
    let captured = &calls[0];
    assert_eq!(
        count_message_tokens(captured),
        count_message_tokens(&baseline),
        "below-threshold payload must pass through untouched"
    );
    assert_eq!(captured[1].content, content);
}
