//! JSON and CSV export helpers for compliance review.
//!
//! Both functions are pure transformations over a slice of [`AuditEvent`]s and
//! do not perform any I/O themselves — callers write the returned `String` to
//! whatever sink they need (file, HTTP response, Tauri command return value).
//!
//! # Example
//!
//! ```rust
//! use pares_agens_audit::{event::{AuditEvent, EventKind}, export::{export_json, export_csv}};
//!
//! let events = vec![
//!     AuditEvent::new(EventKind::ModelCall, "agent-1", "gpt-4o", "tokens: 42", false),
//! ];
//!
//! let json = export_json(&events).unwrap();
//! assert!(json.contains("model_call"));
//!
//! let csv = export_csv(&events).unwrap();
//! assert!(csv.contains("model-call"));
//! ```

use crate::event::AuditEvent;
use crate::AuditError;

// ---------------------------------------------------------------------------
// JSON export
// ---------------------------------------------------------------------------

/// Serialize `events` to a pretty-printed JSON array.
///
/// # Errors
///
/// Returns [`AuditError::Serialize`] when serde_json fails.
pub fn export_json(events: &[AuditEvent]) -> Result<String, AuditError> {
    serde_json::to_string_pretty(events).map_err(AuditError::Serialize)
}

// ---------------------------------------------------------------------------
// CSV export
// ---------------------------------------------------------------------------

/// The CSV header row.
const CSV_HEADER: &str = "id,timestamp,actor,kind,data_summary,destination,pii_flag";

/// Serialize `events` to a CSV string suitable for spreadsheet import.
///
/// Columns: `id`, `timestamp`, `actor`, `kind`, `data_summary`, `destination`, `pii_flag`.
///
/// Fields containing commas or double-quotes are wrapped in double-quotes
/// with internal double-quotes doubled (`""`).
///
/// # Errors
///
/// Currently infallible; returns `Ok` in all cases.  The `Result` return type
/// is provided for API consistency and forward-compatibility.
pub fn export_csv(events: &[AuditEvent]) -> Result<String, AuditError> {
    let mut rows = Vec::with_capacity(events.len() + 1);
    rows.push(CSV_HEADER.to_string());
    for e in events {
        rows.push(format!(
            "{},{},{},{},{},{},{}",
            csv_escape(&e.id),
            csv_escape(&e.timestamp),
            csv_escape(&e.actor),
            csv_escape(e.kind.as_str()),
            csv_escape(&e.data_summary),
            csv_escape(&e.destination),
            e.pii_flag,
        ));
    }
    Ok(rows.join("\n"))
}

/// Wrap `s` in double-quotes if it contains a comma, double-quote, or newline.
fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;

    fn sample_events() -> Vec<AuditEvent> {
        vec![
            AuditEvent::new(EventKind::ModelCall, "a1", "gpt-4o", "tokens: 100", false),
            AuditEvent::new(
                EventKind::MemoryWrite,
                "a2",
                "conv-store",
                "entry id: x",
                true,
            ),
        ]
    }

    #[test]
    fn json_export_is_array() {
        let json = export_json(&sample_events()).unwrap();
        assert!(json.trim_start().starts_with('['));
        assert!(json.trim_end().ends_with(']'));
    }

    #[test]
    fn json_export_contains_kind_label() {
        let json = export_json(&sample_events()).unwrap();
        // serde serialises EventKind with snake_case rename_all, so "model_call"
        assert!(json.contains("model_call"));
    }

    #[test]
    fn json_round_trip() {
        let events = sample_events();
        let json = export_json(&events).unwrap();
        let restored: Vec<AuditEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].actor, events[0].actor);
    }

    #[test]
    fn csv_has_header() {
        let csv = export_csv(&sample_events()).unwrap();
        assert!(csv.starts_with("id,timestamp,actor,kind,data_summary,destination,pii_flag"));
    }

    #[test]
    fn csv_has_correct_row_count() {
        let csv = export_csv(&sample_events()).unwrap();
        // 1 header + 2 data rows
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn csv_contains_pii_flag() {
        let csv = export_csv(&sample_events()).unwrap();
        assert!(csv.contains("false"));
        assert!(csv.contains("true"));
    }

    #[test]
    fn csv_escape_wraps_commas() {
        let s = csv_escape("a,b");
        assert_eq!(s, "\"a,b\"");
    }

    #[test]
    fn csv_escape_doubles_quotes() {
        let s = csv_escape("say \"hello\"");
        assert_eq!(s, "\"say \"\"hello\"\"\"");
    }

    #[test]
    fn csv_escape_passthrough_plain() {
        let s = csv_escape("hello");
        assert_eq!(s, "hello");
    }

    #[test]
    fn empty_slice_exports_header_only() {
        let json = export_json(&[]).unwrap();
        assert_eq!(json.trim(), "[]");

        let csv = export_csv(&[]).unwrap();
        assert_eq!(csv.lines().count(), 1);
    }
}
