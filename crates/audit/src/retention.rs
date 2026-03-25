//! Configurable log rotation / retention for the audit log.
//!
//! [`RetentionConfig`] specifies how long audit events should be kept.
//! [`apply_retention`] applies the policy to any [`AuditStore`] implementation,
//! deleting events older than the configured window.
//!
//! # Example
//!
//! ```rust
//! # use std::sync::Arc;
//! # use pares_agens_audit::{
//! #     event::{AuditEvent, EventKind},
//! #     store::InMemoryAuditStore,
//! #     retention::{RetentionConfig, apply_retention},
//! # };
//! # #[tokio::main] async fn main() {
//! let store = Arc::new(InMemoryAuditStore::new());
//!
//! // Keep events for 90 days.
//! let config = RetentionConfig::days(90);
//! apply_retention(store.as_ref(), &config).await;
//! # }
//! ```

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::store::AuditStore;

// ---------------------------------------------------------------------------
// RetentionConfig
// ---------------------------------------------------------------------------

/// Configuration for audit log retention / rotation.
///
/// Events older than the configured window are eligible for removal when
/// [`apply_retention`] is called.  The special value `RetentionConfig::forever()`
/// disables rotation entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Number of days to keep audit events.
    ///
    /// `None` means "keep forever" (no rotation).
    pub retain_days: Option<u32>,
}

impl RetentionConfig {
    /// Retain events for `days` days.
    pub fn days(days: u32) -> Self {
        Self {
            retain_days: Some(days),
        }
    }

    /// Never rotate — keep all events indefinitely.
    pub fn forever() -> Self {
        Self { retain_days: None }
    }

    /// `true` when this policy will never delete events.
    pub fn is_forever(&self) -> bool {
        self.retain_days.is_none()
    }
}

impl Default for RetentionConfig {
    /// Default retention is 365 days.
    fn default() -> Self {
        Self::days(365)
    }
}

// ---------------------------------------------------------------------------
// apply_retention
// ---------------------------------------------------------------------------

/// Delete audit events older than the window defined in `config`.
///
/// Computes the cutoff timestamp as `now - retain_days` and delegates to
/// [`AuditStore::purge_before`].  When `config` is [`RetentionConfig::forever`]
/// this is a no-op.
pub async fn apply_retention(store: &dyn AuditStore, config: &RetentionConfig) {
    let Some(days) = config.retain_days else {
        return;
    };
    let cutoff = Utc::now() - Duration::days(i64::from(days));
    store.purge_before(&cutoff.to_rfc3339()).await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{event::EventKind, store::InMemoryAuditStore, AuditEvent};

    #[test]
    fn forever_is_none() {
        assert!(RetentionConfig::forever().is_forever());
        assert!(RetentionConfig::forever().retain_days.is_none());
    }

    #[test]
    fn days_sets_retain_days() {
        let c = RetentionConfig::days(30);
        assert_eq!(c.retain_days, Some(30));
        assert!(!c.is_forever());
    }

    #[test]
    fn default_is_365_days() {
        let c = RetentionConfig::default();
        assert_eq!(c.retain_days, Some(365));
    }

    #[tokio::test]
    async fn apply_retention_forever_keeps_all() {
        let store = InMemoryAuditStore::new();
        let mut old = AuditEvent::new(EventKind::ToolExec, "a", "d", "s", false);
        old.timestamp = "2000-01-01T00:00:00+00:00".to_string();
        store.append(old).await;

        apply_retention(&store, &RetentionConfig::forever()).await;
        assert_eq!(store.len().await, 1);
    }

    #[tokio::test]
    async fn apply_retention_removes_old_events() {
        let store = InMemoryAuditStore::new();
        let mut old = AuditEvent::new(EventKind::ToolExec, "old-actor", "d", "s", false);
        old.timestamp = "2000-01-01T00:00:00+00:00".to_string();
        let recent = AuditEvent::new(EventKind::ModelCall, "new-actor", "d", "s", false);
        store.append(old).await;
        store.append(recent).await;

        // Retain only 1 day — "2000-01-01" is way older than that.
        apply_retention(&store, &RetentionConfig::days(1)).await;
        assert_eq!(store.len().await, 1);
        assert_eq!(store.all().await[0].actor, "new-actor");
    }
}
