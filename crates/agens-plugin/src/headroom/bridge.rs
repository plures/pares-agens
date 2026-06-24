//! Headroom context-compression bridge for the production agent loop.
//!
//! This module wires the **real leaf actors** of
//! [`HeadroomActionHandler`](crate::headroom::handler::HeadroomActionHandler) into
//! the agent model loop so that the message list
//! sent to the model on each turn can be losslessly-framed but byte-shrunk
//! before transmission.
//!
//! # Design contract
//!
//! * **Transient compression only.** [`HeadroomHook::compress_messages`]
//!   returns a *new* `Vec<ChatMessage>`; the canonical history the agent
//!   persists is never mutated. Compression is applied to a throwaway clone
//!   immediately before `model_client.complete(...)`.
//! * **Field-preserving.** Only [`ChatMessage::content`] is ever rewritten.
//!   `role`, `tool_call_id`, and `tool_calls` are copied verbatim.
//! * **Threshold-gated.** When the hook is disabled, or the estimated total
//!   token count is at/below `min_tokens`, the original slice is returned
//!   untouched (cloned) with zero leaf-actor work.
//! * **Non-fatal.** Any error from a leaf actor is logged with `warn!` and
//!   the *original* message content is preserved. The bridge never panics
//!   and never drops a message.
//! * **Cheap.** The hot path uses `tiktoken` token counting plus cheap
//!   string heuristics (sentence splitting, signature extraction, line
//!   dedup). It never calls `compute_embedding`.
//!
//! # Observability
//!
//! For each invocation past the gate, the hook writes two keys to the shared
//! CRDT store via the handler's `pluresdb_write` actor:
//!
//! * `headroom:input:<request_id>`  — the serialized *original* messages.
//! * `headroom:output:<request_id>` — the serialized *compressed* messages.
//!
//! These are best-effort; a write failure is logged and ignored.

use std::sync::Arc;

use pluresdb_px::px::executor::ActionHandler;
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
use tracing::{info, warn};

use crate::headroom::handler::HeadroomActionHandler;
use pares_radix_core::model::ChatMessage;
use pares_radix_core::state::StateStore;

/// Default minimum estimated token count below which compression is skipped.
///
/// Matches `MESSAGE_TOKEN_THRESHOLD` in `model_invoker.rs`.
pub const DEFAULT_MIN_TOKENS: usize = 500;

/// Per-message content length (in chars) below which an individual message is
/// left untouched even when the aggregate crosses `min_tokens`. Small messages
/// rarely benefit from compression and the heuristics add overhead.
const PER_MESSAGE_MIN_CHARS: usize = 200;

/// Estimate the token count of a string using the chars/4 heuristic.
///
/// This intentionally mirrors the estimator in `model_invoker.rs` so the
/// gate behaves consistently across the codebase. It is *only* used for the
/// cheap pre-gate check; the real `count_tokens` leaf actor (cl100k) is used
/// for reporting when available.
#[inline]
pub fn count_text_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Estimate the aggregate token count of a slice of messages (chars/4).
pub fn count_message_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(|m| count_text_tokens(&m.content)).sum()
}

/// Compression bridge handle attached to the agent model loop.
///
/// Clone is cheap: every field is an `Arc` or `Copy`.
#[derive(Clone)]
pub struct HeadroomHook {
    /// When `false`, [`compress_messages`](Self::compress_messages) is a
    /// transparent clone-through.
    enabled: bool,
    /// Aggregate token floor (chars/4 estimate) below which compression is
    /// skipped.
    min_tokens: usize,
    /// Shared state store used to persist observability keys.
    state_store: Arc<dyn StateStore>,
    /// Real leaf-actor handler doing the actual compression work.
    handler: Arc<HeadroomActionHandler>,
}

impl HeadroomHook {
    /// Construct an enabled hook.
    ///
    /// `min_tokens` of `0` is normalized to [`DEFAULT_MIN_TOKENS`].
    pub fn new(
        state_store: Arc<dyn StateStore>,
        handler: Arc<HeadroomActionHandler>,
        min_tokens: usize,
    ) -> Self {
        Self {
            enabled: true,
            min_tokens: if min_tokens == 0 {
                DEFAULT_MIN_TOKENS
            } else {
                min_tokens
            },
            state_store,
            handler,
        }
    }

