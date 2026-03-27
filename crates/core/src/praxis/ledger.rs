use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pluresdb::{CrdtStore, MemoryStorage, SledStorage, StorageEngine};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::event::Event;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Outcome of a [`Ledger::validate`] call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ValidationStatus {
    /// The action is permitted under current policies.
    Permitted,
    /// The action requires explicit user approval before it may proceed.
    GateRequired,
    /// The action is unconditionally denied by policy.
    Denied,
}

/// Life-cycle state of an approval gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GateStatus {
    /// Gate has been created; waiting for user decision.
    Pending,
    /// User approved the action.
    Approved,
    /// User rejected the action.
    Rejected,
    /// Not applicable — no gate was created for this entry.
    None,
}

/// Context identifying the channel/session that originated a ledger entry.
///
/// Stored alongside each [`LedgerEntry`] so that [`Ledger::check_gates`] and
/// [`LedgerStore::pending_gates_by_context`] can filter pending gates to only
/// those relevant to the current channel or session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LedgerContext {
    /// The channel the action arrived on (e.g. `"telegram"`, `"stdin"`).
    pub channel: Option<String>,
    /// An opaque session identifier that groups related interactions.
    pub session_id: Option<String>,
}

impl LedgerContext {
    /// Construct a context from a channel name.
    pub fn from_channel(channel: impl Into<String>) -> Self {
        Self {
            channel: Some(channel.into()),
            session_id: None,
        }
    }

    /// Construct a context from both channel and session id.
    pub fn new(channel: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            channel: Some(channel.into()),
            session_id: Some(session_id.into()),
        }
    }

    /// Returns `true` if this context matches `other`.
    ///
    /// Matching rules:
    /// - If both `channel` fields are `Some`, they must be equal.
    /// - If both `session_id` fields are `Some`, they must be equal.
    /// - `None` on either side acts as a wildcard for that field.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        let channel_ok = match (&self.channel, &other.channel) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let session_ok = match (&self.session_id, &other.session_id) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        channel_ok && session_ok
    }
}

/// A single row in the `praxis_ledger` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Unique row identifier (UUID v4).
    pub id: String,
    /// Wall-clock time the entry was created.
    pub timestamp: DateTime<Utc>,
    /// The [`Event::kind`] that triggered this entry, or `"manual"` for
    /// entries not tied to an event.
    pub event_type: String,
    /// Short description of the action being logged or gated.
    pub action: String,
    /// Human-readable explanation of why the action was taken / gated.
    pub rationale: String,
    /// Result of policy validation for this action.
    pub validation_status: ValidationStatus,
    /// Current gate state (only meaningful when
    /// `validation_status == GateRequired`).
    pub gate_status: GateStatus,
    /// Optional serialised response payload associated with this entry (e.g.
    /// the raw model response that triggered a log entry).
    pub response: Option<serde_json::Value>,
    /// Channel/session context that originated this entry.  Used to filter
    /// pending gates by context via [`LedgerStore::pending_gates_by_context`].
    pub context: LedgerContext,
}

impl LedgerEntry {
    fn new(
        event_type: impl Into<String>,
        action: impl Into<String>,
        rationale: impl Into<String>,
        validation_status: ValidationStatus,
        gate_status: GateStatus,
        response: Option<serde_json::Value>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            event_type: event_type.into(),
            action: action.into(),
            rationale: rationale.into(),
            validation_status,
            gate_status,
            response,
            context: LedgerContext::default(),
        }
    }

    fn with_context(mut self, context: LedgerContext) -> Self {
        self.context = context;
        self
    }
}

// ---------------------------------------------------------------------------
// LedgerStore trait
// ---------------------------------------------------------------------------

/// Errors produced by [`LedgerStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum LedgerStoreError {
    /// A serialisation/deserialisation failure.
    #[error("serialise error: {0}")]
    Serialise(String),
    /// The underlying storage backend returned an error.
    #[error("store error: {0}")]
    Store(String),
}

/// Persistence backend for [`Ledger`] entries.
///
/// Implementations must be `Send + Sync` so that a `Ledger` can be shared
/// across async tasks.  All operations are `async` to accommodate both
/// in-process stores (which complete immediately) and remote/file stores.
#[async_trait]
pub trait LedgerStore: Send + Sync {
    /// Persist a new [`LedgerEntry`].
    async fn insert(&self, entry: LedgerEntry) -> Result<(), LedgerStoreError>;

    /// Return all stored entries in insertion order (best-effort).
    async fn all(&self) -> Result<Vec<LedgerEntry>, LedgerStoreError>;

