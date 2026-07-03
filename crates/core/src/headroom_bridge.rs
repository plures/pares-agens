//! Headroom context-compression bridge for the production agent loop.
//!
//! This module wires the **real leaf actors** of
//! [`HeadroomActionHandler`](crate::headroom::HeadroomActionHandler) into
//! [`Agent::run_model_loop`](crate::agent::Agent) so that the message list
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
//! * `headroom:input:<request_id>`  ΓÇö the serialized *original* messages.
//! * `headroom:output:<request_id>` ΓÇö the serialized *compressed* messages.
//!
//! These are best-effort; a write failure is logged and ignored.

use std::sync::Arc;

use pluresdb_px::px::executor::ActionHandler;
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
use tracing::{info, warn};

use crate::headroom::HeadroomActionHandler;
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

/// Compression bridge handle attached to an [`Agent`](crate::agent::Agent).
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
            // json: SmartCrusher array-of-records compaction first (real
            // structural reduction on tool-output / RAG shapes); fall back to
            // whitespace trim when it is not a crushable array. `compress_one`'s
            // own smaller-output guard below still applies.
            "json" => crate::headroom::compress_json_array(
                content,
                &crate::headroom::JsonCrushConfig::default(),
            )
            .or_else(|| self.compress_whitespace(content)),
            // other: structural whitespace trim is the safe default.
            _ => self.compress_whitespace(content),
        };

        // Only accept the rewrite if it is actually smaller; otherwise keep
        // the original so a per-message heuristic can never regress a body.
        match compressed {
            Some(out) if out.len() < content.len() && !out.trim().is_empty() => out,
            _ => content.to_string(),
        }
    }

    // ΓöÇΓöÇ content-type detection ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

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

    // ΓöÇΓöÇ prose: extractive sentence trim ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

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
        out.push_str(&format!("[ΓÇª {dropped} sentences elided ΓÇª] "));
        for s in &sentences[sentences.len() - tail..] {
            out.push_str(s);
            out.push(' ');
        }
        Some(out.trim_end().to_string())
    }

    // ΓöÇΓöÇ code: AST signature extraction ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

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
            "// [headroom: {language} body elided ΓÇö {} signature(s) kept]\n",
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

    // ΓöÇΓöÇ log: line dedup ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

    /// Log compression: group consecutive lines that share the same *template*
    /// (variable fields masked) into `first_raw_line  [xN ~ <TEMPLATE>]`;
    /// singletons pass through verbatim. Pure Rust (no actor needed) but kept
    /// here for symmetry. Returns `None` only if the result is not smaller.
    ///
    /// Path-2 upgrade (ported from the pluresdb-native origin, 2026-07-02): the
    /// old body collapsed only *byte-identical* adjacent lines, which achieves
    /// ~0% on real logs because a timestamp / request-id / duration varies on
    /// every line. The template normalizer (`log_template`) masks those
    /// variable fields so structurally-identical lines collapse. The signature
    /// and call-site are unchanged; the algorithm lives in the module-level
    /// free fn `compress_log_impl` (it needs no `&self`).
    fn compress_log(&self, content: &str) -> Option<String> {
        compress_log_impl(content)
    }

    // ΓöÇΓöÇ json / other: whitespace collapse ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

    fn compress_whitespace(&self, content: &str) -> Option<String> {
        Some(collapse_whitespace(content))
    }

    // ΓöÇΓöÇ observability ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

    /// Persist a serialized message slice under `key` (best-effort).
    ///
    /// Writes through both the handler's `pluresdb_write` actor (so the data
    /// lands in the same CRDT store the handler reads) ΓÇö this keeps the
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

