//! Structured audit event definitions.
//!
//! [`AuditEvent`] is the single immutable record appended to the audit log for
//! every action that touches data.  [`EventKind`] enumerates the five event
//! categories required by the specification:
//!
//! | Kind          | When emitted                                        |
//! |---------------|-----------------------------------------------------|
//! | `ModelCall`   | Outbound request to any language model.             |
//! | `MemoryWrite` | A new memory entry is persisted.                    |
//! | `MemoryRead`  | Memory entries are recalled / injected into context.|
//! | `ToolExec`    | An agent-called tool / function executes.           |
//! | `ChannelSend` | A message is dispatched over a channel adapter.     |

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// EventKind
// ---------------------------------------------------------------------------

/// The category of the audited action.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Outbound request to a language model (local or remote).
    ModelCall,
    /// A memory entry was written to the memory store.
    MemoryWrite,
    /// Memory entries were read / recalled from the store.
    MemoryRead,
    /// An agent tool was executed.
    ToolExec,
    /// A message was sent over a channel adapter (Telegram, stdin, IPC, …).
    ChannelSend,
}

impl EventKind {
    /// Human-readable label used in CSV exports.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ModelCall => "model-call",
            Self::MemoryWrite => "memory-write",
            Self::MemoryRead => "memory-read",
            Self::ToolExec => "tool-exec",
            Self::ChannelSend => "channel-send",
        }
    }
}

// ---------------------------------------------------------------------------
// AuditEvent
// ---------------------------------------------------------------------------

/// A single immutable record in the comprehensive audit log.
///
/// Every field that could vary is captured at creation time and is thereafter
/// read-only.  The struct derives [`Serialize`] / [`Deserialize`] so it can be
/// persisted as JSON or exported for compliance review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique identifier for this log entry (UUIDv4).
    pub id: String,
    /// RFC 3339 timestamp of when the event was recorded.
    pub timestamp: String,
    /// Identity of the component or user that triggered the action
    /// (e.g. `"agent-1"`, `"user:alice"`, `"cron-scheduler"`).
    pub actor: String,
    /// The kind of action that was performed.
    pub kind: EventKind,
    /// Brief human-readable summary of the data involved
    /// (e.g. `"prompt tokens: 512"`, `"memory id: abc123"`).
    /// **Must not** contain raw PII — scrub before setting.
    pub data_summary: String,
    /// Logical destination of the action
    /// (e.g. model name, memory category, tool name, channel adapter id).
    pub destination: String,
    /// `true` when the event involved data that is or may be PII.
    pub pii_flag: bool,
}

impl AuditEvent {
    /// Create a new [`AuditEvent`] stamped with the current UTC time.
    ///
    /// # Arguments
    ///
    /// * `kind`         — The [`EventKind`] of the action.
    /// * `actor`        — Identifier of the component/user performing the action.
    /// * `destination`  — Where the data is going (model, store, channel, tool).
    /// * `data_summary` — Short description of the data involved (no raw PII).
    /// * `pii_flag`     — Whether PII was involved in the action.
    pub fn new(
        kind: EventKind,
        actor: impl Into<String>,
        destination: impl Into<String>,
        data_summary: impl Into<String>,
        pii_flag: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            actor: actor.into(),
            kind,
            data_summary: data_summary.into(),
            destination: destination.into(),
            pii_flag,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_labels() {
        assert_eq!(EventKind::ModelCall.as_str(), "model-call");
        assert_eq!(EventKind::MemoryWrite.as_str(), "memory-write");
        assert_eq!(EventKind::MemoryRead.as_str(), "memory-read");
        assert_eq!(EventKind::ToolExec.as_str(), "tool-exec");
        assert_eq!(EventKind::ChannelSend.as_str(), "channel-send");
    }

    #[test]
    fn new_event_has_unique_ids() {
        let a = AuditEvent::new(EventKind::ModelCall, "actor", "dest", "summary", false);
        let b = AuditEvent::new(EventKind::ModelCall, "actor", "dest", "summary", false);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn new_event_captures_fields() {
        let ev = AuditEvent::new(
            EventKind::ToolExec,
            "agent-x",
            "tool:search",
            "query len: 42",
            true,
        );
        assert_eq!(ev.kind, EventKind::ToolExec);
        assert_eq!(ev.actor, "agent-x");
        assert_eq!(ev.destination, "tool:search");
        assert_eq!(ev.data_summary, "query len: 42");
        assert!(ev.pii_flag);
        assert!(!ev.timestamp.is_empty());
    }

    #[test]
    fn serialise_round_trip() {
        let ev = AuditEvent::new(
            EventKind::ChannelSend,
            "svc",
            "telegram",
            "msg len: 10",
            false,
        );
        let json = serde_json::to_string(&ev).unwrap();
        let restored: AuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev.id, restored.id);
        assert_eq!(ev.kind, restored.kind);
        assert_eq!(ev.pii_flag, restored.pii_flag);
    }
}
