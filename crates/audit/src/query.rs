//! Query builder for filtering audit events.
//!
//! [`AuditQuery`] provides a composable, builder-style API for filtering audit
//! events by date range, [`EventKind`], destination, actor, and PII flag.
//!
//! # Example
//!
//! ```rust
//! use pares_agens_audit::{query::AuditQuery, event::EventKind};
//!
//! let q = AuditQuery::new()
//!     .with_kind(EventKind::ModelCall)
//!     .with_destination("gpt-4o")
//!     .with_pii_only(true);
//!
//! // Pass to AuditStore::query() to get matching events.
//! ```

use crate::event::{AuditEvent, EventKind};

// ---------------------------------------------------------------------------
// AuditQuery
// ---------------------------------------------------------------------------

/// Builder-style query for filtering [`AuditEvent`]s.
///
/// All filters are optional and are **ANDed** together — an event must satisfy
/// every active filter to be included in the result set.
#[derive(Debug, Default, Clone)]
pub struct AuditQuery {
    /// Only include events at or after this RFC 3339 timestamp.
    pub since: Option<String>,
    /// Only include events at or before this RFC 3339 timestamp.
    pub until: Option<String>,
    /// Only include events of this [`EventKind`].
    pub kind: Option<EventKind>,
    /// Only include events whose `destination` equals this value.
    pub destination: Option<String>,
    /// Only include events whose `actor` equals this value.
    pub actor: Option<String>,
    /// When `Some(true)` include only PII-flagged events;
    /// when `Some(false)` include only non-PII events;
    /// when `None` include all events regardless of PII flag.
    pub pii_only: Option<bool>,
}

impl AuditQuery {
    /// Create a new, unfiltered query (returns all events).
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter to events at or after `timestamp` (RFC 3339).
    pub fn with_since(mut self, timestamp: impl Into<String>) -> Self {
        self.since = Some(timestamp.into());
        self
    }

    /// Filter to events at or before `timestamp` (RFC 3339).
    pub fn with_until(mut self, timestamp: impl Into<String>) -> Self {
        self.until = Some(timestamp.into());
        self
    }

