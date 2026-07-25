//! Interactive Telegram `/model` picker state (ADR-0021).
//!
//! Wraps the pares-radix `PoolControl::catalog` / `select_by_key` structured
//! API in a short-lived, owner-scoped, server-side session so a Telegram
//! button click can never smuggle a model identifier through
//! `callback_data` (Telegram limits that field to 64 bytes and it is visible
//! to anyone who can see the message). Instead, callback data only ever
//! carries an opaque session id + a small integer (absolute entry index or
//! page number), and the real [`ModelCatalogEntry`] list lives here, indexed
//! by that session id, until it expires or is consumed.
//!
//! This module is pure state + parsing/formatting logic — it has no
//! dependency on `teloxide` so it is fully unit-testable without a Telegram
//! transport, per the project's channel-agnostic test discipline.

use pares_radix_core::model_pool::{ModelCatalogEntry, ModelCatalogPage};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a picker session stays valid after creation/last navigation.
pub const SESSION_TTL: Duration = Duration::from_secs(300);
/// Models shown per page (keeps the inline keyboard well under Telegram's
/// message/keyboard size limits even for long display names).
pub const DEFAULT_PAGE_SIZE: usize = 6;

/// A single live `/model` picker session, scoped to one Telegram message.
#[derive(Debug, Clone)]
struct PickerSession {
    /// Telegram user id that opened the picker; only this user's callback
    /// queries are honored (owner-scoped, per ADR-0021 + issue #603).
    owner_user_id: i64,
    chat_id: i64,
    message_id: i32,
    /// Fixed snapshot of the catalog at the moment the picker was opened.
    /// Navigating pages re-slices this snapshot; it is never re-fetched
    /// mid-session, so a page-2 click cannot land on a different model set
    /// than what was shown on page 1.
    entries: Vec<ModelCatalogEntry>,
    page_size: usize,
    page: usize,
    last_active_at: Instant,
    /// Set once a selection has been made; further callbacks on this
    /// session are inert (consume-on-select).
    consumed: bool,
}

impl PickerSession {
    fn is_expired(&self, now: Instant, ttl: Duration) -> bool {
        now.duration_since(self.last_active_at) > ttl
    }

    fn total_pages(&self) -> usize {
        if self.entries.is_empty() {
            1
        } else {
            self.entries.len().div_ceil(self.page_size)
        }
    }

    fn page_slice(&self) -> &[ModelCatalogEntry] {
        let start = (self.page * self.page_size).min(self.entries.len());
        let end = (start + self.page_size).min(self.entries.len());
        &self.entries[start..end]
    }
}

/// A parsed `/model` picker callback action. `session_id` is always the
/// first opaque segment; the rest determines what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Select the entry at absolute index `idx` within the session snapshot.
    Select {
        /// Opaque session id this callback belongs to.
        session_id: String,
        /// Absolute index into the session's fixed entry snapshot.
        idx: usize,
    },
    /// Navigate to page `page` (0-indexed) within the session snapshot.
    Page {
        /// Opaque session id this callback belongs to.
        session_id: String,
        /// Target page number (0-indexed).
        page: usize,
    },
    /// Cancel/dismiss the picker.
    Cancel {
        /// Opaque session id this callback belongs to.
        session_id: String,
    },
}

impl PickerAction {
    /// Return the opaque session id carried by this callback.
    pub fn session_id(&self) -> &str {
        match self {
            Self::Select { session_id, .. }
            | Self::Page { session_id, .. }
            | Self::Cancel { session_id } => session_id,
        }
    }
}

const CB_PREFIX: &str = "mp";

/// Build the opaque callback-data string for selecting entry `idx`.
pub fn select_callback_data(session_id: &str, idx: usize) -> String {
    format!("{CB_PREFIX}:{session_id}:s:{idx}")
}

/// Build the opaque callback-data string for navigating to `page`.
pub fn page_callback_data(session_id: &str, page: usize) -> String {
    format!("{CB_PREFIX}:{session_id}:p:{page}")
}