    /// Construct a disabled hook (transparent clone-through).
    ///
    /// Useful when callers want a non-`None` field but compression switched
    /// off; the agent treats `None` and a disabled hook identically.
    pub fn disabled(
        state_store: Arc<dyn StateStore>,
        handler: Arc<HeadroomActionHandler>,
    ) -> Self {
        Self {
            enabled: false,
            min_tokens: DEFAULT_MIN_TOKENS,
            state_store,
            handler,
        }
    }

    /// Whether this hook will attempt compression.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Compress a transient copy of `messages` for transmission to the model.
    ///
    /// Returns a new `Vec<ChatMessage>` with identical roles/tool metadata
    /// and (possibly) shrunk `content`. The input is never mutated.
    ///
    /// The gate short-circuits (returning an exact clone) when:
    /// * the hook is disabled, or
    /// * the estimated aggregate token count is `<= min_tokens`.
    ///
    /// All leaf-actor failures degrade to "keep original content" and are
    /// logged; this function does not return `Result` because it must never
    /// break the agent loop.
    pub async fn compress_messages(
        &self,
        request_id: &str,
        messages: &[ChatMessage],
    ) -> Vec<ChatMessage> {
        if !self.enabled {
            return messages.to_vec();
        }

        let original_tokens = count_message_tokens(messages);
        if original_tokens <= self.min_tokens {
            return messages.to_vec();
        }

        let start = std::time::Instant::now();

        // Best-effort observability: record the original messages.
        self.write_observability(&format!("headroom:input:{request_id}"), messages);

        let mut compressed: Vec<ChatMessage> = Vec::with_capacity(messages.len());
        for msg in messages {
            let new_content = self.compress_one(&msg.content);
            compressed.push(ChatMessage {
                role: msg.role.clone(),
                content: new_content,
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls: msg.tool_calls.clone(),
            });
        }

        // Best-effort observability: record the compressed messages.
        self.write_observability(&format!("headroom:output:{request_id}"), &compressed);

        let compressed_tokens = count_message_tokens(&compressed);
        let elapsed_ms = start.elapsed().as_millis();

        // Safety net: if compression somehow *grew* the payload, fall back to
        // the originals so we never spend more tokens than we started with.
        if compressed_tokens >= original_tokens {
            info!(
                request_id,
                original_tokens,
                compressed_tokens,
                elapsed_ms,
                "headroom: compression produced no net savings; using originals"
            );
            return messages.to_vec();
        }

        info!(
            request_id,
            messages = messages.len(),
            original_tokens,
            compressed_tokens,
            saved_tokens = original_tokens.saturating_sub(compressed_tokens),
            elapsed_ms,
            "headroom: compressed transient message clone"
        );

        compressed
    }

    /// Compress a single message body by detected content type.
    ///
    /// Returns the original `content` unchanged on any failure or when the
    /// content is too small to be worth compressing.
    fn compress_one(&self, content: &str) -> String {
        if content.len() < PER_MESSAGE_MIN_CHARS {
            return content.to_string();
        }

        let content_type = self.detect_content_type(content);
        let compressed = match content_type.as_str() {
            "code" => self.compress_code(content),
            "log" => self.compress_log(content),
            "prose" | "error" => self.compress_prose(content),
            // json / other: structural whitespace trim is the safe default.
            _ => self.compress_whitespace(content),
        };

        // Only accept the rewrite if it is actually smaller; otherwise keep
        // the original so a per-message heuristic can never regress a body.
        match compressed {
            Some(out) if out.len() < content.len() && !out.trim().is_empty() => out,
            _ => content.to_string(),
        }
    }

    // ── content-type detection ────────────────────────────────────────────

    fn detect_content_type(&self, content: &str) -> String {
        match self
            .handler
            .call("detect_content_type", &json!({ "content": content }))
        {
            Ok(v) => v
                .get("content_type")
                .and_then(|c| c.as_str())
                .unwrap_or("prose")
                .to_string(),
            Err(e) => {
                warn!(error = %e, "headroom: detect_content_type failed; defaulting to prose");
                "prose".to_string()
            }
        }
    }

    // ── prose: extractive sentence trim ───────────────────────────────────