    /// Filter to events of the given [`EventKind`].
    pub fn with_kind(mut self, kind: EventKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Filter to events whose `destination` matches `dest`.
    pub fn with_destination(mut self, dest: impl Into<String>) -> Self {
        self.destination = Some(dest.into());
        self
    }

    /// Filter to events whose `actor` matches `actor`.
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    /// When `pii_only` is `true`, return only PII-flagged events.
    /// When `false`, return only non-PII events.
    pub fn with_pii_only(mut self, pii_only: bool) -> Self {
        self.pii_only = Some(pii_only);
        self
    }

    /// Return `true` when `event` satisfies every active filter.
    pub fn matches(&self, event: &AuditEvent) -> bool {
        if let Some(ref since) = self.since {
            if event.timestamp.as_str() < since.as_str() {
                return false;
            }
        }
        if let Some(ref until) = self.until {
            if event.timestamp.as_str() > until.as_str() {
                return false;
            }
        }
        if let Some(ref kind) = self.kind {
            if &event.kind != kind {
                return false;
            }
        }
        if let Some(ref dest) = self.destination {
            if &event.destination != dest {
                return false;
            }
        }
        if let Some(ref actor) = self.actor {
            if &event.actor != actor {
                return false;
            }
        }
        if let Some(pii) = self.pii_only {
            if event.pii_flag != pii {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EventKind, actor: &str, dest: &str, pii: bool, ts: &str) -> AuditEvent {
        let mut e = AuditEvent::new(kind, actor, dest, "summary", pii);
        e.timestamp = ts.to_string();
        e
    }

    #[test]
    fn unfiltered_query_matches_everything() {
        let q = AuditQuery::new();
        let e = ev(
            EventKind::ModelCall,
            "a",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        assert!(q.matches(&e));
    }

    #[test]
    fn kind_filter() {
        let q = AuditQuery::new().with_kind(EventKind::ToolExec);
        let match_ev = ev(
            EventKind::ToolExec,
            "a",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let no_match = ev(
            EventKind::ModelCall,
            "a",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        assert!(q.matches(&match_ev));
        assert!(!q.matches(&no_match));
    }

    #[test]
    fn since_filter_inclusive() {
        let q = AuditQuery::new().with_since("2024-06-01T00:00:00+00:00");
        let before = ev(
            EventKind::MemoryRead,
            "a",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let exact = ev(
            EventKind::MemoryRead,
            "a",
            "d",
            false,
            "2024-06-01T00:00:00+00:00",
        );
        let after = ev(
            EventKind::MemoryRead,
            "a",
            "d",
            false,
            "2025-01-01T00:00:00+00:00",
        );
        assert!(!q.matches(&before));
        assert!(q.matches(&exact));
        assert!(q.matches(&after));
    }

    #[test]
    fn until_filter_inclusive() {
        let q = AuditQuery::new().with_until("2024-06-01T00:00:00+00:00");
        let before = ev(
            EventKind::ChannelSend,
            "a",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let exact = ev(
            EventKind::ChannelSend,
            "a",
            "d",
            false,
            "2024-06-01T00:00:00+00:00",
        );
        let after = ev(
            EventKind::ChannelSend,
            "a",
            "d",
            false,
            "2025-01-01T00:00:00+00:00",
        );
        assert!(q.matches(&before));
        assert!(q.matches(&exact));
        assert!(!q.matches(&after));
    }

    #[test]
    fn destination_filter() {
        let q = AuditQuery::new().with_destination("gpt-4o");
        let yes = ev(
            EventKind::ModelCall,
            "a",
            "gpt-4o",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let no = ev(
            EventKind::ModelCall,
            "a",
            "claude",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        assert!(q.matches(&yes));
        assert!(!q.matches(&no));
    }

    #[test]
    fn actor_filter() {
        let q = AuditQuery::new().with_actor("agent-1");
        let yes = ev(
            EventKind::ToolExec,
            "agent-1",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let no = ev(
            EventKind::ToolExec,
            "agent-2",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        assert!(q.matches(&yes));
        assert!(!q.matches(&no));
    }

    #[test]
    fn pii_only_true_filter() {
        let q = AuditQuery::new().with_pii_only(true);
        let pii = ev(
            EventKind::MemoryWrite,
            "a",
            "d",
            true,
            "2024-01-01T00:00:00+00:00",
        );
        let no_pii = ev(
            EventKind::MemoryWrite,
            "a",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        assert!(q.matches(&pii));
        assert!(!q.matches(&no_pii));
    }

    #[test]
    fn pii_only_false_filter() {
        let q = AuditQuery::new().with_pii_only(false);
        let pii = ev(
            EventKind::MemoryWrite,
            "a",
            "d",
            true,
            "2024-01-01T00:00:00+00:00",
        );
        let no_pii = ev(
            EventKind::MemoryWrite,
            "a",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        assert!(!q.matches(&pii));
        assert!(q.matches(&no_pii));
    }

    #[test]
    fn combined_filters_are_anded() {
        let q = AuditQuery::new()
            .with_kind(EventKind::ModelCall)
            .with_actor("agent-1")
            .with_pii_only(false);

        let full_match = ev(
            EventKind::ModelCall,
            "agent-1",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let wrong_kind = ev(
            EventKind::ToolExec,
            "agent-1",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let wrong_actor = ev(
            EventKind::ModelCall,
            "agent-2",
            "d",
            false,
            "2024-01-01T00:00:00+00:00",
        );
        let pii_event = ev(
            EventKind::ModelCall,
            "agent-1",
            "d",
            true,
            "2024-01-01T00:00:00+00:00",
        );

        assert!(q.matches(&full_match));
        assert!(!q.matches(&wrong_kind));
        assert!(!q.matches(&wrong_actor));
        assert!(!q.matches(&pii_event));
    }
}