// ── log: template-normalizing consecutive run collapse (Path-2) ─────────────
//
// Ported from the pluresdb-native origin (crates/pluresdb-node/src/headroom.rs,
// 2026-07-02). The old run-collapse collapsed only *byte-identical* adjacent
// lines, which achieves ~0% on real logs because a timestamp / request-id /
// duration varies on every line. Instead we render each line into a **template**
// by masking variable fields and collapse CONSECUTIVE lines that share the same
// rendered template. The first RAW line of a run is emitted verbatim (a human /
// embedding always sees a real example); singletons are emitted 100% unchanged.
// Pure `std` — NO regex, NO once_cell — so no new dependency is introduced.

/// Minimum identical-template run before we collapse.
const MIN_RUN: usize = 2;

/// Log compression: group runs of adjacent lines with an identical *template*
/// (variable fields masked) into `first_raw_line  [xN ~ <TEMPLATE>]`; singletons
/// pass through verbatim. Deterministic, streaming one-line lookahead — same
/// contract as the old run-collapse, but template-aware.
///
/// The marker uses a plain ASCII `x` (not the U+00D7 multiplication sign) to
/// avoid the mojibake-on-disk artifact the origin flagged.
fn compress_log_impl(content: &str) -> Option<String> {
    let mut out = String::with_capacity(content.len());
    let mut prev_line: Option<&str> = None;
    let mut prev_tmpl = String::new();
    let mut cur_tmpl = String::new();
    let mut run: usize = 0;

    let flush = |out: &mut String, rep_line: &str, tmpl: &str, run: usize| {
        out.push_str(rep_line);
        if run >= MIN_RUN {
            out.push_str("  [x");
            out.push_str(&run.to_string());
            // Only append `~ <template>` when the template actually differs from
            // the representative line (i.e. at least one variable field was
            // masked). When no field varies, the template == the raw line, so
            // repeating it is pure redundancy that can inflate the TOKEN count
            // above the original run even though bytes shrink. In that case emit
            // the terse `[xN]` form. Positive runs with real placeholders keep
            // the informative `[xN ~ <TEMPLATE>]` shape.
            if tmpl != rep_line {
                out.push_str(" ~ ");
                out.push_str(tmpl);
            }
            out.push(']');
        }
        out.push('\n');
    };

    for line in content.lines() {
        cur_tmpl.clear();
        log_template(line, &mut cur_tmpl);
        match prev_line {
            None => {
                prev_line = Some(line);
                prev_tmpl.clear();
                prev_tmpl.push_str(&cur_tmpl);
                run = 1;
            }
            Some(_) if cur_tmpl == prev_tmpl => run += 1,
            Some(p) => {
                flush(&mut out, p, &prev_tmpl, run);
                prev_line = Some(line);
                prev_tmpl.clear();
                prev_tmpl.push_str(&cur_tmpl);
                run = 1;
            }
        }
    }
    if let Some(p) = prev_line {
        flush(&mut out, p, &prev_tmpl, run);
    }

    Some(out.trim_end().to_string())
}

// ── log template masker — std-only byte scanners ────────────────────────────