    /// Prose compression: split into sentences via the real `split_sentences`
    /// actor and keep a head+tail extractive window, collapsing the middle.
    /// This preserves the opening context and the most recent content (which
    /// is typically the most relevant) while dropping the bulk middle.
    fn compress_prose(&self, content: &str) -> Option<String> {
        let v = self
            .handler
            .call("split_sentences", &json!({ "content": content }))
            .map_err(|e| warn!(error = %e, "headroom: split_sentences failed"))
            .ok()?;
        let sentences: Vec<String> = v
            .get("sentences")?
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();

        // Not enough sentences to meaningfully trim.
        if sentences.len() <= 6 {
            return Some(collapse_whitespace(content));
        }

        let head = 3usize;
        let tail = 3usize;
        let dropped = sentences.len() - head - tail;
        let mut out = String::with_capacity(content.len());
        for s in &sentences[..head] {
            out.push_str(s);
            out.push(' ');
        }
        out.push_str(&format!("[… {dropped} sentences elided …] "));
        for s in &sentences[sentences.len() - tail..] {
            out.push_str(s);
            out.push(' ');
        }
        Some(out.trim_end().to_string())
    }

    // ── code: AST signature extraction ────────────────────────────────────

    /// Code compression: detect the language, then replace the body with the
    /// extracted signatures (functions, types, etc.) from the real
    /// `extract_ast_signatures` actor. Falls back to whitespace collapse when
    /// no signatures are found.
    fn compress_code(&self, content: &str) -> Option<String> {
        let language = self.detect_language(content);
        let v = self
            .handler
            .call(
                "extract_ast_signatures",
                &json!({ "content": content, "language": language }),
            )
            .map_err(|e| warn!(error = %e, "headroom: extract_ast_signatures failed"))
            .ok()?;
        let sigs: Vec<String> = v
            .get("signatures")?
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();

        if sigs.is_empty() {
            return Some(collapse_whitespace(content));
        }

        let mut out = String::with_capacity(content.len() / 2);
        out.push_str(&format!(
            "// [headroom: {language} body elided — {} signature(s) kept]\n",
            sigs.len()
        ));
        for s in sigs {
            out.push_str(&s);
            out.push('\n');
        }
        Some(out.trim_end().to_string())
    }

    fn detect_language(&self, content: &str) -> String {
        match self
            .handler
            .call("detect_language", &json!({ "content": content }))
        {
            Ok(v) => v
                .get("language")
                .and_then(|l| l.as_str())
                .unwrap_or("unknown")
                .to_string(),
            Err(_) => "unknown".to_string(),
        }
    }

    // ── log: line dedup ───────────────────────────────────────────────────