/// Build the opaque callback-data string for cancelling the picker.
pub fn cancel_callback_data(session_id: &str) -> String {
    format!("{CB_PREFIX}:{session_id}:c:0")
}

/// Parse `callback_data` produced by this module. Returns `None` for any
/// data this module didn't emit (foreign/malformed callbacks are simply
/// ignored by the caller, per ADR-0021).
pub fn parse_callback(data: &str) -> Option<PickerAction> {
    let mut parts = data.splitn(4, ':');
    let prefix = parts.next()?;
    if prefix != CB_PREFIX {
        return None;
    }
    let session_id = parts.next()?.to_string();
    let kind = parts.next()?;
    let value = parts.next()?;
    match kind {
        "s" => value
            .parse::<usize>()
            .ok()
            .map(|idx| PickerAction::Select { session_id, idx }),
        "p" => value
            .parse::<usize>()
            .ok()
            .map(|page| PickerAction::Page { session_id, page }),
        "c" => Some(PickerAction::Cancel { session_id }),
        _ => None,
    }
}

/// A rendered page of the picker: text body + `(label, callback_data)`
/// button rows ready to hand to `build_inline_keyboard`.
#[derive(Debug, Clone)]
pub struct RenderedPage {
    /// The message body/header text (page indicator + total count).
    pub text: String,
    /// Rows of (label, callback_data) pairs; one model per row plus a final
    /// nav row (Prev/Next/Cancel, only the applicable buttons included).
    pub rows: Vec<Vec<(String, String)>>,
}

/// Render an entry's label for the picker keyboard: `provider/model_id`,
/// with a checkmark when it matches the currently active model key and a
/// test-tube marker for preview/experimental models.
fn format_entry_label(entry: &ModelCatalogEntry, is_current: bool) -> String {
    let check = if is_current { "✅ " } else { "" };
    let preview = if entry.preview { " 🧪" } else { "" };
    format!("{check}{}/{}{preview}", entry.provider, entry.model_id)
}

/// Thread-safe registry of live picker sessions, keyed by opaque session id.
#[derive(Default)]
pub struct ModelPickerStore {
    sessions: Mutex<HashMap<String, PickerSession>>,
}

impl ModelPickerStore {
    /// Create an empty picker session store.
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Open a new picker session from a fresh, already-refreshed catalog
    /// page/snapshot. Returns the session id and the initial rendered page.
    pub fn open(
        &self,
        owner_user_id: i64,
        chat_id: i64,
        message_id: i32,
        entries: Vec<ModelCatalogEntry>,
        current_key: Option<&str>,
        page_size: usize,
    ) -> (String, RenderedPage) {
        self.sweep_expired();
        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let session = PickerSession {
            owner_user_id,
            chat_id,
            message_id,
            entries,
            page_size: page_size.max(1),
            page: 0,
            last_active_at: Instant::now(),
            consumed: false,
        };
        let rendered = render_session(&session, &session_id, current_key);
        self.sessions.lock().unwrap().insert(session_id.clone(), session);
        (session_id, rendered)
    }

    /// Handle a parsed callback action for the given Telegram user. Returns
    /// `None` if the session is missing/expired/consumed, or the callback
    /// user does not own the session (foreign callback — a no-op per
    /// ADR-0021, never an error surfaced to the caller).
    pub fn handle(
        &self,
        action: &PickerAction,
        caller_user_id: i64,
        current_key: Option<&str>,
    ) -> Option<PickerOutcome> {
        let session_id = match action {
            PickerAction::Select { session_id, .. }
            | PickerAction::Page { session_id, .. }
            | PickerAction::Cancel { session_id } => session_id.clone(),
        };
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.get_mut(&session_id)?;
        let now = Instant::now();
        if session.is_expired(now, SESSION_TTL) || session.consumed {
            return None;
        }
        if session.owner_user_id != caller_user_id {
            // Foreign callback: acknowledged upstream, no state change.
            return Some(PickerOutcome::Ignored);
        }
        match action {
            PickerAction::Page { page, .. } => {
                let total_pages = session.total_pages();
                session.page = (*page).min(total_pages.saturating_sub(1));
                session.last_active_at = now;
                let rendered = render_session(session, &session_id, current_key);
                Some(PickerOutcome::Rendered(rendered))
            }
            PickerAction::Select { idx, .. } => {
                let entry = session.entries.get(*idx).cloned();
                match entry {
                    Some(entry) => {
                        session.consumed = true;
                        Some(PickerOutcome::Selected(entry))
                    }
                    None => Some(PickerOutcome::Ignored),
                }
            }
            PickerAction::Cancel { .. } => {
                session.consumed = true;
                Some(PickerOutcome::Cancelled)
            }
        }
    }