/// Render `line` into its template by masking variable fields, writing into a
/// reused buffer. Ordering is most-specific-first so a span is consumed before
/// a broader masker can chip it (the over-masking defense):
/// TS -> UUID -> IP -> DUR -> HEX -> KV(keep key) -> INT.
fn log_template(line: &str, buf: &mut String) {
    let b = line.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(j) = take_timestamp(b, i) {
            buf.push_str("<TS>");
            i = j;
            continue;
        }
        if let Some(j) = take_uuid(b, i) {
            buf.push_str("<UUID>");
            i = j;
            continue;
        }
        if let Some(j) = take_ipv4(b, i) {
            buf.push_str("<IP>");
            i = j;
            continue;
        }
        if let Some(j) = take_duration(b, i) {
            buf.push_str("<DUR>");
            i = j;
            continue;
        }
        if let Some(j) = take_hex(b, i) {
            buf.push_str("<HEX>");
            i = j;
            continue;
        }
        // key=value: emit the '=' and mask ONLY the value (keep the key), so
        // structurally different key-sets produce different templates.
        if b[i] == b'=' {
            buf.push('=');
            if let Some(j) = take_kv_value(b, i + 1) {
                buf.push_str("<KV>");
                i = j;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(j) = take_int(b, i) {
            buf.push_str("<INT>");
            i = j;
            continue;
        }
        copy_next_utf8_char(line, &mut i, buf);
    }
}

/// Advance `*i` by one full UTF-8 char and copy those bytes into `buf`, so
/// multibyte text is never split mid-codepoint.
fn copy_next_utf8_char(line: &str, i: &mut usize, buf: &mut String) {
    let b = line.as_bytes();
    let start = *i;
    // UTF-8 leading-byte length (1..=4); default 1 for any stray continuation.
    let len = match b[start] {
        x if x < 0x80 => 1,
        x if x >> 5 == 0b110 => 2,
        x if x >> 4 == 0b1110 => 3,
        x if x >> 3 == 0b11110 => 4,
        _ => 1,
    };
    let end = (start + len).min(b.len());
    // Safe because `line` is valid UTF-8 and we advance on codepoint boundaries.
    buf.push_str(&line[start..end]);
    *i = end;
}

#[inline]
fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// ISO-8601 timestamp: `YYYY-MM-DD[T ]hh:mm:ss(.frac)?(Z|+-hh(:?mm)?)?`.
/// Requires the full date+time skeleton (offsets 4/7 '-', 10 'T'/' ', 13/16 ':').
fn take_timestamp(b: &[u8], i: usize) -> Option<usize> {
    let d = |k: usize| b.get(k).is_some_and(|c| c.is_ascii_digit());
    // YYYY-MM-DD
    if !(d(i) && d(i + 1) && d(i + 2) && d(i + 3)) {
        return None;
    }
    if b.get(i + 4) != Some(&b'-') || b.get(i + 7) != Some(&b'-') {
        return None;
    }
    if !(d(i + 5) && d(i + 6) && d(i + 8) && d(i + 9)) {
        return None;
    }
    // separator [T ]
    match b.get(i + 10) {
        Some(&b'T') | Some(&b' ') => {}
        _ => return None,
    }
    // hh:mm:ss
    if !(d(i + 11) && d(i + 12)) || b.get(i + 13) != Some(&b':') {
        return None;
    }
    if !(d(i + 14) && d(i + 15)) || b.get(i + 16) != Some(&b':') {
        return None;
    }
    if !(d(i + 17) && d(i + 18)) {
        return None;
    }
    let mut j = i + 19;
    // optional fractional seconds .\d{1,9}
    if b.get(j) == Some(&b'.') {
        let mut k = j + 1;
        while b.get(k).is_some_and(|c| c.is_ascii_digit()) {
            k += 1;
        }
        if k > j + 1 {
            j = k;
        }
    }
    // optional zone: Z | +-hh(:?mm)?
    match b.get(j) {
        Some(&b'Z') => j += 1,
        Some(&b'+') | Some(&b'-') => {
            if d(j + 1) && d(j + 2) {
                let mut k = j + 3;
                if b.get(k) == Some(&b':') && d(k + 1) && d(k + 2) {
                    k += 3;
                } else if d(k) && d(k + 1) {
                    k += 2;
                }
                j = k;
            }
        }
        _ => {}
    }
    Some(j)
}

/// UUID: 8-4-4-4-12 hex with hyphens. Left/right word-boundary guarded.
fn take_uuid(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && is_word_byte(b[i - 1]) {
        return None;
    }
    let hex = |k: usize| b.get(k).is_some_and(|c| c.is_ascii_hexdigit());
    let group = |start: usize, n: usize| -> bool { (0..n).all(|o| hex(start + o)) };
    let mut j = i;
    for (idx, &n) in [8usize, 4, 4, 4, 12].iter().enumerate() {
        if !group(j, n) {
            return None;
        }
        j += n;
        if idx < 4 {
            if b.get(j) != Some(&b'-') {
                return None;
            }
            j += 1;
        }
    }
    // must not continue into another hex/word char (else it's a longer token)
    if b.get(j).is_some_and(|&c| is_word_byte(c) || c == b'-') {
        return None;
    }
    Some(j)
}

/// IPv4 with optional `:port`. All four octets required.
fn take_ipv4(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && (is_word_byte(b[i - 1]) || b[i - 1] == b'.') {
        return None;
    }
    let mut j = i;
    let take_octet = |b: &[u8], mut k: usize| -> Option<usize> {
        let s = k;
        while b.get(k).is_some_and(|c| c.is_ascii_digit()) && k - s < 3 {
            k += 1;
        }
        if k == s {
            None
        } else {
            Some(k)
        }
    };
    for octet in 0..4 {
        j = take_octet(b, j)?;
        if octet < 3 {
            if b.get(j) != Some(&b'.') {
                return None;
            }
            j += 1;
        }
    }
    // reject a 5th dotted group (not an IPv4)
    if b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    // optional :port
    if b.get(j) == Some(&b':') && b.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
        let mut k = j + 1;
        while b.get(k).is_some_and(|c| c.is_ascii_digit()) && k - (j + 1) < 5 {
            k += 1;
        }
        j = k;
    }
    if b.get(j).is_some_and(|&c| is_word_byte(c)) {
        return None;
    }
    Some(j)
}