    /// Log compression: collapse consecutive duplicate lines and summarize
    /// runs. Pure Rust (no actor needed) but kept here for symmetry. Returns
    /// `None` only if the result is not smaller.
    fn compress_log(&self, content: &str) -> Option<String> {
        let mut out = String::with_capacity(content.len());
        let mut prev: Option<&str> = None;
        let mut run: usize = 0;

        let flush = |out: &mut String, line: &str, run: usize| {
            if run > 1 {
                out.push_str(line);
                out.push_str(&format!("  [×{run}]\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        };

        for line in content.lines() {
            match prev {
                Some(p) if p == line => run += 1,
                Some(p) => {
                    flush(&mut out, p, run);
                    prev = Some(line);
                    run = 1;
                }
                None => {
                    prev = Some(line);
                    run = 1;
                }
            }
        }
        if let Some(p) = prev {
            flush(&mut out, p, run);
        }

        Some(out.trim_end().to_string())
    }

    // ── json / other: whitespace collapse ─────────────────────────────────

    fn compress_whitespace(&self, content: &str) -> Option<String> {
        Some(collapse_whitespace(content))
    }

    // ── observability ─────────────────────────────────────────────────────

    /// Persist a serialized message slice under `key` (best-effort).
    ///
    /// Writes through both the handler's `pluresdb_write` actor (so the data
    /// lands in the same CRDT store the handler reads) — this keeps the
    /// observability surface consistent with the analyze-stage protocol.
    fn write_observability(&self, key: &str, messages: &[ChatMessage]) {
        let payload = match serde_json::to_value(messages) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, key, "headroom: failed to serialize messages for observability");
                return;
            }
        };
        if let Err(e) = self.handler.call(
            "pluresdb_write",
            &json!({ "key": key, "value": payload }),
        ) {
            warn!(error = %e, key, "headroom: observability write failed");
        }
    }

    /// Test/diagnostic accessor: read back an observability key as a value.
    #[cfg(test)]
    fn read_observability(&self, key: &str) -> Option<Value> {
        self.handler
            .call("pluresdb_read", &json!({ "key": key }))
            .ok()
            .and_then(|v| v.get("value").cloned())
            .filter(|v| !v.is_null())
    }

    /// Expose the shared state store (used by callers that need to verify the
    /// same backing store is wired through).
    pub fn state_store(&self) -> Arc<dyn StateStore> {
        Arc::clone(&self.state_store)
    }
}

/// Collapse runs of whitespace (including blank lines) into single spaces,
/// trimming each line. Cheap structural shrink used for json/other and as a
/// fallback for prose/code.
fn collapse_whitespace(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut last_was_space = false;
    for ch in content.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pares_radix_core::state::PluresDbStateStore;
    use pluresdb::CrdtStore;

    fn make_hook(min_tokens: usize) -> HeadroomHook {
        let store = Arc::new(PluresDbStateStore::in_memory());
        let handler = Arc::new(HeadroomActionHandler::new(Arc::new(CrdtStore::default())));
        HeadroomHook::new(store, handler, min_tokens)
    }

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[tokio::test]
    async fn below_threshold_returns_originals_untouched() {
        let hook = make_hook(500);
        let messages = vec![msg("user", "short message")];
        let out = hook.compress_messages("req-1", &messages).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content, "short message");
    }

    #[tokio::test]
    async fn disabled_hook_is_passthrough() {
        let store = Arc::new(PluresDbStateStore::in_memory());
        let handler = Arc::new(HeadroomActionHandler::new(Arc::new(CrdtStore::default())));
        let hook = HeadroomHook::disabled(store, handler);
        let big = "word ".repeat(2000); // ~10k chars, well over threshold
        let messages = vec![msg("user", &big)];
        let out = hook.compress_messages("req-2", &messages).await;
        assert_eq!(out[0].content, big);
    }

    #[tokio::test]
    async fn preserves_roles_and_tool_metadata() {
        let hook = make_hook(10);
        let mut m = msg("tool", &"sentence one. sentence two. ".repeat(100));
        m.tool_call_id = Some("call_abc".into());
        let messages = vec![m];
        let out = hook.compress_messages("req-3", &messages).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "tool");
        assert_eq!(out[0].tool_call_id.as_deref(), Some("call_abc"));
    }

    #[tokio::test]
    async fn large_prose_is_compressed() {
        let hook = make_hook(10);
        // 80 distinct sentences → should trim the middle.
        let prose: String = (0..80)
            .map(|i| format!("This is sentence number {i} with some filler words. "))
            .collect();
        let messages = vec![msg("user", &prose)];
        let out = hook.compress_messages("req-4", &messages).await;
        assert!(
            out[0].content.len() < prose.len(),
            "expected compression to shrink prose: {} >= {}",
            out[0].content.len(),
            prose.len()
        );
        assert!(out[0].content.contains("elided"));
    }

    #[tokio::test]
    async fn observability_keys_written() {
        let hook = make_hook(10);
        let prose: String = (0..80)
            .map(|i| format!("This is sentence number {i} with some filler words. "))
            .collect();
        let messages = vec![msg("user", &prose)];
        let _ = hook.compress_messages("req-5", &messages).await;
        assert!(hook.read_observability("headroom:input:req-5").is_some());
        assert!(hook.read_observability("headroom:output:req-5").is_some());
    }

    #[test]
    fn whitespace_collapse_shrinks() {
        let input = "a\n\n\n   b      c\t\t d";
        assert_eq!(collapse_whitespace(input), "a b c d");
    }

    #[test]
    fn log_dedup_collapses_runs() {
        let hook = make_hook(10);
        let log = "ERROR boom\nERROR boom\nERROR boom\nINFO ok\n";
        let out = hook.compress_log(log).unwrap();
        assert!(out.contains("[×3]"));
        assert!(out.contains("INFO ok"));
    }
}