    /// Set the outbound Telegram message that hosts this picker after it is
    /// successfully sent. The session is created first so its opaque id can
    /// be embedded in that message's keyboard.
    pub fn set_location(&self, session_id: &str, chat_id: i64, message_id: i32) {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(session_id) {
            session.chat_id = chat_id;
            session.message_id = message_id;
        }
    }

    /// Look up the chat/message id a session is bound to (for editing the
    /// original picker message in place). Returns `None` if the session is
    /// gone.
    pub fn location(&self, session_id: &str) -> Option<(i64, i32)> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session_id).map(|s| (s.chat_id, s.message_id))
    }

    /// Drop expired sessions. Called opportunistically on `open()`; safe to
    /// call anytime (e.g. from a maintenance sweep).
    pub fn sweep_expired(&self) {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, s| !s.is_expired(now, SESSION_TTL));
    }

    #[cfg(test)]
    fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

/// Result of handling a picker callback.
#[derive(Debug, Clone)]
pub enum PickerOutcome {
    /// Page navigation produced a new page to render.
    Rendered(RenderedPage),
    /// A model was selected; caller should apply it via
    /// `PoolControl::select_by_key(entry.key)` and edit the message.
    Selected(ModelCatalogEntry),
    /// The picker was cancelled.
    Cancelled,
    /// A foreign, already-consumed, or otherwise inert callback — answer the
    /// callback query (to clear the Telegram spinner) but change nothing.
    Ignored,
}

fn render_session(
    session: &PickerSession,
    session_id: &str,
    current_key: Option<&str>,
) -> RenderedPage {
    let total = session.entries.len();
    let total_pages = session.total_pages();
    let mut text = format!(
        "🧠 <b>Select a model</b> (page {}/{}, {} total)",
        session.page + 1,
        total_pages,
        total
    );
    if total == 0 {
        text.push_str("\n\nNo models discovered. Try /model refresh.");
    }
    let mut rows: Vec<Vec<(String, String)>> = Vec::new();
    for (offset, entry) in session.page_slice().iter().enumerate() {
        let idx = session.page * session.page_size + offset;
        let is_current = current_key.is_some_and(|k| k == entry.key);
        let label = format_entry_label(entry, is_current);
        rows.push(vec![(label, select_callback_data(session_id, idx))]);
    }
    let mut nav_row: Vec<(String, String)> = Vec::new();
    if session.page > 0 {
        nav_row.push((
            "⬅️ Prev".to_string(),
            page_callback_data(session_id, session.page - 1),
        ));
    }
    if session.page + 1 < total_pages {
        nav_row.push((
            "➡️ Next".to_string(),
            page_callback_data(session_id, session.page + 1),
        ));
    }
    nav_row.push(("✖️ Cancel".to_string(), cancel_callback_data(session_id)));
    rows.push(nav_row);
    RenderedPage { text, rows }
}

