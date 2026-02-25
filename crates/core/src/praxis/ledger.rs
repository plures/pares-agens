use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
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
        }
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

/// In-memory ledger that maps to the `praxis_ledger` PluresDB table.
///
/// The inner state is wrapped in an `Arc<Mutex<…>>` so the ledger can be
/// cheaply cloned and shared across async tasks while keeping CRUD
/// operations synchronous (PluresDB writes will eventually be async, but
/// that migration is out of scope here).
#[derive(Clone)]
pub struct Ledger {
    entries: Arc<Mutex<Vec<LedgerEntry>>>,
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
    /// Create a new ledger with the given notification channel and empty
    /// policy lists.
    pub fn new(channel: Arc<dyn NotificationChannel>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            channel,
            gated_actions: Arc::new(Vec::new()),
            denied_actions: Arc::new(Vec::new()),
        }
    }

    /// Create a ledger with explicit policy lists.
    ///
    /// * `gated_actions` — action prefixes/names that trigger a gate.
    /// * `denied_actions` — action prefixes/names that are always denied.
    pub fn with_policies(
        channel: Arc<dyn NotificationChannel>,
        gated_actions: Vec<String>,
        denied_actions: Vec<String>,
    ) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            channel,
            gated_actions: Arc::new(gated_actions),
            denied_actions: Arc::new(denied_actions),
        }
    }

    // -----------------------------------------------------------------------
    // CRUD helpers
    // -----------------------------------------------------------------------

    /// Append an entry and return its id.
    fn insert(&self, entry: LedgerEntry) -> String {
        let id = entry.id.clone();
        self.entries.lock().unwrap().push(entry);
        id
    }

    /// Return an immutable snapshot of all entries.
    pub fn all_entries(&self) -> Vec<LedgerEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// Look up an entry by id.
    pub fn get(&self, id: &str) -> Option<LedgerEntry> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.id == id)
            .cloned()
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
    pub fn log(&self, event: &Event, response: serde_json::Value) -> String {
        let action = format!("model_response:{}", event.kind());
        let rationale = "Audit log of model interaction".to_string();
        let entry = LedgerEntry::new(
            event.kind(),
            action,
            rationale,
            ValidationStatus::Permitted,
            GateStatus::None,
            Some(response),
        );
        tracing::debug!(id = %entry.id, event_type = %entry.event_type, "praxis::log");
        self.insert(entry)
    }

    /// `praxis.validate` — check an action against stored policies.
    ///
    /// Returns:
    /// * [`ValidationStatus::Permitted`] — safe to proceed.
    /// * [`ValidationStatus::GateRequired`] — must call [`Ledger::gate`] first.
    /// * [`ValidationStatus::Denied`] — action is forbidden.
    pub fn validate(&self, action: &str) -> ValidationStatus {
        if self.denied_actions.iter().any(|d| action.starts_with(d.as_str())) {
            return ValidationStatus::Denied;
        }
        if self.gated_actions.iter().any(|g| action.starts_with(g.as_str())) {
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
    pub fn gate(
        &self,
        action: impl Into<String>,
        rationale: impl Into<String>,
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
        );
        let id = entry.id.clone();
        tracing::info!(gate_id = %id, %action, "praxis::gate: created pending gate");
        self.insert(entry.clone());
        self.channel.notify_gate(&entry)?;
        Ok(id)
    }

    /// Resolve a previously created gate.
    ///
    /// Updates the entry's `gate_status` to `Approved` or `Rejected`.
    /// Returns `Err` if the gate id is not found or the gate is not in
    /// [`GateStatus::Pending`] state.
    pub fn resolve_gate(&self, gate_id: &str, approved: bool) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .iter_mut()
            .find(|e| e.id == gate_id)
            .ok_or_else(|| format!("gate not found: {gate_id}"))?;

        if entry.gate_status != GateStatus::Pending {
            return Err(format!(
                "gate {gate_id} is not pending (current status: {:?})",
                entry.gate_status
            ));
        }

        entry.gate_status = if approved {
            GateStatus::Approved
        } else {
            GateStatus::Rejected
        };

        tracing::info!(
            gate_id,
            approved,
            "praxis::resolve_gate: gate resolved"
        );
        Ok(())
    }

    /// `praxis.check_gates` — return all pending gates for the given event context.
    ///
    /// The current implementation returns every [`GateStatus::Pending`] entry
    /// in the ledger.  A future version will filter by context (channel,
    /// session, etc.) once those fields are available.
    pub fn check_gates(&self, _event: &Event) -> Vec<LedgerEntry> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.gate_status == GateStatus::Pending)
            .cloned()
            .collect()
    }

    // -----------------------------------------------------------------------
    // Audit export
    // -----------------------------------------------------------------------

    /// Export the full ledger as a JSON array.
    ///
    /// Each element is a serialised [`LedgerEntry`].  The output is suitable
    /// for archiving, compliance audits, or shipping to a remote log store.
    pub fn export_json(&self) -> serde_json::Value {
        let entries = self.all_entries();
        serde_json::to_value(&entries).unwrap_or(serde_json::Value::Array(vec![]))
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

    #[test]
    fn log_appends_entry_with_permitted_status() {
        let ledger = Ledger::default();
        let event = msg_event();
        let id = ledger.log(&event, serde_json::json!({"model": "qwen3", "tokens": 42}));

        let entry = ledger.get(&id).expect("entry should exist");
        assert_eq!(entry.validation_status, ValidationStatus::Permitted);
        assert_eq!(entry.gate_status, GateStatus::None);
        assert_eq!(entry.event_type, "message");
        assert!(entry.response.is_some());
    }

    #[test]
    fn log_multiple_interactions_all_recorded() {
        let ledger = Ledger::default();
        let event = msg_event();
        for i in 0..5 {
            ledger.log(&event, serde_json::json!({"i": i}));
        }
        assert_eq!(ledger.all_entries().len(), 5);
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

    #[test]
    fn gate_creates_pending_entry_and_notifies() {
        let count = Arc::new(AtomicUsize::new(0));
        let ledger = Ledger::new(Arc::new(SpyChannel {
            count: count.clone(),
        }));

        let gate_id = ledger
            .gate("send_email:bob@example.com", "User asked to send email")
            .expect("gate should succeed");

        let entry = ledger.get(&gate_id).expect("entry should exist");
        assert_eq!(entry.gate_status, GateStatus::Pending);
        assert_eq!(entry.validation_status, ValidationStatus::GateRequired);
        assert_eq!(count.load(Ordering::SeqCst), 1, "user should be notified");
    }

    #[test]
    fn resolve_gate_approved() {
        let ledger = Ledger::default();
        let gate_id = ledger
            .gate("send_email:alice", "test")
            .expect("gate should succeed");

        ledger
            .resolve_gate(&gate_id, true)
            .expect("resolve should succeed");

        let entry = ledger.get(&gate_id).unwrap();
        assert_eq!(entry.gate_status, GateStatus::Approved);
    }

    #[test]
    fn resolve_gate_rejected() {
        let ledger = Ledger::default();
        let gate_id = ledger
            .gate("post_public:twitter", "Posting a thread")
            .expect("gate should succeed");

        ledger
            .resolve_gate(&gate_id, false)
            .expect("resolve should succeed");

        let entry = ledger.get(&gate_id).unwrap();
        assert_eq!(entry.gate_status, GateStatus::Rejected);
    }

    #[test]
    fn resolve_gate_error_on_unknown_id() {
        let ledger = Ledger::default();
        let result = ledger.resolve_gate("nonexistent-id", true);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_gate_error_if_already_resolved() {
        let ledger = Ledger::default();
        let gate_id = ledger.gate("send_email:carol", "test").unwrap();
        ledger.resolve_gate(&gate_id, true).unwrap();

        // Resolving again should fail.
        let result = ledger.resolve_gate(&gate_id, false);
        assert!(result.is_err(), "double-resolve should return an error");
    }

    // -----------------------------------------------------------------------
    // check_gates
    // -----------------------------------------------------------------------

    #[test]
    fn check_gates_returns_only_pending() {
        let ledger = Ledger::default();
        let event = msg_event();

        let g1 = ledger.gate("send_email:a", "reason a").unwrap();
        let g2 = ledger.gate("send_email:b", "reason b").unwrap();
        ledger.resolve_gate(&g1, true).unwrap(); // approve first gate

        let pending = ledger.check_gates(&event);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, g2);
    }

    #[test]
    fn check_gates_empty_when_no_pending() {
        let ledger = Ledger::default();
        let event = msg_event();
        assert!(ledger.check_gates(&event).is_empty());
    }

    // -----------------------------------------------------------------------
    // Full gate flow
    // -----------------------------------------------------------------------

    #[test]
    fn full_gate_flow_validate_gate_resolve_proceed() {
        let ledger = ledger_with_policies();
        let action = "send_email:team@example.com";

        // Step 1: validate — should require a gate
        let status = ledger.validate(action);
        assert_eq!(status, ValidationStatus::GateRequired);

        // Step 2: create the gate
        let gate_id = ledger
            .gate(action, "User asked to send a team email")
            .unwrap();

        // Step 3: user approves
        ledger.resolve_gate(&gate_id, true).unwrap();

        // Step 4: procedure checks gate is approved and continues
        let entry = ledger.get(&gate_id).unwrap();
        assert_eq!(entry.gate_status, GateStatus::Approved);
    }

    #[test]
    fn full_gate_flow_validate_gate_resolve_abort() {
        let ledger = ledger_with_policies();
        let action = "post_public:reddit";

        let status = ledger.validate(action);
        assert_eq!(status, ValidationStatus::GateRequired);

        let gate_id = ledger.gate(action, "Post to Reddit community").unwrap();
        ledger.resolve_gate(&gate_id, false).unwrap();

        let entry = ledger.get(&gate_id).unwrap();
        assert_eq!(entry.gate_status, GateStatus::Rejected);
    }

    // -----------------------------------------------------------------------
    // Audit export
    // -----------------------------------------------------------------------

    #[test]
    fn export_json_produces_array() {
        let ledger = Ledger::default();
        let event = msg_event();
        ledger.log(&event, serde_json::json!({"model": "qwen3"}));
        ledger.gate("send_email:x", "reason").unwrap();

        let json = ledger.export_json();
        let arr = json.as_array().expect("export should be a JSON array");
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn export_json_entries_have_required_fields() {
        let ledger = Ledger::default();
        let event = msg_event();
        ledger.log(&event, serde_json::json!(null));

        let json = ledger.export_json();
        let entry = &json[0];
        assert!(entry.get("id").is_some());
        assert!(entry.get("timestamp").is_some());
        assert!(entry.get("event_type").is_some());
        assert!(entry.get("action").is_some());
        assert!(entry.get("rationale").is_some());
        assert!(entry.get("validation_status").is_some());
        assert!(entry.get("gate_status").is_some());
    }

    #[test]
    fn export_json_empty_ledger_is_empty_array() {
        let ledger = Ledger::default();
        let json = ledger.export_json();
        assert_eq!(json, serde_json::json!([]));
    }
}
