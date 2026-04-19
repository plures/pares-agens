//! Anonymous telemetry aggregation primitives.
//!
//! This module stores only aggregate counters and latency statistics:
//! - model calls per day
//! - tool usage frequency
//! - response latency summary
//!
//! No conversation content, prompts, tool arguments, or user identifiers are
//! represented in these types.

use std::collections::BTreeMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Aggregate anonymous telemetry counters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryAggregate {
    #[serde(default)]
    model_calls_by_day: BTreeMap<String, u64>,
    #[serde(default)]
    tool_usage_frequency: BTreeMap<String, u64>,
    #[serde(default)]
    latency_sample_count: u64,
    #[serde(default)]
    latency_total_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latency_min_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latency_max_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_upload_at: Option<String>,
}

/// Read-only telemetry dashboard snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySnapshot {
    /// Number of model calls grouped by UTC day (`YYYY-MM-DD`).
    pub model_calls_by_day: BTreeMap<String, u64>,
    /// Number of tool invocations by tool name.
    pub tool_usage_frequency: BTreeMap<String, u64>,
    /// Number of latency samples included in the aggregate.
    pub latency_sample_count: u64,
    /// Sum of all latency samples in milliseconds.
    pub latency_total_ms: u64,
    /// Minimum observed latency in milliseconds.
    pub latency_min_ms: Option<u64>,
    /// Maximum observed latency in milliseconds.
    pub latency_max_ms: Option<u64>,
    /// Average latency in milliseconds.
    pub avg_latency_ms: Option<f64>,
    /// UTC timestamp of the most recent successful telemetry upload.
    pub last_upload_at: Option<String>,
}

impl TelemetryAggregate {
    /// Record a model call for the current UTC day.
    pub fn record_model_call(&mut self, latency_ms: u64) {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        self.record_model_call_for_day(&day, latency_ms);
    }

    /// Record a model call for a specific day.
    ///
    /// This variant is used by tests and deterministic backfills.
    pub fn record_model_call_for_day(&mut self, day: &str, latency_ms: u64) {
        *self.model_calls_by_day.entry(day.to_string()).or_insert(0) += 1;
        self.latency_sample_count = self.latency_sample_count.saturating_add(1);
        self.latency_total_ms = self.latency_total_ms.saturating_add(latency_ms);
        self.latency_min_ms = Some(
            self.latency_min_ms
                .map_or(latency_ms, |v| v.min(latency_ms)),
        );
        self.latency_max_ms = Some(
            self.latency_max_ms
                .map_or(latency_ms, |v| v.max(latency_ms)),
        );
    }

    /// Record a tool invocation count for `tool_name`.
    pub fn record_tool_usage(&mut self, tool_name: &str) {
        let name = tool_name.trim();
        if name.is_empty() {
            return;
        }
        *self
            .tool_usage_frequency
            .entry(name.to_string())
            .or_insert(0) += 1;
    }

    /// Mark aggregate data as uploaded at the current UTC timestamp.
    pub fn mark_uploaded_now(&mut self) {
        self.last_upload_at = Some(Utc::now().to_rfc3339());
    }

    /// Convert current aggregates into a serializable dashboard snapshot.
    pub fn snapshot(&self) -> TelemetrySnapshot {
        let avg_latency_ms = if self.latency_sample_count == 0 {
            None
        } else {
            Some(self.latency_total_ms as f64 / self.latency_sample_count as f64)
        };

        TelemetrySnapshot {
            model_calls_by_day: self.model_calls_by_day.clone(),
            tool_usage_frequency: self.tool_usage_frequency.clone(),
            latency_sample_count: self.latency_sample_count,
            latency_total_ms: self.latency_total_ms,
            latency_min_ms: self.latency_min_ms,
            latency_max_ms: self.latency_max_ms,
            avg_latency_ms,
            last_upload_at: self.last_upload_at.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TelemetryAggregate;

    #[test]
    fn model_calls_are_grouped_per_day() {
        let mut telemetry = TelemetryAggregate::default();
        telemetry.record_model_call_for_day("2026-04-17", 100);
        telemetry.record_model_call_for_day("2026-04-17", 300);
        telemetry.record_model_call_for_day("2026-04-18", 200);

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.model_calls_by_day.get("2026-04-17"), Some(&2));
        assert_eq!(snapshot.model_calls_by_day.get("2026-04-18"), Some(&1));
    }

    #[test]
    fn tool_usage_frequency_counts_invocations() {
        let mut telemetry = TelemetryAggregate::default();
        telemetry.record_tool_usage("filesystem.read");
        telemetry.record_tool_usage("filesystem.read");
        telemetry.record_tool_usage("time.now");
        telemetry.record_tool_usage("   ");

        let snapshot = telemetry.snapshot();
        assert_eq!(
            snapshot.tool_usage_frequency.get("filesystem.read"),
            Some(&2)
        );
        assert_eq!(snapshot.tool_usage_frequency.get("time.now"), Some(&1));
        assert_eq!(snapshot.tool_usage_frequency.len(), 2);
    }

    #[test]
    fn latency_summary_tracks_min_max_and_average() {
        let mut telemetry = TelemetryAggregate::default();
        telemetry.record_model_call_for_day("2026-04-18", 120);
        telemetry.record_model_call_for_day("2026-04-18", 240);
        telemetry.record_model_call_for_day("2026-04-18", 360);

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.latency_sample_count, 3);
        assert_eq!(snapshot.latency_min_ms, Some(120));
        assert_eq!(snapshot.latency_max_ms, Some(360));
        assert_eq!(snapshot.avg_latency_ms, Some(240.0));
    }
}
