//! `ForgettingEngine` — orchestrates the full forgetting workflow.
//!
//! The engine ties together:
//!
//! - [`super::policy::RetentionPolicy`] — decides which entries are eligible
//! - [`super::gate::ApprovalGate`] — confirmation before destructive operations
//! - [`super::audit::AuditLog`] — immutable record of every action
//!
//! # Usage
//!
//! ```rust,no_run
//! # use std::sync::Arc;
//! # use pares_agens_core::memory::{
//! #     store::InMemoryStore,
//! #     forgetting::{
//! #         policy::{RetentionPolicy, RetentionRule},
//! #         engine::ForgettingEngine,
//! #         gate::AutoApproveGate,
//! #     },
//! # };
//! # use pares_agens_core::memory::entry::MemoryCategory;
//! # #[tokio::main] async fn main() {
//! let store = Arc::new(InMemoryStore::new());
//! let engine = ForgettingEngine::new(store, 24);
//!
//! let mut policy = RetentionPolicy::new();
//! policy.set_rule(MemoryCategory::Conversation, RetentionRule::expire_after(30));
//!
//! // Dry-run — inspect what would be purged
//! let report = engine.dry_run(&policy).await.unwrap();
//! println!("would purge {} entries", report.total_affected);
//!
//! // Execute with auto-approval
//! let result = engine.execute(report, &AutoApproveGate).await.unwrap();
//! println!("soft-deleted: {:?}", result.soft_deleted_ids);
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::memory::{
    entry::{MemoryCategory, MemoryEntry},
    store::MemoryStore,
    Error,
};

use super::{
    audit::{AuditAction, AuditEntry, AuditLog},
    gate::ApprovalGate,
    policy::RetentionPolicy,
};

// ---------------------------------------------------------------------------
// ImpactEntry
// ---------------------------------------------------------------------------

/// A single entry in a [`PurgeReport`] — one memory eligible for removal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactEntry {
    /// ID of the affected memory.
    pub memory_id: String,
    /// Category of the memory.
    pub category: MemoryCategory,
    /// Age of the entry in fractional days.
    pub age_days: f64,
    /// Human-readable reason this entry is eligible (`"expired"`, `"over_count"`).
    pub reason: String,
}

// ---------------------------------------------------------------------------
// PurgeReport
// ---------------------------------------------------------------------------

/// Result of a dry-run pass — lists every entry that would be affected.
///
/// Pass this to [`ForgettingEngine::execute`] (with an [`ApprovalGate`]) to
/// perform the actual purge, or discard it to abort.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeReport {
    /// Individual entries eligible for purge.
    pub entries: Vec<ImpactEntry>,
    /// `entries.len()` — exposed as a convenience field.
    pub total_affected: usize,
    /// Always `true` for reports from [`ForgettingEngine::dry_run`];
    /// set to `false` once [`ForgettingEngine::execute`] starts processing.
    pub is_dry_run: bool,
}

impl PurgeReport {
    /// `true` when no entries would be affected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Summarise the report as a human-readable string suitable for an
    /// approval prompt.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "No memories are eligible for purge.".to_owned();
        }
        let mut by_cat: HashMap<String, usize> = HashMap::new();
        for e in &self.entries {
            *by_cat.entry(e.category.as_str().to_owned()).or_insert(0) += 1;
        }
        let mut lines = vec![format!(
            "Purge impact: {} entries affected",
            self.total_affected
        )];
        let mut cats: Vec<_> = by_cat.into_iter().collect();
        cats.sort_by_key(|(k, _)| k.clone());
        for (cat, count) in cats {
            lines.push(format!("  [{cat}] {count}"));
        }
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// PurgeResult
// ---------------------------------------------------------------------------

/// Outcome of a [`ForgettingEngine::execute`] call.
#[derive(Debug, Default)]
pub struct PurgeResult {
    /// IDs of entries that were soft-deleted (recoverable within the window).
    pub soft_deleted_ids: Vec<String>,
    /// IDs of entries that were previously soft-deleted and are now hard-purged.
    pub hard_purged_ids: Vec<String>,
    /// Audit entries appended to the log.
    pub audit_entries: Vec<AuditEntry>,
}

// ---------------------------------------------------------------------------
// SoftDeletedRecord — internal
// ---------------------------------------------------------------------------

/// An entry in the soft-delete holding area.
struct SoftDeletedRecord {
    entry: MemoryEntry,
    /// RFC3339 timestamp after which recovery is no longer possible.
    expires_at: DateTime<Utc>,
    reason: String,
}