    /// Look up a single entry by its [`LedgerEntry::id`].
    async fn get(&self, id: &str) -> Result<Option<LedgerEntry>, LedgerStoreError>;

    /// Overwrite the [`GateStatus`] of an existing entry.
    ///
    /// Used by [`Ledger::resolve_gate`] to approve or reject a pending gate.
    async fn update_gate_status(
        &self,
        id: &str,
        gate_status: GateStatus,
    ) -> Result<(), LedgerStoreError>;

    /// Return all [`GateStatus::Pending`] entries whose [`LedgerEntry::context`]
    /// matches `context`.
    ///
    /// The matching semantics follow [`LedgerContext::matches`]: a `None`
    /// field on either side acts as a wildcard.
    async fn pending_gates_by_context(
        &self,
        context: &LedgerContext,
    ) -> Result<Vec<LedgerEntry>, LedgerStoreError>;
}

// ---------------------------------------------------------------------------
// InMemoryLedgerStore
// ---------------------------------------------------------------------------

/// A [`LedgerStore`] that keeps all entries in an in-process `Vec`.
///
/// Suitable for tests and single-process deployments that do not require
/// durability across restarts.
pub struct InMemoryLedgerStore {
    entries: RwLock<Vec<LedgerEntry>>,
}

impl InMemoryLedgerStore {
    /// Create a new, empty store.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }
}

impl Default for InMemoryLedgerStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LedgerStore for InMemoryLedgerStore {
    async fn insert(&self, entry: LedgerEntry) -> Result<(), LedgerStoreError> {
        self.entries.write().await.push(entry);
        Ok(())
    }

    async fn all(&self) -> Result<Vec<LedgerEntry>, LedgerStoreError> {
        Ok(self.entries.read().await.clone())
    }

    async fn get(&self, id: &str) -> Result<Option<LedgerEntry>, LedgerStoreError> {
        Ok(self
            .entries
            .read()
            .await
            .iter()
            .find(|e| e.id == id)
            .cloned())
    }

    async fn update_gate_status(
        &self,
        id: &str,
        gate_status: GateStatus,
    ) -> Result<(), LedgerStoreError> {
        let mut entries = self.entries.write().await;
        match entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.gate_status = gate_status;
                Ok(())
            }
            None => Err(LedgerStoreError::Store(format!("entry not found: {id}"))),
        }
    }

    async fn pending_gates_by_context(
        &self,
        context: &LedgerContext,
    ) -> Result<Vec<LedgerEntry>, LedgerStoreError> {
        Ok(self
            .entries
            .read()
            .await
            .iter()
            .filter(|e| e.gate_status == GateStatus::Pending && e.context.matches(context))
            .cloned()
            .collect())
    }
}

// ---------------------------------------------------------------------------
// PluresDbLedgerStore
// ---------------------------------------------------------------------------

/// The PluresDB actor ID used for all ledger write operations.
const LEDGER_ACTOR: &str = "pares-agens-ledger";

/// A [`LedgerStore`] backed by a PluresDB [`CrdtStore`].
///
/// Ledger entries are serialised to JSON and stored as node payloads.
/// Uses [`SledStorage`] for on-disk durability when opened via
/// [`PluresDbLedgerStore::open`]; an ephemeral variant is available via
/// [`PluresDbLedgerStore::in_memory`].
pub struct PluresDbLedgerStore {
    store: CrdtStore,
}

impl PluresDbLedgerStore {
    /// Open or create a PluresDB-backed ledger store at `path`.
    ///
    /// # Errors
    /// Returns [`LedgerStoreError::Store`] if the underlying [`SledStorage`]
    /// cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerStoreError> {
        let storage: Arc<dyn StorageEngine> = Arc::new(
            SledStorage::open(path)
                .map_err(|e| LedgerStoreError::Store(format!("open failed: {e}")))?,
        );
        let store = CrdtStore::default().with_persistence(storage);
        Ok(Self { store })
    }

    /// Create an ephemeral in-memory PluresDB ledger store.
    ///
    /// Useful for integration tests that need real [`CrdtStore`] behaviour
    /// without touching the filesystem.
    pub fn in_memory() -> Self {
        let storage: Arc<dyn StorageEngine> = Arc::new(MemoryStorage::default());
        let store = CrdtStore::default().with_persistence(storage);
        Self { store }
    }
}