/// Duration / size: `\d+(\.\d+)?(ns|us|ms|s|m|h|kb|mb|gb|B)` word-bounded.
fn take_duration(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && (is_word_byte(b[i - 1]) || b[i - 1] == b'.') {
        return None;
    }
    let mut j = i;
    while b.get(j).is_some_and(|c| c.is_ascii_digit()) {
        j += 1;
    }
    if j == i {
        return None;
    }
    // optional fractional part
    if b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
        j += 1;
        while b.get(j).is_some_and(|c| c.is_ascii_digit()) {
            j += 1;
        }
    }
    // unit suffix (longest-match; case-sensitive set)
    let rest = &b[j..];
    const UNITS: [&[u8]; 12] = [
        b"ns", b"us", b"ms", b"kb", b"mb", b"gb", b"KB", b"MB", b"GB", b"s", b"m", b"h",
    ];
    let mut unit_len = 0usize;
    for u in UNITS {
        if rest.starts_with(u) && u.len() > unit_len {
            unit_len = u.len();
        }
    }
    // single-byte 'B' (bytes) only if not part of a longer unit already matched
    if unit_len == 0 && rest.first() == Some(&b'B') {
        unit_len = 1;
    }
    if unit_len == 0 {
        return None;
    }
    let end = j + unit_len;
    // must be word-bounded: next byte not alphanumeric/_ (so `500ms` yes, `500msg` no)
    if b.get(end).is_some_and(|&c| is_word_byte(c)) {
        return None;
    }
    Some(end)
}

/// Hex id: `0x[0-9a-fA-F]+` OR bare `[0-9a-fA-F]{4,}` containing >=1 a-f.
/// A purely-decimal run is left for `take_int`.
///
/// Length floor is >=4 (L4 fix): real request/correlation ids are frequently
/// 4-8 hex chars (`req-1a2b`, short shas). The both-sides non-word guards keep
/// this from chipping hex-looking fragments out of a larger word, and the
/// mandatory alpha keeps it from swallowing short decimals. A 4-char id
/// (`1a2b`) slipped through at the old >=6 floor and produced 0% compression on
/// an otherwise-identical access-log template.
fn take_hex(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && is_word_byte(b[i - 1]) {
        return None;
    }
    // 0x-prefixed
    if b.get(i) == Some(&b'0') && matches!(b.get(i + 1), Some(&b'x') | Some(&b'X')) {
        let mut j = i + 2;
        while b.get(j).is_some_and(|c| c.is_ascii_hexdigit()) {
            j += 1;
        }
        if j > i + 2 {
            if b.get(j).is_some_and(|&c| is_word_byte(c)) {
                return None;
            }
            return Some(j);
        }
        return None;
    }
    // bare hex run: a token of hex digits containing at least one a-f/A-F
    // (so pure-digit ints are excluded and handled by take_int).
    let mut j = i;
    let mut has_alpha = false;
    while let Some(&c) = b.get(j) {
        if c.is_ascii_digit() {
            j += 1;
        } else if c.is_ascii_hexdigit() {
            has_alpha = true;
            j += 1;
        } else {
            break;
        }
    }
    if j - i >= 4 && has_alpha && !b.get(j).is_some_and(|&c| is_word_byte(c)) {
        return Some(j);
    }
    None
}