// ---------------------------------------------------------------------------
// ForgettingEngine
// ---------------------------------------------------------------------------

/// Orchestrates controlled forgetting across a [`MemoryStore`].
///
/// Thread-safe: wraps all mutable state in `RwLock`s.
pub struct ForgettingEngine {
    store: Arc<dyn MemoryStore>,
    /// Soft-deleted entries indexed by memory ID.
    soft_deleted: RwLock<HashMap<String, SoftDeletedRecord>>,
    /// Immutable audit log of every forgetting action.
    pub audit_log: AuditLog,
    /// How many hours a soft-deleted entry can be recovered before it expires.
    recovery_window_hours: u64,
}

impl ForgettingEngine {
    /// Create a new engine wrapping `store`.
    ///
    /// `recovery_window_hours` — how long soft-deleted entries remain recoverable
    /// before they are eligible for hard purge on the next scheduled pass.
    pub fn new(store: Arc<dyn MemoryStore>, recovery_window_hours: u64) -> Self {
        Self {
            store,
            soft_deleted: RwLock::new(HashMap::new()),
            audit_log: AuditLog::new(),
            recovery_window_hours,
        }
    }

    // ── Dry-run ─────────────────────────────────────────────────────────────

    /// Compute a dry-run [`PurgeReport`] without modifying any data.
    ///
    /// Applies `policy` to every entry in the store and returns the full list
    /// of entries that would be affected.  Entries currently in the
    /// soft-delete holding area are **not** re-evaluated (they are either
    /// awaiting recovery or will be hard-purged by the next scheduled pass).
    ///
    /// # Errors
    /// Propagates store errors.
    pub async fn dry_run(&self, policy: &RetentionPolicy) -> Result<PurgeReport, Error> {
        let all = self.store.all().await?;
        let now = Utc::now();
        let soft_deleted_guard = self.soft_deleted.read().await;

        // Group live entries by category for count-based rules
        let mut by_category: HashMap<MemoryCategory, Vec<&MemoryEntry>> = HashMap::new();
        for entry in &all {
            if soft_deleted_guard.contains_key(&entry.id) {
                continue; // already in soft-delete area
            }
            by_category
                .entry(entry.category.clone())
                .or_default()
                .push(entry);
        }

        let mut eligible: Vec<ImpactEntry> = Vec::new();

        for (category, mut entries) in by_category {
            let rule = policy.rule_for(&category);

            // Age-based: mark entries older than max_age_days
            if let Some(max_age) = rule.max_age_days {
                let cutoff = now - chrono::TimeDelta::days(max_age as i64);
                for e in &entries {
                    if let Ok(created) = DateTime::parse_from_rfc3339(&e.created_at) {
                        let age_days =
                            (now - created.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
                        if created.with_timezone(&Utc) < cutoff {
                            eligible.push(ImpactEntry {
                                memory_id: e.id.clone(),
                                category: category.clone(),
                                age_days,
                                reason: format!("expired: age {age_days:.1}d > max {max_age}d"),
                            });
                        }
                    } else {
                        warn!(id = %e.id, "could not parse created_at; skipping age check");
                    }
                }
            }

            // Count-based: mark oldest excess entries
            if let Some(max_count) = rule.max_count {
                if entries.len() > max_count {
                    // Sort by created_at ascending so the oldest are at the front
                    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
                    let excess = entries.len() - max_count;
                    for e in entries.iter().take(excess) {
                        // Avoid adding duplicates (entry may already be age-eligible)
                        let already_listed = eligible.iter().any(|x| x.memory_id == e.id);
                        if !already_listed {
                            let age_days = DateTime::parse_from_rfc3339(&e.created_at)
                                .map(|dt| {
                                    (now - dt.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0
                                })
                                .unwrap_or(0.0);
                            eligible.push(ImpactEntry {
                                memory_id: e.id.clone(),
                                category: category.clone(),
                                age_days,
                                reason: format!(
                                    "over_count: {}/{max_count} in category",
                                    entries.len()
                                ),
                            });
                        }
                    }
                }
            }
        }

        let total = eligible.len();
        debug!(total, "dry_run complete");

        Ok(PurgeReport {
            entries: eligible,
            total_affected: total,
            is_dry_run: true,
        })
    }

    // ── Execute ─────────────────────────────────────────────────────────────

    /// Execute a purge based on a previously computed [`PurgeReport`].
    ///
    /// The `gate` is called first; if it returns `false` the purge is aborted
    /// and an empty [`PurgeResult`] is returned (no mutations, no audit entries).
    ///
    /// Affected entries are **soft-deleted**: moved to the internal holding
    /// area.  They can be recovered via [`restore`][Self::restore] until the
    /// recovery window expires.
    ///
    /// # Errors
    /// Propagates store errors from reading the entry list.
    pub async fn execute(
        &self,
        report: PurgeReport,
        gate: &dyn ApprovalGate,
    ) -> Result<PurgeResult, Error> {
        if !gate.approve(&report) {
            info!("purge aborted: approval gate denied");
            return Ok(PurgeResult::default());
        }

        if report.is_empty() {
            return Ok(PurgeResult::default());
        }

        let all = self.store.all().await?;
        let by_id: HashMap<String, MemoryEntry> =
            all.into_iter().map(|e| (e.id.clone(), e)).collect();

        let now = Utc::now();
        let expires_at = now + chrono::TimeDelta::hours(self.recovery_window_hours as i64);

        let mut soft_deleted_guard = self.soft_deleted.write().await;
        let mut result = PurgeResult::default();

        for impact in report.entries {
            if let Some(entry) = by_id.get(&impact.memory_id) {
                soft_deleted_guard.insert(
                    impact.memory_id.clone(),
                    SoftDeletedRecord {
                        entry: entry.clone(),
                        expires_at,
                        reason: impact.reason.clone(),
                    },
                );
                let audit = AuditEntry::new(
                    &impact.memory_id,
                    entry.category.clone(),
                    AuditAction::SoftDeleted,
                    &impact.reason,
                    false,
                );
                self.audit_log.append(audit.clone()).await;
                result.soft_deleted_ids.push(impact.memory_id);
                result.audit_entries.push(audit);
            }
        }

        info!(
            soft_deleted = result.soft_deleted_ids.len(),
            "purge execute complete"
        );
        Ok(result)
    }

    // ── Restore ─────────────────────────────────────────────────────────────

    /// Restore a soft-deleted memory entry by ID.
    ///
    /// Returns the restored [`MemoryEntry`] if it was in the soft-delete
    /// holding area and its recovery window has not yet expired.  Returns
    /// `Err(Error::Store(...))` if the ID is unknown or the window has lapsed.
    pub async fn restore(&self, memory_id: &str) -> Result<MemoryEntry, Error> {
        let mut guard = self.soft_deleted.write().await;
        let record = guard.remove(memory_id).ok_or_else(|| {
            Error::Store(format!(
                "restore failed: '{memory_id}' not in soft-delete area"
            ))
        })?;

        let now = Utc::now();
        if now > record.expires_at {
            // Put it back and report expired
            guard.insert(memory_id.to_owned(), record);
            return Err(Error::Store(format!(
                "restore failed: recovery window expired for '{memory_id}'"
            )));
        }

        self.audit_log
            .append(AuditEntry::new(
                memory_id,
                record.entry.category.clone(),
                AuditAction::Restored,
                "user-initiated restore",
                false,
            ))
            .await;

        info!(memory_id, "memory restored from soft-delete");
        Ok(record.entry)
    }

    // ── Soft-delete inspection ───────────────────────────────────────────────

    /// Return the IDs of all entries currently in the soft-delete holding area.
    pub async fn soft_deleted_ids(&self) -> Vec<String> {
        self.soft_deleted.read().await.keys().cloned().collect()
    }

    /// `true` when `memory_id` is currently soft-deleted.
    pub async fn is_soft_deleted(&self, memory_id: &str) -> bool {
        self.soft_deleted.read().await.contains_key(memory_id)
    }

    // ── Scheduled purge ──────────────────────────────────────────────────────

    /// Run a full purge cycle: dry-run → approve (auto) → execute.
    ///
    /// Also hard-purges any soft-deleted entries whose recovery window has
    /// expired.
    ///
    /// This method is designed to be called from a scheduler task:
    ///
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use pares_agens_core::memory::{
    /// #     store::InMemoryStore,
    /// #     forgetting::{
    /// #         engine::ForgettingEngine,
    /// #         policy::RetentionPolicy,
    /// #     },
    /// # };
    /// # async fn example() {
    /// let engine = Arc::new(ForgettingEngine::new(Arc::new(InMemoryStore::new()), 24));
    /// let policy = RetentionPolicy::default_production();
    /// tokio::spawn({
    ///     let engine = Arc::clone(&engine);
    ///     async move {
    ///         loop {
    ///             tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    ///             let _ = engine.run_scheduled_purge(&policy).await;
    ///         }
    ///     }
    /// });
    /// # }
    /// ```
    ///
    /// # Errors
    /// Propagates store errors from [`dry_run`][Self::dry_run].
    pub async fn run_scheduled_purge(
        &self,
        policy: &RetentionPolicy,
    ) -> Result<PurgeResult, Error> {
        info!("scheduled purge: starting");

        // 1. Hard-purge expired soft-deleted entries
        let hard_purged = self.hard_purge_expired().await;

        // 2. Dry-run the live store
        let report = self.dry_run(policy).await?;

        // 3. Auto-approve and execute
        let mut result = self.execute(report, &super::gate::AutoApproveGate).await?;
        result.hard_purged_ids.extend(hard_purged);

        // 4. Append a summary audit entry using the aggregate constructor
        self.audit_log
            .append(AuditEntry::aggregate(
                AuditAction::ScheduledPurgeRan,
                format!(
                    "soft_deleted={}, hard_purged={}",
                    result.soft_deleted_ids.len(),
                    result.hard_purged_ids.len()
                ),
            ))
            .await;

        info!(
            soft_deleted = result.soft_deleted_ids.len(),
            hard_purged = result.hard_purged_ids.len(),
            "scheduled purge complete"
        );
        Ok(result)
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Hard-purge all soft-deleted entries whose recovery window has expired.
    /// Returns the IDs of entries that were permanently removed.
    async fn hard_purge_expired(&self) -> Vec<String> {
        let now = Utc::now();
        let mut guard = self.soft_deleted.write().await;
        let expired: Vec<(String, MemoryCategory, String)> = guard
            .iter()
            .filter(|(_, r)| now > r.expires_at)
            .map(|(id, r)| (id.clone(), r.entry.category.clone(), r.reason.clone()))
            .collect();

        let ids: Vec<String> = expired.iter().map(|(id, _, _)| id.clone()).collect();
        for id in &ids {
            guard.remove(id);
        }
        drop(guard);

        // Append audit entries after releasing the lock
        for (id, category, reason) in &expired {
            self.audit_log
                .append(AuditEntry::new(
                    id,
                    category.clone(),
                    AuditAction::HardPurged,
                    format!(
                        "recovery window expired ({}h); original reason: {reason}",
                        self.recovery_window_hours
                    ),
                    false,
                ))
                .await;
        }

        ids
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{
        entry::{MemoryCategory, MemoryEntry},
        forgetting::gate::{AutoApproveGate, DenyAllGate},
        store::InMemoryStore,
    };
    use chrono::{TimeDelta, Utc};

    fn old_entry(id: &str, days_ago: i64, category: MemoryCategory) -> MemoryEntry {
        let created_at = (Utc::now() - TimeDelta::days(days_ago)).to_rfc3339();
        MemoryEntry {
            id: id.to_owned(),
            content: format!("content of {id}"),
            category,
            tags: vec![],
            embedding: vec![0.1, 0.2],
            score: 0.0,
            created_at,
        }
    }

    async fn engine_with_entries(entries: Vec<MemoryEntry>) -> ForgettingEngine {
        let store = Arc::new(InMemoryStore::new());
        for e in entries {
            store.insert(e).await.unwrap();
        }
        ForgettingEngine::new(store, 24)
    }

    // ── dry_run ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn dry_run_empty_store_returns_empty_report() {
        let engine = engine_with_entries(vec![]).await;
        let policy = RetentionPolicy::new();
        let report = engine.dry_run(&policy).await.unwrap();
        assert!(report.is_empty());
        assert_eq!(report.total_affected, 0);
        assert!(report.is_dry_run);
    }

    #[tokio::test]
    async fn dry_run_keep_forever_policy_returns_empty_report() {
        let engine =
            engine_with_entries(vec![old_entry("a", 100, MemoryCategory::Conversation)]).await;
        let report = engine.dry_run(&RetentionPolicy::new()).await.unwrap();
        assert!(report.is_empty());
    }

    #[tokio::test]
    async fn dry_run_flags_expired_entry() {
        let engine = engine_with_entries(vec![
            old_entry("a", 40, MemoryCategory::Conversation), // 40 days old, limit 30
        ])
        .await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert_eq!(report.total_affected, 1);
        assert_eq!(report.entries[0].memory_id, "a");
        assert!(report.entries[0].reason.contains("expired"));
    }

    #[tokio::test]
    async fn dry_run_flags_over_count_entries() {
        let entries = (0..5_u8)
            .map(|i| old_entry(&format!("e{i}"), i as i64 + 1, MemoryCategory::CodePattern))
            .collect();
        let engine = engine_with_entries(entries).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::CodePattern,
            super::super::policy::RetentionRule::limit_count(3),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert_eq!(report.total_affected, 2); // 5 - 3 = 2
    }

    #[tokio::test]
    async fn dry_run_does_not_mutate_store() {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(old_entry("x", 100, MemoryCategory::Conversation))
            .await
            .unwrap();
        let engine = ForgettingEngine::new(Arc::clone(&store) as _, 24);
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(7),
        );
        engine.dry_run(&policy).await.unwrap();
        // Store still has 1 entry
        assert_eq!(store.all().await.unwrap().len(), 1);
    }

    // ── execute ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn execute_denied_by_gate_returns_empty_result() {
        let engine =
            engine_with_entries(vec![old_entry("a", 60, MemoryCategory::Conversation)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        let result = engine.execute(report, &DenyAllGate).await.unwrap();
        assert!(result.soft_deleted_ids.is_empty());
        assert!(engine.audit_log.is_empty().await);
    }

    #[tokio::test]
    async fn execute_soft_deletes_eligible_entries() {
        let engine = engine_with_entries(vec![
            old_entry("a", 60, MemoryCategory::Conversation),
            old_entry("b", 5, MemoryCategory::Conversation), // fresh, not expired
        ])
        .await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        let result = engine.execute(report, &AutoApproveGate).await.unwrap();
        assert_eq!(result.soft_deleted_ids, vec!["a"]);
        assert!(engine.is_soft_deleted("a").await);
        assert!(!engine.is_soft_deleted("b").await);
    }

    #[tokio::test]
    async fn execute_appends_audit_entries() {
        let engine =
            engine_with_entries(vec![old_entry("a", 60, MemoryCategory::Conversation)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        engine.execute(report, &AutoApproveGate).await.unwrap();
        assert_eq!(engine.audit_log.len().await, 1);
        let entries = engine.audit_log.entries().await;
        assert_eq!(entries[0].action, AuditAction::SoftDeleted);
        assert_eq!(entries[0].memory_id, "a");
    }

    // ── restore ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn restore_recovers_soft_deleted_entry() {
        let engine =
            engine_with_entries(vec![old_entry("a", 60, MemoryCategory::Conversation)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        engine.execute(report, &AutoApproveGate).await.unwrap();

        let restored = engine.restore("a").await.unwrap();
        assert_eq!(restored.id, "a");
        assert!(!engine.is_soft_deleted("a").await);
    }

    #[tokio::test]
    async fn restore_unknown_id_returns_error() {
        let engine = engine_with_entries(vec![]).await;
        let err = engine.restore("nonexistent").await.unwrap_err();
        assert!(matches!(err, Error::Store(_)));
    }

    // ── purge report summary ─────────────────────────────────────────────────

    #[test]
    fn purge_report_summary_empty() {
        let report = PurgeReport {
            entries: vec![],
            total_affected: 0,
            is_dry_run: true,
        };
        assert_eq!(report.summary(), "No memories are eligible for purge.");
    }

    #[test]
    fn purge_report_summary_with_entries() {
        let report = PurgeReport {
            entries: vec![ImpactEntry {
                memory_id: "x".into(),
                category: MemoryCategory::Conversation,
                age_days: 35.0,
                reason: "expired".into(),
            }],
            total_affected: 1,
            is_dry_run: true,
        };
        let s = report.summary();
        assert!(s.contains("1 entries affected"));
        assert!(s.contains("conversation"));
    }

    /// Kills mutant: replace += with *= in PurgeReport::summary (line 110)
    /// With *=, count for a category with 2 entries would stay 0 (0*1=0) or
    /// go wrong. This test asserts the exact count for multi-entry categories.
    #[test]
    fn purge_report_summary_counts_multiple_entries_per_category() {
        let report = PurgeReport {
            entries: vec![
                ImpactEntry {
                    memory_id: "a".into(),
                    category: MemoryCategory::Conversation,
                    age_days: 40.0,
                    reason: "expired".into(),
                },
                ImpactEntry {
                    memory_id: "b".into(),
                    category: MemoryCategory::Conversation,
                    age_days: 50.0,
                    reason: "expired".into(),
                },
                ImpactEntry {
                    memory_id: "c".into(),
                    category: MemoryCategory::CodePattern,
                    age_days: 60.0,
                    reason: "excess".into(),
                },
            ],
            total_affected: 3,
            is_dry_run: true,
        };
        let s = report.summary();
        // Must contain "[conversation] 2" — with *= it would be 0 or 1
        assert!(s.contains("[conversation] 2"), "summary was: {s}");
        assert!(s.contains("[code-pattern] 1"), "summary was: {s}");
    }

    /// Kills mutant: replace / with % or * in age_days calc (line 222)
    /// 86400 seconds = 1.0 day. With % it would be 0.0, with * it would be
    /// enormous.
    #[tokio::test]
    async fn dry_run_age_days_is_correct() {
        // Entry is exactly 10 days old, with expire_after(5)
        let engine =
            engine_with_entries(vec![old_entry("age10", 10, MemoryCategory::Conversation)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(5),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert_eq!(report.entries.len(), 1);
        let age = report.entries[0].age_days;
        // age_days should be ~10.0 (within float tolerance)
        // With / replaced by %, age would be 0.0; with *, it'd be ~7.46e10
        assert!(
            (9.9..=10.1).contains(&age),
            "expected age ~10.0 but got {age}"
        );
    }

    /// Kills mutant: replace < with <= on cutoff comparison (line 223)
    /// An entry at exactly the cutoff boundary (created_at == cutoff) should
    /// NOT be flagged with `<`, but WOULD be with `<=`.
    /// We set the entry 1 second younger than the cutoff to reliably
    /// distinguish `<` from `<=`.
    #[tokio::test]
    async fn dry_run_entry_at_cutoff_boundary_not_expired() {
        // max_age_days=30, entry is 30 days minus 1 second old
        // cutoff = now - 30 days. entry created = now - 30 days + 1 second > cutoff
        // With `<`: created > cutoff → false → not expired ✓
        // With `<=`: created > cutoff → false → not expired (same)
        // But we also test the complementary case: exactly 30 days + 1 second
        let store = Arc::new(InMemoryStore::new());
        // Entry just barely under the limit
        let barely_under = (Utc::now() - TimeDelta::days(30) + TimeDelta::seconds(60)).to_rfc3339();
        store
            .insert(MemoryEntry {
                id: "barely_under".to_owned(),
                content: "near boundary".to_owned(),
                category: MemoryCategory::Conversation,
                tags: vec![],
                embedding: vec![0.1],
                score: 0.0,
                created_at: barely_under,
            })
            .await
            .unwrap();
        // Entry just barely over the limit
        let barely_over = (Utc::now() - TimeDelta::days(30) - TimeDelta::seconds(60)).to_rfc3339();
        store
            .insert(MemoryEntry {
                id: "barely_over".to_owned(),
                content: "past boundary".to_owned(),
                category: MemoryCategory::Conversation,
                tags: vec![],
                embedding: vec![0.1],
                score: 0.0,
                created_at: barely_over,
            })
            .await
            .unwrap();
        let engine = ForgettingEngine::new(store, 24);
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        // Only the "barely_over" entry should be expired
        assert_eq!(report.total_affected, 1, "report: {:?}", report.entries);
        assert_eq!(report.entries[0].memory_id, "barely_over");
    }

    /// Companion: entry clearly past cutoff IS expired (31 days > 30 limit)
    #[tokio::test]
    async fn dry_run_entry_past_cutoff_is_expired() {
        let engine =
            engine_with_entries(vec![old_entry("old", 31, MemoryCategory::Conversation)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert_eq!(report.total_affected, 1);
        assert_eq!(report.entries[0].memory_id, "old");
    }

    /// Kills mutant: replace > with >= in count check (line 239)
    /// When entries.len() == max_count, NO entries should be removed.
    #[tokio::test]
    async fn dry_run_count_at_exact_limit_has_no_excess() {
        // 3 entries with max_count=3 → len == max → no excess
        let entries = (0..3_u8)
            .map(|i| old_entry(&format!("e{i}"), i as i64 + 1, MemoryCategory::CodePattern))
            .collect();
        let engine = engine_with_entries(entries).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::CodePattern,
            super::super::policy::RetentionRule::limit_count(3),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert!(
            report.is_empty(),
            "entries at exact count limit should not be purged, got {} affected",
            report.total_affected
        );
    }

    /// Kills mutant: replace ForgettingEngine::soft_deleted_ids -> Vec<String> with vec![]
    #[tokio::test]
    async fn soft_deleted_ids_returns_ids_after_execute() {
        let engine =
            engine_with_entries(vec![old_entry("a", 60, MemoryCategory::Conversation)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        engine.execute(report, &AutoApproveGate).await.unwrap();
        let ids = engine.soft_deleted_ids().await;
        assert_eq!(ids, vec!["a"]);
    }

    /// Kills mutant: replace > with == or >= in restore expiry check (line 360)
    /// After the recovery window expires, restore should fail.
    #[tokio::test]
    async fn restore_fails_after_recovery_window_expires() {
        // Use recovery_window_hours=0 so it expires immediately
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(old_entry("a", 60, MemoryCategory::Conversation))
            .await
            .unwrap();
        let engine = ForgettingEngine::new(store, 0); // 0-hour recovery window

        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        engine.execute(report, &AutoApproveGate).await.unwrap();

        // Small sleep to ensure now > expires_at (recovery window is 0 hours)
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let err = engine.restore("a").await.unwrap_err();
        assert!(
            format!("{err:?}").contains("expired"),
            "expected recovery expired error, got: {err:?}"
        );
    }

    /// Kills mutant: replace / with % or * in count-based age_days calc (line 249)
    /// Verifies age_days is populated correctly for count-based excess entries.
    #[tokio::test]
    async fn dry_run_count_excess_has_correct_age_days() {
        // 4 entries with max_count=2, oldest are 10 and 8 days old
        let entries = vec![
            old_entry("e1", 10, MemoryCategory::CodePattern),
            old_entry("e2", 8, MemoryCategory::CodePattern),
            old_entry("e3", 3, MemoryCategory::CodePattern),
            old_entry("e4", 1, MemoryCategory::CodePattern),
        ];
        let engine = engine_with_entries(entries).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::CodePattern,
            super::super::policy::RetentionRule::limit_count(2),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert_eq!(report.total_affected, 2);
        // The excess entries should have age_days approximately correct
        for entry in &report.entries {
            // With / replaced by %, age would be 0.0 or tiny; with *, it'd be enormous
            assert!(
                entry.age_days > 1.0 && entry.age_days < 15.0,
                "age_days {} is outside expected range for count-based excess",
                entry.age_days
            );
        }
    }

    // ── run_scheduled_purge ────────────────────────────────────────────────

    /// Kills mutant: replace run_scheduled_purge -> Ok(Default::default())
    #[tokio::test]
    async fn scheduled_purge_soft_deletes_expired_entries() {
        let engine =
            engine_with_entries(vec![old_entry("a", 60, MemoryCategory::Conversation)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let result = engine.run_scheduled_purge(&policy).await.unwrap();
        // With Default::default(), these would be empty
        assert_eq!(result.soft_deleted_ids, vec!["a"]);
    }

    /// Kills mutant: replace run_scheduled_purge -> Ok(Default::default())
    /// Also verifies audit entry is appended for ScheduledPurgeRan
    #[tokio::test]
    async fn scheduled_purge_appends_audit_summary() {
        let engine =
            engine_with_entries(vec![old_entry("b", 45, MemoryCategory::CodePattern)]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::CodePattern,
            super::super::policy::RetentionRule::expire_after(30),
        );
        engine.run_scheduled_purge(&policy).await.unwrap();
        let entries = engine.audit_log.entries().await;
        // At least one SoftDeleted + one ScheduledPurgeRan
        let scheduled: Vec<_> = entries
            .iter()
            .filter(|e| e.action == AuditAction::ScheduledPurgeRan)
            .collect();
        assert_eq!(
            scheduled.len(),
            1,
            "expected exactly one ScheduledPurgeRan audit entry"
        );
        assert!(scheduled[0].reason.contains("soft_deleted=1"));
    }

    // ── hard_purge_expired ─────────────────────────────────────────────────

    /// Kills mutant: replace hard_purge_expired -> vec![]
    #[tokio::test]
    async fn hard_purge_expired_removes_past_recovery_entries() {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(old_entry("hp1", 60, MemoryCategory::Conversation))
            .await
            .unwrap();
        // Use 0-hour recovery window so entries expire immediately
        let engine = ForgettingEngine::new(store, 0);

        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        // First pass: soft-delete the entry
        let report = engine.dry_run(&policy).await.unwrap();
        engine.execute(report, &AutoApproveGate).await.unwrap();

        // Small wait for recovery window (0h) to expire
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Now hard_purge_expired should return the ID
        let purged = engine.hard_purge_expired().await;
        assert_eq!(purged, vec!["hp1"]);
    }

    /// Verifies scheduled purge integrates hard_purge into its result
    #[tokio::test]
    async fn scheduled_purge_includes_hard_purged_ids() {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(old_entry("first", 60, MemoryCategory::Conversation))
            .await
            .unwrap();
        let engine = ForgettingEngine::new(store.clone(), 0);

        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );

        // First scheduled purge: soft-deletes "first"
        let r1 = engine.run_scheduled_purge(&policy).await.unwrap();
        assert!(r1.soft_deleted_ids.contains(&"first".to_owned()));

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Add another entry for the second pass to work on
        store
            .insert(old_entry("second", 60, MemoryCategory::Conversation))
            .await
            .unwrap();

        // Second scheduled purge: hard-purges "first" + soft-deletes "second"
        let r2 = engine.run_scheduled_purge(&policy).await.unwrap();
        assert!(
            r2.hard_purged_ids.contains(&"first".to_owned()),
            "expected 'first' in hard_purged_ids, got {:?}",
            r2.hard_purged_ids
        );
        assert!(r2.soft_deleted_ids.contains(&"second".to_owned()));
    }

    // ── boundary condition tests (mutation gap coverage) ─────────────────────

    /// Kills mutant: replace < with <= at line 223 (entry exactly at cutoff boundary)
    /// NOTE: This mutant is effectively equivalent because the cutoff is computed
    /// from `now` inside dry_run, making `created == cutoff` unreachable in practice.
    /// An entry created slightly AFTER the cutoff (1 second younger) should NOT be purged.
    /// An entry created slightly BEFORE the cutoff (1 second older) SHOULD be purged.
    #[tokio::test]
    async fn dry_run_entry_just_past_cutoff_is_purged() {
        // Entry is 30 days + 2 seconds old, cutoff = now - 30 days
        // So created < cutoff → true → purged
        let created_at = (Utc::now() - TimeDelta::days(30) - TimeDelta::seconds(2)).to_rfc3339();
        let entry = MemoryEntry {
            id: "just_past".to_owned(),
            content: "just past cutoff".to_owned(),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: vec![0.1, 0.2],
            score: 0.0,
            created_at,
        };
        let engine = engine_with_entries(vec![entry]).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Conversation,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert_eq!(
            report.total_affected, 1,
            "entry just past cutoff should be purged"
        );
    }

    /// Kills mutant: replace > with >= at line 239 (entries.len() == max_count)
    /// When the count of entries equals max_count exactly, no entries should be
    /// purged. The condition is `len > max_count`, not `>=`.
    #[tokio::test]
    async fn dry_run_entries_at_exact_max_count_not_purged() {
        // Create exactly 3 entries with max_count=3
        let entries = (0..3_u8)
            .map(|i| old_entry(&format!("e{i}"), i as i64 + 1, MemoryCategory::CodePattern))
            .collect();
        let engine = engine_with_entries(entries).await;
        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::CodePattern,
            super::super::policy::RetentionRule::limit_count(3),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        assert_eq!(
            report.total_affected, 0,
            "exactly max_count entries should not trigger purge (strict greater-than)"
        );
    }

    /// hard_purge_expired appends HardPurged audit entries
    #[tokio::test]
    async fn hard_purge_expired_creates_audit_entries() {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(old_entry("audit_hp", 60, MemoryCategory::Decision))
            .await
            .unwrap();
        let engine = ForgettingEngine::new(store, 0);

        let mut policy = RetentionPolicy::new();
        policy.set_rule(
            MemoryCategory::Decision,
            super::super::policy::RetentionRule::expire_after(30),
        );
        let report = engine.dry_run(&policy).await.unwrap();
        engine.execute(report, &AutoApproveGate).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        engine.hard_purge_expired().await;

        let entries = engine
            .audit_log
            .entries_by_action(AuditAction::HardPurged)
            .await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].memory_id, "audit_hp");
        assert!(entries[0].reason.contains("recovery window expired"));
    }
}