#[async_trait]
impl LedgerStore for PluresDbLedgerStore {
    async fn insert(&self, entry: LedgerEntry) -> Result<(), LedgerStoreError> {
        let id = entry.id.clone();
        let data =
            serde_json::to_value(&entry).map_err(|e| LedgerStoreError::Serialise(e.to_string()))?;
        self.store.put(id, LEDGER_ACTOR, data);
        Ok(())
    }

    async fn all(&self) -> Result<Vec<LedgerEntry>, LedgerStoreError> {
        let records = self.store.list();
        let mut entries = Vec::with_capacity(records.len());
        for record in records {
            let entry = serde_json::from_value::<LedgerEntry>(record.data)
                .map_err(|e| LedgerStoreError::Serialise(e.to_string()))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    async fn get(&self, id: &str) -> Result<Option<LedgerEntry>, LedgerStoreError> {
        match self.store.get(id) {
            Some(record) => {
                let entry = serde_json::from_value::<LedgerEntry>(record.data)
                    .map_err(|e| LedgerStoreError::Serialise(e.to_string()))?;
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    async fn update_gate_status(
        &self,
        id: &str,
        gate_status: GateStatus,
    ) -> Result<(), LedgerStoreError> {
        match self.store.get(id) {
            None => Err(LedgerStoreError::Store(format!("entry not found: {id}"))),
            Some(record) => {
                let mut entry = serde_json::from_value::<LedgerEntry>(record.data)
                    .map_err(|e| LedgerStoreError::Serialise(e.to_string()))?;
                entry.gate_status = gate_status;
                let data = serde_json::to_value(&entry)
                    .map_err(|e| LedgerStoreError::Serialise(e.to_string()))?;
                self.store.put(id, LEDGER_ACTOR, data);
                Ok(())
            }
        }
    }

    async fn pending_gates_by_context(
        &self,
        context: &LedgerContext,
    ) -> Result<Vec<LedgerEntry>, LedgerStoreError> {
        let records = self.store.list();
        let mut entries = Vec::new();
        for record in records {
            let entry = serde_json::from_value::<LedgerEntry>(record.data)
                .map_err(|e| LedgerStoreError::Serialise(e.to_string()))?;
            if entry.gate_status == GateStatus::Pending && entry.context.matches(context) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// Notification channel abstraction
// ---------------------------------------------------------------------------

/// Abstraction over the active user-notification channel.
///
/// In production this will route to whatever channel (Telegram, stdin, Tauri
/// IPC, …) is currently active.  In tests a no-op or recording
/// implementation can be injected.
pub trait NotificationChannel: Send + Sync {
    /// Notify the user that a gate has been created and requires their
    /// approval.  Returns `Ok(())` on success.
    fn notify_gate(&self, entry: &LedgerEntry) -> Result<(), String>;
}

/// No-op channel used when no channel is configured.
pub struct NoOpChannel;

impl NotificationChannel for NoOpChannel {
    fn notify_gate(&self, entry: &LedgerEntry) -> Result<(), String> {
        tracing::info!(
            gate_id = %entry.id,
            action = %entry.action,
            "praxis: gate pending (no notification channel configured)"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

/// Storage-backed ledger that maps to the `praxis_ledger` PluresDB table.
///
/// The ledger delegates all persistence to a [`LedgerStore`] so that entries
/// survive process restarts when a durable backend (e.g.
/// [`PluresDbLedgerStore`]) is used.  The default constructor wires in an
/// [`InMemoryLedgerStore`] for zero-setup use in tests and embedded scenarios.
///
/// All public procedures are `async` because the underlying store operations
/// may perform I/O.
#[derive(Clone)]
pub struct Ledger {
    store: Arc<dyn LedgerStore>,
    channel: Arc<dyn NotificationChannel>,
    /// Actions that require a gate (checked by [`Ledger::validate`]).
    gated_actions: Arc<Vec<String>>,
    /// Actions that are unconditionally denied.
    denied_actions: Arc<Vec<String>>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new(Arc::new(NoOpChannel))
    }
}

impl Ledger {
    /// Create a new ledger backed by an [`InMemoryLedgerStore`].
    pub fn new(channel: Arc<dyn NotificationChannel>) -> Self {
        Self::with_store(Arc::new(InMemoryLedgerStore::new()), channel)
    }

    /// Create a ledger backed by the supplied [`LedgerStore`].
    ///
    /// Use this to wire in a [`PluresDbLedgerStore`] (or any other durable
    /// backend) for production deployments.
    pub fn with_store(store: Arc<dyn LedgerStore>, channel: Arc<dyn NotificationChannel>) -> Self {
        Self {
            store,
            channel,
            gated_actions: Arc::new(Vec::new()),
            denied_actions: Arc::new(Vec::new()),
        }
    }

    /// Create a ledger with explicit policy lists, backed by an
    /// [`InMemoryLedgerStore`].
    ///
    /// * `gated_actions` — action prefixes/names that trigger a gate.
    /// * `denied_actions` — action prefixes/names that are always denied.
    pub fn with_policies(
        channel: Arc<dyn NotificationChannel>,
        gated_actions: Vec<String>,
        denied_actions: Vec<String>,
    ) -> Self {
        Self {
            store: Arc::new(InMemoryLedgerStore::new()),
            channel,
            gated_actions: Arc::new(gated_actions),
            denied_actions: Arc::new(denied_actions),
        }
    }

    /// Create a ledger with explicit policy lists and a custom store.
    pub fn with_store_and_policies(
        store: Arc<dyn LedgerStore>,
        channel: Arc<dyn NotificationChannel>,
        gated_actions: Vec<String>,
        denied_actions: Vec<String>,
    ) -> Self {
        Self {
            store,
            channel,
            gated_actions: Arc::new(gated_actions),
            denied_actions: Arc::new(denied_actions),
        }
    }

    // -----------------------------------------------------------------------
    // CRUD helpers
    // -----------------------------------------------------------------------

    /// Append an entry and return its id.
    async fn insert(&self, entry: LedgerEntry) -> String {
        let id = entry.id.clone();
        if let Err(e) = self.store.insert(entry).await {
            tracing::error!(error = %e, "praxis::ledger: failed to persist entry");
        }
        id
    }

    /// Return an immutable snapshot of all entries.
    pub async fn all_entries(&self) -> Vec<LedgerEntry> {
        match self.store.all().await {
            Ok(entries) => entries,
            Err(e) => {
                tracing::error!(error = %e, "praxis::ledger: failed to load all entries");
                Vec::new()
            }
        }
    }

    /// Look up an entry by id.
    pub async fn get(&self, id: &str) -> Option<LedgerEntry> {
        match self.store.get(id).await {
            Ok(entry) => entry,
            Err(e) => {
                tracing::error!(error = %e, id, "praxis::ledger: failed to get entry");
                None
            }
        }
    }

    // -----------------------------------------------------------------------
    // Procedures
    // -----------------------------------------------------------------------

    /// `praxis.log` — append an audit entry for a model interaction.
    ///
    /// Every model response should be logged so there is a complete,
    /// immutable audit trail of what the agent did and why.
    ///
    /// Returns the id of the new ledger entry.
    pub async fn log(&self, event: &Event, response: serde_json::Value) -> String {
        let action = format!("model_response:{}", event.kind());
        let rationale = "Audit log of model interaction".to_string();
        let context = Self::context_from_event(event);
        let entry = LedgerEntry::new(
            event.kind(),
            action,
            rationale,
            ValidationStatus::Permitted,
            GateStatus::None,
            Some(response),
        )
        .with_context(context);
        tracing::debug!(id = %entry.id, event_type = %entry.event_type, "praxis::log");
        self.insert(entry).await
    }

    /// `praxis.validate` — check an action against stored policies.
    ///
    /// Returns:
    /// * [`ValidationStatus::Permitted`] — safe to proceed.
    /// * [`ValidationStatus::GateRequired`] — must call [`Ledger::gate`] first.
    /// * [`ValidationStatus::Denied`] — action is forbidden.
    pub fn validate(&self, action: &str) -> ValidationStatus {
        if self
            .denied_actions
            .iter()
            .any(|d| action.starts_with(d.as_str()))
        {
            return ValidationStatus::Denied;
        }
        if self
            .gated_actions
            .iter()
            .any(|g| action.starts_with(g.as_str()))
        {
            return ValidationStatus::GateRequired;
        }
        ValidationStatus::Permitted
    }

    /// `praxis.gate` — create an approval gate for a high-stakes action.
    ///
    /// Appends a [`GateStatus::Pending`] entry to the ledger and notifies
    /// the user via the active channel.  Returns the gate entry id which
    /// callers must pass to [`Ledger::resolve_gate`].
    ///
    /// # Errors
    /// Returns `Err` if the notification channel fails to deliver the alert.
    pub async fn gate(
        &self,
        action: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Result<String, String> {
        self.gate_with_context(action, rationale, LedgerContext::default())
            .await
    }

    /// `praxis.gate` variant that records the originating channel/session.
    ///
    /// Behaves identically to [`Ledger::gate`] but stores `context` on the
    /// entry so that [`Ledger::check_gates`] and
    /// [`LedgerStore::pending_gates_by_context`] can filter by it.
    pub async fn gate_with_context(
        &self,
        action: impl Into<String>,
        rationale: impl Into<String>,
        context: LedgerContext,
    ) -> Result<String, String> {
        let action = action.into();
        let rationale = rationale.into();
        let entry = LedgerEntry::new(
            "gate",
            &action,
            &rationale,
            ValidationStatus::GateRequired,
            GateStatus::Pending,
            None,
        )
        .with_context(context);
        let id = entry.id.clone();
        tracing::info!(gate_id = %id, %action, "praxis::gate: created pending gate");
        self.insert(entry.clone()).await;
        self.channel.notify_gate(&entry)?;
        Ok(id)
    }

    /// Resolve a previously created gate.
    ///
    /// Updates the entry's `gate_status` to `Approved` or `Rejected`.
    /// Returns `Err` if the gate id is not found or the gate is not in
    /// [`GateStatus::Pending`] state.
    pub async fn resolve_gate(&self, gate_id: &str, approved: bool) -> Result<(), String> {
        let entry = self
            .store
            .get(gate_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("gate not found: {gate_id}"))?;

        if entry.gate_status != GateStatus::Pending {
            return Err(format!(
                "gate {gate_id} is not pending (current status: {:?})",
                entry.gate_status
            ));
        }

        let new_status = if approved {
            GateStatus::Approved
        } else {
            GateStatus::Rejected
        };

        self.store
            .update_gate_status(gate_id, new_status)
            .await
            .map_err(|e| e.to_string())?;

        tracing::info!(gate_id, approved, "praxis::resolve_gate: gate resolved");
        Ok(())
    }

    /// `praxis.check_gates` — return all pending gates for the given event context.
    ///
    /// Filters the ledger to entries with [`GateStatus::Pending`] whose
    /// [`LedgerEntry::context`] matches the channel of the supplied event.
    /// A `None` channel field on either side acts as a wildcard.
    pub async fn check_gates(&self, event: &Event) -> Vec<LedgerEntry> {
        let context = Self::context_from_event(event);
        match self.store.pending_gates_by_context(&context).await {
            Ok(gates) => gates,
            Err(e) => {
                tracing::error!(error = %e, "praxis::ledger: failed to query pending gates");
                Vec::new()
            }
        }
    }

    // -----------------------------------------------------------------------
    // Audit export
    // -----------------------------------------------------------------------

    /// Export the full ledger as a JSON array.
    ///
    /// Each element is a serialised [`LedgerEntry`].  The output is suitable
    /// for archiving, compliance audits, or shipping to a remote log store.
    ///
    /// # Pro feature
    ///
    /// This method requires a valid Pro license.  Pass a [`License`] obtained
    /// from a [`LicenseValidator`] (or [`License::pro`] in tests).  Returns
    /// [`LicenseError::FeatureNotAvailable`] on the Free tier and
    /// [`LicenseError::Expired`] when the Pro license has expired.
    ///
    /// [`LicenseValidator`]: crate::license::LicenseValidator
    /// [`License`]: crate::license::License
    pub async fn export_json(
        &self,
        license: &crate::license::License,
    ) -> Result<serde_json::Value, crate::license::LicenseError> {
        license.check_feature(crate::license::Feature::PraxisAuditExport)?;
        let entries = self.all_entries().await;
        Ok(serde_json::to_value(&entries).unwrap_or(serde_json::Value::Array(vec![])))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Extract a [`LedgerContext`] from an [`Event`], using the channel name
    /// when available.
    fn context_from_event(event: &Event) -> LedgerContext {
        match event {
            Event::Message { channel, .. } => LedgerContext::from_channel(channel.clone()),
            _ => LedgerContext::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn msg_event() -> Event {
        Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: "send an email to alice".into(),
        }
    }

    fn ledger_with_policies() -> Ledger {
        Ledger::with_policies(
            Arc::new(NoOpChannel),
            vec!["send_email".into(), "post_public".into()],
            vec!["delete_all".into()],
        )
    }

    // -----------------------------------------------------------------------
    // Notification channel spy
    // -----------------------------------------------------------------------

    struct SpyChannel {
        count: Arc<AtomicUsize>,
    }

    impl NotificationChannel for SpyChannel {
        fn notify_gate(&self, _entry: &LedgerEntry) -> Result<(), String> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // log
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn log_appends_entry_with_permitted_status() {
        let ledger = Ledger::default();
        let event = msg_event();
        let id = ledger
            .log(&event, serde_json::json!({"model": "qwen3", "tokens": 42}))
            .await;

        let entry = ledger.get(&id).await.expect("entry should exist");
        assert_eq!(entry.validation_status, ValidationStatus::Permitted);
        assert_eq!(entry.gate_status, GateStatus::None);
        assert_eq!(entry.event_type, "message");
        assert!(entry.response.is_some());
    }

    #[tokio::test]
    async fn log_multiple_interactions_all_recorded() {
        let ledger = Ledger::default();
        let event = msg_event();
        for i in 0..5 {
            ledger.log(&event, serde_json::json!({"i": i})).await;
        }
        assert_eq!(ledger.all_entries().await.len(), 5);
    }

    // -----------------------------------------------------------------------
    // log captures channel context
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn log_captures_channel_context() {
        let ledger = Ledger::default();
        let event = msg_event();
        let id = ledger.log(&event, serde_json::json!(null)).await;
        let entry = ledger.get(&id).await.unwrap();
        assert_eq!(entry.context.channel.as_deref(), Some("test"));
    }

    // -----------------------------------------------------------------------
    // validate
    // -----------------------------------------------------------------------

    #[test]
    fn validate_permitted_for_unknown_action() {
        let ledger = ledger_with_policies();
        assert_eq!(ledger.validate("read_file"), ValidationStatus::Permitted);
    }

    #[test]
    fn validate_gate_required_for_gated_action() {
        let ledger = ledger_with_policies();
        assert_eq!(
            ledger.validate("send_email:alice@example.com"),
            ValidationStatus::GateRequired
        );
    }

    #[test]
    fn validate_denied_for_denied_action() {
        let ledger = ledger_with_policies();
        assert_eq!(
            ledger.validate("delete_all:users"),
            ValidationStatus::Denied
        );
    }

    // -----------------------------------------------------------------------
    // gate lifecycle
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn gate_creates_pending_entry_and_notifies() {
        let count = Arc::new(AtomicUsize::new(0));
        let ledger = Ledger::new(Arc::new(SpyChannel {
            count: count.clone(),
        }));

        let gate_id = ledger
            .gate("send_email:bob@example.com", "User asked to send email")
            .await
            .expect("gate should succeed");

        let entry = ledger.get(&gate_id).await.expect("entry should exist");
        assert_eq!(entry.gate_status, GateStatus::Pending);
        assert_eq!(entry.validation_status, ValidationStatus::GateRequired);
        assert_eq!(count.load(Ordering::SeqCst), 1, "user should be notified");
    }

    #[tokio::test]
    async fn resolve_gate_approved() {
        let ledger = Ledger::default();
        let gate_id = ledger
            .gate("send_email:alice", "test")
            .await
            .expect("gate should succeed");

        ledger
            .resolve_gate(&gate_id, true)
            .await
            .expect("resolve should succeed");

        let entry = ledger.get(&gate_id).await.unwrap();
        assert_eq!(entry.gate_status, GateStatus::Approved);
    }

    #[tokio::test]
    async fn resolve_gate_rejected() {
        let ledger = Ledger::default();
        let gate_id = ledger
            .gate("post_public:twitter", "Posting a thread")
            .await
            .expect("gate should succeed");

        ledger
            .resolve_gate(&gate_id, false)
            .await
            .expect("resolve should succeed");

        let entry = ledger.get(&gate_id).await.unwrap();
        assert_eq!(entry.gate_status, GateStatus::Rejected);
    }

    #[tokio::test]
    async fn resolve_gate_error_on_unknown_id() {
        let ledger = Ledger::default();
        let result = ledger.resolve_gate("nonexistent-id", true).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn resolve_gate_error_if_already_resolved() {
        let ledger = Ledger::default();
        let gate_id = ledger.gate("send_email:carol", "test").await.unwrap();
        ledger.resolve_gate(&gate_id, true).await.unwrap();

        // Resolving again should fail.
        let result = ledger.resolve_gate(&gate_id, false).await;
        assert!(result.is_err(), "double-resolve should return an error");
    }

    // -----------------------------------------------------------------------
    // check_gates
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn check_gates_returns_only_pending() {
        let ledger = Ledger::default();
        let event = msg_event();

        let g1 = ledger.gate("send_email:a", "reason a").await.unwrap();
        let g2 = ledger.gate("send_email:b", "reason b").await.unwrap();
        ledger.resolve_gate(&g1, true).await.unwrap(); // approve first gate

        let pending = ledger.check_gates(&event).await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, g2);
    }

    #[tokio::test]
    async fn check_gates_empty_when_no_pending() {
        let ledger = Ledger::default();
        let event = msg_event();
        assert!(ledger.check_gates(&event).await.is_empty());
    }

    // -----------------------------------------------------------------------
    // check_gates filters by context (channel)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn check_gates_filters_by_channel() {
        let ledger = Ledger::default();

        // Create two gates on different channels.
        ledger
            .gate_with_context(
                "send_email:a",
                "reason a",
                LedgerContext::from_channel("telegram"),
            )
            .await
            .unwrap();
        ledger
            .gate_with_context(
                "send_email:b",
                "reason b",
                LedgerContext::from_channel("stdin"),
            )
            .await
            .unwrap();

        // check_gates for a "telegram" message event should only return the
        // telegram gate.
        let telegram_event = Event::Message {
            id: "2".into(),
            channel: "telegram".into(),
            sender: "u".into(),
            content: "".into(),
        };
        let pending = ledger.check_gates(&telegram_event).await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].context.channel.as_deref(), Some("telegram"));
    }

    // -----------------------------------------------------------------------
    // Full gate flow
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn full_gate_flow_validate_gate_resolve_proceed() {
        let ledger = ledger_with_policies();
        let action = "send_email:team@example.com";

        // Step 1: validate — should require a gate
        let status = ledger.validate(action);
        assert_eq!(status, ValidationStatus::GateRequired);

        // Step 2: create the gate
        let gate_id = ledger
            .gate(action, "User asked to send a team email")
            .await
            .unwrap();

        // Step 3: user approves
        ledger.resolve_gate(&gate_id, true).await.unwrap();

        // Step 4: procedure checks gate is approved and continues
        let entry = ledger.get(&gate_id).await.unwrap();
        assert_eq!(entry.gate_status, GateStatus::Approved);
    }

    #[tokio::test]
    async fn full_gate_flow_validate_gate_resolve_abort() {
        let ledger = ledger_with_policies();
        let action = "post_public:reddit";

        let status = ledger.validate(action);
        assert_eq!(status, ValidationStatus::GateRequired);

        let gate_id = ledger
            .gate(action, "Post to Reddit community")
            .await
            .unwrap();
        ledger.resolve_gate(&gate_id, false).await.unwrap();

        let entry = ledger.get(&gate_id).await.unwrap();
        assert_eq!(entry.gate_status, GateStatus::Rejected);
    }

    // -----------------------------------------------------------------------
    // Audit export
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn export_json_produces_array() {
        let ledger = Ledger::default();
        let event = msg_event();
        ledger
            .log(&event, serde_json::json!({"model": "qwen3"}))
            .await;
        ledger.gate("send_email:x", "reason").await.unwrap();

        let lic = crate::license::License::pro(None);
        let json = ledger
            .export_json(&lic)
            .await
            .expect("pro license should allow export");
        let arr = json.as_array().expect("export should be a JSON array");
        assert_eq!(arr.len(), 2);
    }

    #[tokio::test]
    async fn export_json_entries_have_required_fields() {
        let ledger = Ledger::default();
        let event = msg_event();
        ledger.log(&event, serde_json::json!(null)).await;

        let lic = crate::license::License::pro(None);
        let json = ledger
            .export_json(&lic)
            .await
            .expect("pro license should allow export");
        let entry = &json[0];
        assert!(entry.get("id").is_some());
        assert!(entry.get("timestamp").is_some());
        assert!(entry.get("event_type").is_some());
        assert!(entry.get("action").is_some());
        assert!(entry.get("rationale").is_some());
        assert!(entry.get("validation_status").is_some());
        assert!(entry.get("gate_status").is_some());
    }

    #[tokio::test]
    async fn export_json_empty_ledger_is_empty_array() {
        let ledger = Ledger::default();
        let lic = crate::license::License::pro(None);
        let json = ledger
            .export_json(&lic)
            .await
            .expect("pro license should allow export");
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn export_json_blocked_on_free_tier() {
        let ledger = Ledger::default();
        let event = msg_event();
        ledger
            .log(&event, serde_json::json!({"model": "qwen3"}))
            .await;

        let lic = crate::license::License::free();
        let result = ledger.export_json(&lic).await;
        assert!(
            matches!(
                result,
                Err(crate::license::LicenseError::FeatureNotAvailable { .. })
            ),
            "free tier should not be able to export the audit ledger"
        );
    }

    // -----------------------------------------------------------------------
    // PluresDbLedgerStore persistence round-trip
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn pluresdb_store_insert_and_all() {
        let store = PluresDbLedgerStore::in_memory();
        let entry = LedgerEntry::new(
            "message",
            "send_email:x",
            "reason",
            ValidationStatus::GateRequired,
            GateStatus::Pending,
            None,
        );
        let id = entry.id.clone();
        store.insert(entry).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
    }

    #[tokio::test]
    async fn pluresdb_store_roundtrip_preserves_fields() {
        let store = PluresDbLedgerStore::in_memory();
        let entry = LedgerEntry::new(
            "message",
            "post_public:reddit",
            "User requested post",
            ValidationStatus::GateRequired,
            GateStatus::Pending,
            None,
        )
        .with_context(LedgerContext::from_channel("telegram"));
        let id = entry.id.clone();
        store.insert(entry).await.unwrap();

        let got = store.get(&id).await.unwrap().expect("entry should exist");
        assert_eq!(got.id, id);
        assert_eq!(got.action, "post_public:reddit");
        assert_eq!(got.gate_status, GateStatus::Pending);
        assert_eq!(got.context.channel.as_deref(), Some("telegram"));
    }

    #[tokio::test]
    async fn pluresdb_store_update_gate_status() {
        let store = PluresDbLedgerStore::in_memory();
        let entry = LedgerEntry::new(
            "gate",
            "send_email:a",
            "reason",
            ValidationStatus::GateRequired,
            GateStatus::Pending,
            None,
        );
        let id = entry.id.clone();
        store.insert(entry).await.unwrap();

        store
            .update_gate_status(&id, GateStatus::Approved)
            .await
            .unwrap();

        let got = store.get(&id).await.unwrap().unwrap();
        assert_eq!(got.gate_status, GateStatus::Approved);
    }

    #[tokio::test]
    async fn pluresdb_store_pending_gates_by_context() {
        let store = PluresDbLedgerStore::in_memory();

        let e1 = LedgerEntry::new(
            "gate",
            "send_email:a",
            "r",
            ValidationStatus::GateRequired,
            GateStatus::Pending,
            None,
        )
        .with_context(LedgerContext::from_channel("telegram"));
        let e2 = LedgerEntry::new(
            "gate",
            "send_email:b",
            "r",
            ValidationStatus::GateRequired,
            GateStatus::Pending,
            None,
        )
        .with_context(LedgerContext::from_channel("stdin"));

        store.insert(e1).await.unwrap();
        store.insert(e2).await.unwrap();

        let ctx = LedgerContext::from_channel("telegram");
        let pending = store.pending_gates_by_context(&ctx).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].context.channel.as_deref(), Some("telegram"));
    }

    #[tokio::test]
    async fn pluresdb_store_persistence_survives_reload() {
        let dir = tempfile::tempdir().unwrap();

        {
            let store = PluresDbLedgerStore::open(dir.path()).unwrap();
            let entry = LedgerEntry::new(
                "message",
                "send_email:bob",
                "reason",
                ValidationStatus::GateRequired,
                GateStatus::Pending,
                None,
            );
            store.insert(entry).await.unwrap();
        }

        // Reopen the same path — entries must survive the restart.
        let store2 = PluresDbLedgerStore::open(dir.path()).unwrap();
        let all = store2.all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].action, "send_email:bob");
    }

    // -----------------------------------------------------------------------
    // Ledger backed by PluresDbLedgerStore
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn ledger_with_pluresdb_store_full_gate_flow() {
        let store = Arc::new(PluresDbLedgerStore::in_memory());
        let ledger = Ledger::with_store(store, Arc::new(NoOpChannel));

        let gate_id = ledger
            .gate("send_email:alice", "needs approval")
            .await
            .expect("gate should be created");
        ledger.resolve_gate(&gate_id, true).await.unwrap();

        let entry = ledger.get(&gate_id).await.unwrap();
        assert_eq!(entry.gate_status, GateStatus::Approved);
    }

    // -----------------------------------------------------------------------
    // LedgerContext matching
    // -----------------------------------------------------------------------

    #[test]
    fn context_matches_same_channel() {
        let a = LedgerContext::from_channel("telegram");
        let b = LedgerContext::from_channel("telegram");
        assert!(a.matches(&b));
    }

    #[test]
    fn context_does_not_match_different_channel() {
        let a = LedgerContext::from_channel("telegram");
        let b = LedgerContext::from_channel("stdin");
        assert!(!a.matches(&b));
    }

    #[test]
    fn context_wildcard_matches_any_channel() {
        let wildcard = LedgerContext::default(); // no channel set
        let specific = LedgerContext::from_channel("telegram");
        assert!(wildcard.matches(&specific));
        assert!(specific.matches(&wildcard));
    }
}