/// Standalone integer, applied LAST. Not adjacent to `.`/word char, so it can't
/// chip a version/float/id already handled.
fn take_int(b: &[u8], i: usize) -> Option<usize> {
    if !b.get(i).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    // left guard: preceding byte not a word char or '.'
    if i > 0 && (is_word_byte(b[i - 1]) || b[i - 1] == b'.') {
        return None;
    }
    let mut j = i;
    while b.get(j).is_some_and(|c| c.is_ascii_digit()) {
        j += 1;
    }
    // right guard: following byte not a word char or '.'
    if b.get(j).is_some_and(|&c| is_word_byte(c) || c == b'.') {
        return None;
    }
    Some(j)
}

/// Mask a `key=value` VALUE span starting at byte `i` (just after the `=`),
/// keeping the key. A quoted `"..."` value is consumed whole; otherwise the
/// value runs up to whitespace or a comma. Returns None when there is no value
/// (so a bare `=` stays literal).
fn take_kv_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i) {
        None => None,
        Some(&b'"') => {
            let mut j = i + 1;
            while let Some(&c) = b.get(j) {
                j += 1;
                if c == b'"' {
                    break;
                }
            }
            Some(j)
        }
        Some(&c) if c == b' ' || c == b'\t' || c == b',' => None,
        Some(_) => {
            let mut j = i;
            while let Some(&c) = b.get(j) {
                if c == b' ' || c == b'\t' || c == b',' {
                    break;
                }
                j += 1;
            }
            Some(j)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pares_radix_core::state::PluresDbStateStore;

    fn make_hook(min_tokens: usize) -> HeadroomHook {
        let store = Arc::new(PluresDbStateStore::in_memory());
        let handler = Arc::new(HeadroomActionHandler::new(store.crdt_store()));
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
        let handler = Arc::new(HeadroomActionHandler::new(store.crdt_store()));
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
        // 80 distinct sentences ΓåÆ should trim the middle.
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
        // No variable field in `ERROR boom`, so template == raw line and the
        // marker is the terse ASCII `[x3]` form (Path-2). The old `[×3]`
        // (U+00D7) marker was replaced to avoid the mojibake-on-disk artifact.
        assert!(out.contains("[x3]"), "missing terse run marker:\n{out}");
        assert!(out.contains("INFO ok"));
    }

    // ═══════════════════════════════════════════════════════════
    // PATH-2 FIXTURE GATE (ported from the pluresdb-native origin,
    // crates/pluresdb-node/src/headroom.rs). Each POSITIVE case must collapse
    // to the expected run-count, carry a `[x` marker, and keep its first raw
    // line verbatim. Each NEGATIVE case must be byte-identical to the input
    // (trimmed) with NO `[x` marker — the over-masking guard. Gate tests call
    // the module-level free fn `compress_log_impl` directly (the algorithm
    // under test), matching the origin's free-fn tests.
    // ═══════════════════════════════════════════════════════════

    const FX_CASE1: &str = concat!(
        "2026-07-02T10:15:03.123Z INFO  [req-a1b2c3d4] handler=GET /api/users status=200 dur=12ms\n",
        "2026-07-02T10:15:03.451Z INFO  [req-e5f6a7b8] handler=GET /api/users status=200 dur=9ms\n",
        "2026-07-02T10:15:03.998Z INFO  [req-9c0d1e2f] handler=GET /api/users status=200 dur=14ms\n",
        "2026-07-02T10:15:04.203Z INFO  [req-3a4b5c6d] handler=GET /api/users status=200 dur=11ms\n",
        "2026-07-02T10:15:04.774Z INFO  [req-7e8f9a0b] handler=GET /api/users status=200 dur=10ms\n",
        "2026-07-02T10:15:05.012Z INFO  [req-1c2d3e4f] handler=GET /api/users status=200 dur=13ms\n",
    );

    // L4 fix fixture: SHORT (4-char) hex request ids. Same access-log template as
    // case1 but with `req-<4hex>` instead of 8 — the width that slipped through the
    // old >=6 hex floor and produced 0% compression. Must now collapse 6 -> 1.
    const FX_CASE7: &str = concat!(
        "2026-07-02T10:15:03.123Z INFO  [req-1a2b] handler=GET /api/users status=200 dur=12ms\n",
        "2026-07-02T10:15:03.451Z INFO  [req-1a2c] handler=GET /api/users status=200 dur=9ms\n",
        "2026-07-02T10:15:03.998Z INFO  [req-1a2d] handler=GET /api/users status=200 dur=14ms\n",
        "2026-07-02T10:15:04.203Z INFO  [req-1a2e] handler=GET /api/users status=200 dur=11ms\n",
        "2026-07-02T10:15:04.774Z INFO  [req-1a2f] handler=GET /api/users status=200 dur=10ms\n",
        "2026-07-02T10:15:05.012Z INFO  [req-1a30] handler=GET /api/users status=200 dur=13ms\n",
    );

    const FX_CASE2: &str = concat!(
        "2026-07-02T10:15:03.123Z INFO  worker=3 processed batch id=48213 items=500\n",
        "2026-07-02T10:15:03.451Z INFO  worker=1 processed batch id=48214 items=500\n",
        "2026-07-02T10:15:03.998Z WARN  worker=3 retry batch id=48215 attempt=1\n",
        "2026-07-02T10:15:04.203Z INFO  worker=2 processed batch id=48216 items=500\n",
        "2026-07-02T10:15:04.774Z INFO  worker=1 processed batch id=48217 items=500\n",
        "2026-07-02T10:15:05.012Z ERROR worker=2 failed batch id=48218 err=timeout\n",
    );

    const FX_CASE3: &str = concat!(
        "[2026-07-02 10:15:03] DEBUG conn 0x7f3a1c00 opened from 10.0.0.14:52001\n",
        "[2026-07-02 10:15:03] DEBUG conn 0x7f3a2d10 opened from 10.0.0.22:52002\n",
        "[2026-07-02 10:15:04] DEBUG conn 0x7f3a3e20 opened from 10.0.0.9:52003\n",
        "[2026-07-02 10:15:04] DEBUG conn 0x7f3a4f30 opened from 10.0.0.31:52004\n",
        "[2026-07-02 10:15:05] DEBUG conn 0x7f3a5a40 opened from 10.0.0.7:52005\n",
    );

    const FX_CASE4: &str = concat!(
        "INFO 2026-07-02T10:15:03.001Z request 550e8400-e29b-41d4-a716-446655440000 completed in 45ms\n",
        "INFO 2026-07-02T10:15:03.502Z request 6ba7b810-9dad-11d1-80b4-00c04fd430c8 completed in 52ms\n",
        "INFO 2026-07-02T10:15:04.003Z request 6ba7b811-9dad-11d1-80b4-00c04fd430c8 completed in 38ms\n",
        "INFO 2026-07-02T10:15:04.504Z request 6ba7b812-9dad-11d1-80b4-00c04fd430c8 completed in 61ms\n",
        "INFO 2026-07-02T10:15:05.005Z request 6ba7b814-9dad-11d1-80b4-00c04fd430c8 completed in 47ms\n",
    );

    const FX_CASE5: &str = concat!(
        "2026-07-02T10:15:03.123Z INFO  cache hit key=user:1001 ttl=300s\n",
        "2026-07-02T10:15:03.201Z INFO  cache hit key=user:1002 ttl=300s\n",
        "2026-07-02T10:15:03.288Z INFO  cache miss key=user:1003 -> fetch db\n",
        "2026-07-02T10:15:03.377Z INFO  cache hit key=user:1004 ttl=300s\n",
        "2026-07-02T10:15:03.466Z INFO  cache hit key=user:1005 ttl=300s\n",
        "2026-07-02T10:15:03.555Z INFO  cache miss key=user:1006 -> fetch db\n",
        "2026-07-02T10:15:03.644Z INFO  cache hit key=user:1007 ttl=300s\n",
    );

    const FX_CASE6: &str = concat!(
        "2026-07-02T10:15:03.123Z ERROR db connection failed host=10.0.0.5 port=5432 err=\"connection refused\"\n",
        "2026-07-02T10:15:03.623Z ERROR db connection failed host=10.0.0.5 port=5432 err=\"connection refused\"\n",
        "2026-07-02T10:15:04.123Z ERROR db connection failed host=10.0.0.5 port=5432 err=\"connection refused\"\n",
        "2026-07-02T10:15:04.623Z ERROR db connection failed host=10.0.0.5 port=5432 err=\"connection refused\"\n",
        "2026-07-02T10:15:05.123Z ERROR db connection failed host=10.0.0.5 port=5432 err=\"connection refused\"\n",
    );

    const FX_NEG1: &str = concat!(
        "2026-07-02T10:15:03.123Z INFO  starting service version=2.4.1 pid=8842\n",
        "2026-07-02T10:15:03.140Z INFO  loaded config from /etc/app/config.yaml sections=12\n",
        "2026-07-02T10:15:03.155Z WARN  deprecated flag --legacy-mode will be removed in 3.0\n",
        "2026-07-02T10:15:03.170Z INFO  bound listener addr=0.0.0.0:8080 tls=true\n",
        "2026-07-02T10:15:03.185Z ERROR failed to open plugin dir /opt/app/plugins: no such file or directory\n",
        "2026-07-02T10:15:03.200Z INFO  service ready in 77ms\n",
    );

    const FX_NEG2: &str = concat!(
        "2026-07-02T10:15:03.123Z INFO  user=alice action=login result=ok\n",
        "2026-07-02T10:15:03.451Z INFO  user=bob action=purchase amount=49.99 currency=USD\n",
        "2026-07-02T10:15:03.998Z INFO  user=carol action=logout session_dur=3600s\n",
        "2026-07-02T10:15:04.203Z INFO  user=dave action=login result=fail reason=bad_password\n",
        "2026-07-02T10:15:04.774Z INFO  user=erin action=update_profile fields=email,phone\n",
    );

    /// Assert a POSITIVE fixture collapses to `expected_lines`, shows a `[x`
    /// marker, and keeps its first RAW input line verbatim as line 1.
    fn assert_positive(name: &str, input: &str, expected_lines: usize) {
        let out = compress_log_impl(input).expect("compress_log_impl returned None");
        let n = out.lines().count();
        assert_eq!(
            n, expected_lines,
            "{name}: expected {expected_lines} output lines, got {n}\n---\n{out}\n---"
        );
        assert!(out.contains("[x"), "{name}: missing [x run marker:\n{out}");
        // The representative is the first RAW line verbatim, with the marker
        // appended on the SAME line: so line 1 STARTS WITH the raw first input
        // line and the marker is its suffix.
        let first_raw = input.lines().next().unwrap();
        let first_out = out.lines().next().unwrap();
        assert!(
            first_out.starts_with(first_raw),
            "{name}: first raw line not kept verbatim as representative\n  raw: {first_raw}\n  out: {first_out}"
        );
        assert!(
            first_out.ends_with(']') && first_out[first_raw.len()..].contains("[x"),
            "{name}: marker not appended to first raw line\n  out: {first_out}"
        );
        // net win: template-collapsed output is strictly shorter than input.
        assert!(out.len() < input.len(), "{name}: output not shorter than input");
    }

    /// Assert a NEGATIVE fixture is emitted byte-identical to `input.trim_end()`
    /// with NO `[x` marker anywhere (no over-masking / spurious collapse).
    fn assert_negative(name: &str, input: &str) {
        let out = compress_log_impl(input).expect("compress_log_impl returned None");
        assert!(!out.contains("[x"), "{name}: spurious run-collapse marker:\n{out}");
        assert_eq!(
            out,
            input.trim_end(),
            "{name}: negative fixture was mutated (expected verbatim passthrough)"
        );
    }

    #[test]
    fn compress_log_gate_case1_access_log_same_template() {
        // 6 access lines, identical template (TS/req-id/dur vary) -> 1.
        assert_positive("case1", FX_CASE1, 1);
    }

    #[test]
    fn compress_log_gate_case2_mixed_levels_interspersed() {
        // INFO,INFO -> [x2]; WARN single; INFO,INFO -> [x2]; ERROR single => 4.
        assert_positive("case2", FX_CASE2, 4);
    }

    #[test]
    fn compress_log_gate_case3_bracketed_hex_ip() {
        // 5 DEBUG conn lines (hex ptr + IP:port vary), identical template -> 1.
        assert_positive("case3", FX_CASE3, 1);
    }

    #[test]
    fn compress_log_gate_case4_level_prefixed_uuid_duration() {
        // 5 level-prefixed lines (UUID + Nms vary), identical template -> 1.
        assert_positive("case4", FX_CASE4, 1);
    }

    #[test]
    fn compress_log_gate_case5_two_alternating_templates() {
        // hit,hit -> [x2]; miss single; hit,hit -> [x2]; miss single; hit single => 5.
        assert_positive("case5", FX_CASE5, 5);
    }

    #[test]
    fn compress_log_gate_case6_only_timestamp_varies() {
        // 5 ERROR lines differing ONLY in timestamp -> 1 (the headline Path-2 win).
        assert_positive("case6", FX_CASE6, 1);
    }

    #[test]
    fn compress_log_gate_case7_short_hex_req_id() {
        // L4 regression: 6 access lines with 4-char hex req-ids -> 1. Guards the
        // >=4 hex floor so short correlation ids normalize (was 0% at >=6).
        assert_positive("case7", FX_CASE7, 1);
    }

    #[test]
    fn compress_log_gate_neg1_all_distinct_startup() {
        // 6 structurally-distinct startup lines -> 0 collapses, verbatim.
        assert_negative("neg1", FX_NEG1);
    }

    #[test]
    fn compress_log_gate_neg2_same_prefix_different_structure() {
        // Shared prefix but different trailing key-sets (KV keeps keys) -> verbatim.
        assert_negative("neg2", FX_NEG2);
    }

    #[test]
    fn compress_log_gate_all_fixtures_summary() {
        // Single roll-up so a run of `compress_log_impl` prints every before->after.
        let cases: [(&str, &str, Option<usize>); 8] = [
            ("case1", FX_CASE1, Some(1)),
            ("case2", FX_CASE2, Some(4)),
            ("case3", FX_CASE3, Some(1)),
            ("case4", FX_CASE4, Some(1)),
            ("case5", FX_CASE5, Some(5)),
            ("case6", FX_CASE6, Some(1)),
            ("neg1", FX_NEG1, None),
            ("neg2", FX_NEG2, None),
        ];
        for (name, input, expected) in cases {
            let before = input.lines().count();
            let out = compress_log_impl(input).unwrap();
            let after = out.lines().count();
            match expected {
                Some(exp) => assert_eq!(
                    after, exp,
                    "{name}: {before}->{after} (expected {exp})"
                ),
                None => assert_eq!(
                    after, before,
                    "{name}: negative changed line count {before}->{after}"
                ),
            }
        }
    }
}
