//! Application-level metrics instrumentation using OpenTelemetry.
//!
//! Emits tool latency histograms, call counters, and error counters via the
//! OTel global meter provider. When no OTLP exporter is configured, instruments
//! are no-ops (zero overhead).
//!
//! # Metrics emitted
//!
//! | Metric | Type | Unit | Attributes |
//! |--------|------|------|------------|
//! | `radix.tool.duration` | Histogram | ms | tool, success |
//! | `radix.tool.calls` | Counter | 1 | tool, success |
//! | `radix.tool.errors` | Counter | 1 | tool |
//! | `radix.model.duration` | Histogram | ms | model, provider |
//! | `radix.model.calls` | Counter | 1 | model, provider |
//! | `radix.session.active` | UpDownCounter | 1 | — |

use opentelemetry::metrics::{Counter, Histogram, UpDownCounter};
use opentelemetry::{global, KeyValue};

/// Application metrics bundle — create once, clone cheaply.
#[derive(Clone)]
pub struct AppMetrics {
    tool_duration: Histogram<f64>,
    tool_calls: Counter<u64>,
    tool_errors: Counter<u64>,
    model_duration: Histogram<f64>,
    model_calls: Counter<u64>,
    active_sessions: UpDownCounter<i64>,
}

impl AppMetrics {
    /// Create instruments from the global meter provider.
    pub fn new() -> Self {
        let meter = global::meter("pares-radix");

        let tool_duration = meter
            .f64_histogram("radix.tool.duration")
            .with_description("Tool call duration in milliseconds")
            .with_unit("ms")
            .build();

        let tool_calls = meter
            .u64_counter("radix.tool.calls")
            .with_description("Total tool calls")
            .build();

        let tool_errors = meter
            .u64_counter("radix.tool.errors")
            .with_description("Total tool call errors")
            .build();

        let model_duration = meter
            .f64_histogram("radix.model.duration")
            .with_description("Model call duration in milliseconds")
            .with_unit("ms")
            .build();

        let model_calls = meter
            .u64_counter("radix.model.calls")
            .with_description("Total model calls")
            .build();

        let active_sessions = meter
            .i64_up_down_counter("radix.session.active")
            .with_description("Currently active sessions")
            .build();

        Self {
            tool_duration,
            tool_calls,
            tool_errors,
            model_duration,
            model_calls,
            active_sessions,
        }
    }

    /// Record a tool call completion.
    pub fn record_tool_call(&self, tool_name: &str, latency_ms: f64, success: bool) {
        let attrs = [
            KeyValue::new("tool", tool_name.to_string()),
            KeyValue::new("success", success.to_string()),
        ];
        self.tool_duration.record(latency_ms, &attrs);
        self.tool_calls.add(1, &attrs);
        if !success {
            self.tool_errors
                .add(1, &[KeyValue::new("tool", tool_name.to_string())]);
        }
    }

    /// Record a model (LLM) call completion.
    pub fn record_model_call(&self, model: &str, provider: &str, latency_ms: f64) {
        let attrs = [
            KeyValue::new("model", model.to_string()),
            KeyValue::new("provider", provider.to_string()),
        ];
        self.model_duration.record(latency_ms, &attrs);
        self.model_calls.add(1, &attrs);
    }

    /// Increment active session count.
    pub fn session_started(&self) {
        self.active_sessions.add(1, &[]);
    }

    /// Decrement active session count.
    pub fn session_ended(&self) {
        self.active_sessions.add(-1, &[]);
    }
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_metrics_creation() {
        // Should not panic even without a real meter provider
        let metrics = AppMetrics::new();
        // Instruments are no-ops by default
        metrics.record_tool_call("read_file", 42.0, true);
        metrics.record_tool_call("run_command", 1500.0, false);
        metrics.record_model_call("gpt-4.1", "openai", 2300.0);
        metrics.session_started();
        metrics.session_ended();
    }

    #[test]
    fn test_app_metrics_default() {
        let metrics = AppMetrics::default();
        metrics.record_tool_call("test_tool", 10.0, true);
    }

    #[test]
    fn test_app_metrics_clone() {
        let metrics = AppMetrics::new();
        let cloned = metrics.clone();
        cloned.record_tool_call("cloned_call", 5.0, true);
        metrics.record_tool_call("original_call", 3.0, true);
    }

    #[test]
    fn test_record_tool_call_success() {
        let metrics = AppMetrics::new();
        // Success path
        metrics.record_tool_call("memory_search", 150.0, true);
        // Should not panic, instruments are no-op without provider
    }

    #[test]
    fn test_record_tool_call_failure() {
        let metrics = AppMetrics::new();
        // Failure path — records to errors counter too
        metrics.record_tool_call("web_fetch", 5000.0, false);
    }

    #[test]
    fn test_record_model_call() {
        let metrics = AppMetrics::new();
        metrics.record_model_call("claude-opus-4", "anthropic", 3200.0);
        metrics.record_model_call("gpt-4.1-mini", "openai", 800.0);
    }

    #[test]
    fn test_session_lifecycle() {
        let metrics = AppMetrics::new();
        metrics.session_started();
        metrics.session_started();
        metrics.session_ended();
        metrics.session_ended();
    }
}