/// Convenience: build entries directly from a `ModelCatalogPage` for callers
/// that already fetched a single (usually "all in one") page via
/// `PoolControl::catalog(0, 0)`.
pub fn entries_from_catalog_page(page: ModelCatalogPage) -> Vec<ModelCatalogEntry> {
    page.entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use pares_radix_core::model_pool::ModelCost;

    fn entry(provider: &str, model_id: &str, preview: bool) -> ModelCatalogEntry {
        ModelCatalogEntry {
            provider: provider.to_string(),
            model_id: model_id.to_string(),
            display_name: format!("{model_id} display"),
            key: format!("{provider}/{model_id}"),
            enabled: true,
            preview,
            reasoning: false,
            cost: ModelCost::default(),
        }
    }

    fn sample_entries(n: usize) -> Vec<ModelCatalogEntry> {
        (0..n)
            .map(|i| entry("provider", &format!("model-{i}"), false))
            .collect()
    }

    // ── callback parsing ────────────────────────────────────────────────

    #[test]
    fn parse_select_callback() {
        let data = select_callback_data("abc123", 4);
        assert_eq!(
            parse_callback(&data),
            Some(PickerAction::Select {
                session_id: "abc123".to_string(),
                idx: 4
            })
        );
    }

    #[test]
    fn parse_page_callback() {
        let data = page_callback_data("abc123", 2);
        assert_eq!(
            parse_callback(&data),
            Some(PickerAction::Page {
                session_id: "abc123".to_string(),
                page: 2
            })
        );
    }

    #[test]
    fn parse_cancel_callback() {
        let data = cancel_callback_data("abc123");
        assert_eq!(
            parse_callback(&data),
            Some(PickerAction::Cancel {
                session_id: "abc123".to_string()
            })
        );
    }

    #[test]
    fn parse_foreign_callback_returns_none() {
        assert_eq!(parse_callback("stop:req-42"), None);
        assert_eq!(parse_callback("approval:yes:req-42"), None);
        assert_eq!(parse_callback("mp:onlytwo"), None);
        assert_eq!(parse_callback("mp:abc:x:1"), None);
        assert_eq!(parse_callback("mp:abc:s:notanumber"), None);
    }

    // ── session lifecycle ───────────────────────────────────────────────

    #[test]
    fn open_creates_first_page_with_nav_row() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(10);
        let (session_id, page) = store.open(1, 100, 200, entries, None, 4);
        assert!(!session_id.is_empty());
        // 4 model rows + 1 nav row (Next + Cancel, no Prev on page 0)
        assert_eq!(page.rows.len(), 5);
        let nav = &page.rows[4];
        assert!(nav.iter().any(|(label, _)| label.contains("Next")));
        assert!(nav.iter().any(|(label, _)| label.contains("Cancel")));
        assert!(!nav.iter().any(|(label, _)| label.contains("Prev")));
        assert_eq!(store.session_count(), 1);
    }

    #[test]
    fn current_model_marked_with_checkmark() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(2);
        let (_id, page) = store.open(1, 100, 200, entries, Some("provider/model-1"), 4);
        assert!(page.rows[1][0].0.contains('✅'));
        assert!(!page.rows[0][0].0.contains('✅'));
    }

    #[test]
    fn page_navigation_moves_forward_and_back() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(10);
        let (session_id, _) = store.open(1, 100, 200, entries, None, 4);

        let action = PickerAction::Page {
            session_id: session_id.clone(),
            page: 1,
        };
        let outcome = store.handle(&action, 1, None).unwrap();
        let PickerOutcome::Rendered(page) = outcome else {
            panic!("expected Rendered");
        };
        assert!(page.text.contains("page 2/3"));
        // page 1 (middle): both Prev and Next present
        let nav = page.rows.last().unwrap();
        assert!(nav.iter().any(|(l, _)| l.contains("Prev")));
        assert!(nav.iter().any(|(l, _)| l.contains("Next")));

        let action = PickerAction::Page {
            session_id: session_id.clone(),
            page: 2,
        };
        let outcome = store.handle(&action, 1, None).unwrap();
        let PickerOutcome::Rendered(page) = outcome else {
            panic!("expected Rendered");
        };
        // last page (2 entries left): Prev present, no Next
        let nav = page.rows.last().unwrap();
        assert!(nav.iter().any(|(l, _)| l.contains("Prev")));
        assert!(!nav.iter().any(|(l, _)| l.contains("Next")));
    }

    #[test]
    fn page_navigation_clamps_out_of_range_page() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(5);
        let (session_id, _) = store.open(1, 100, 200, entries, None, 4);
        let action = PickerAction::Page {
            session_id,
            page: 99,
        };
        let outcome = store.handle(&action, 1, None).unwrap();
        let PickerOutcome::Rendered(page) = outcome else {
            panic!("expected Rendered");
        };
        assert!(page.text.contains("page 2/2"));
    }

    #[test]
    fn select_consumes_session() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(3);
        let (session_id, _) = store.open(1, 100, 200, entries, None, 4);

        let action = PickerAction::Select {
            session_id: session_id.clone(),
            idx: 1,
        };
        let outcome = store.handle(&action, 1, None).unwrap();
        let PickerOutcome::Selected(entry) = outcome else {
            panic!("expected Selected");
        };
        assert_eq!(entry.model_id, "model-1");

        // Second interaction on the same session is inert (consumed).
        let action2 = PickerAction::Page {
            session_id,
            page: 0,
        };
        assert!(store.handle(&action2, 1, None).is_none());
    }

    #[test]
    fn select_out_of_range_index_is_ignored_not_panicking() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(2);
        let (session_id, _) = store.open(1, 100, 200, entries, None, 4);
        let action = PickerAction::Select {
            session_id,
            idx: 999,
        };
        let outcome = store.handle(&action, 1, None).unwrap();
        assert!(matches!(outcome, PickerOutcome::Ignored));
    }

    #[test]
    fn cancel_consumes_session() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(2);
        let (session_id, _) = store.open(1, 100, 200, entries, None, 4);
        let action = PickerAction::Cancel {
            session_id: session_id.clone(),
        };
        let outcome = store.handle(&action, 1, None).unwrap();
        assert!(matches!(outcome, PickerOutcome::Cancelled));
        assert!(store
            .handle(
                &PickerAction::Cancel { session_id },
                1,
                None
            )
            .is_none());
    }

    #[test]
    fn foreign_user_callback_is_ignored_not_applied() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(2);
        let (session_id, _) = store.open(1, 100, 200, entries, None, 4);

        // A different Telegram user id (999) clicking select must not
        // consume or mutate the session — it's owner-scoped (issue #603).
        let action = PickerAction::Select {
            session_id: session_id.clone(),
            idx: 0,
        };
        let outcome = store.handle(&action, 999, None).unwrap();
        assert!(matches!(outcome, PickerOutcome::Ignored));

        // The rightful owner can still select afterwards.
        let action2 = PickerAction::Select { session_id, idx: 0 };
        let outcome2 = store.handle(&action2, 1, None).unwrap();
        assert!(matches!(outcome2, PickerOutcome::Selected(_)));
    }

    #[test]
    fn location_returns_chat_and_message_id() {
        let store = ModelPickerStore::new();
        let entries = sample_entries(1);
        let (session_id, _) = store.open(42, 555, 0, entries, None, 4);
        store.set_location(&session_id, 555, 777);
        assert_eq!(store.location(&session_id), Some((555, 777)));
    }

    #[test]
    fn location_returns_none_for_unknown_session() {
        let store = ModelPickerStore::new();
        assert_eq!(store.location("nope"), None);
    }

    #[test]
    fn empty_catalog_renders_helpful_message_no_panic() {
        let store = ModelPickerStore::new();
        let (_id, page) = store.open(1, 100, 200, vec![], None, 4);
        assert!(page.text.contains("No models discovered"));
        // Only the nav row (Cancel only, since there's nothing to page).
        assert_eq!(page.rows.len(), 1);
    }

    #[test]
    fn entries_from_catalog_page_unwraps_entries() {
        let entries = sample_entries(3);
        let expected_ids: Vec<String> = entries.iter().map(|e| e.key.clone()).collect();
        let page = ModelCatalogPage {
            entries,
            total: 3,
            page: 0,
            page_size: 3,
            has_more: false,
            snapshot_at: std::time::SystemTime::now(),
        };
        let unwrapped = entries_from_catalog_page(page);
        let unwrapped_ids: Vec<String> = unwrapped.iter().map(|e| e.key.clone()).collect();
        assert_eq!(unwrapped_ids, expected_ids);
    }
}
