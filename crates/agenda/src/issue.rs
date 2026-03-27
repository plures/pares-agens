//! Issue work-item model and lifecycle for Agenda.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AgendaError;

// ── IssueStatus ───────────────────────────────────────────────────────────────

/// Lifecycle state of an [`Issue`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    /// The issue is open and awaiting action.
    Open,
    /// Work on the issue is actively in progress.
    InProgress,
    /// The issue has been resolved.
    Closed,
}

// ── IssuePriority ─────────────────────────────────────────────────────────────

/// Priority level of an [`Issue`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuePriority {
    /// Low priority — address when convenient.
    Low,
    /// Normal priority.
    Normal,
    /// High priority — address soon.
    High,
    /// Critical — blocks progress; address immediately.
    Critical,
}

// ── Issue ─────────────────────────────────────────────────────────────────────

/// A single work item tracked by the Agenda.
///
/// # Example
///
/// ```
/// use pares_agens_agenda::issue::{Issue, IssuePriority};
///
/// let issue = Issue::new("Fix login bug", "Users cannot log in with SSO.").unwrap();
/// assert_eq!(issue.title(), "Fix login bug");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Unique identifier (UUID v4).
    pub id: String,

    /// Short, human-readable title (1–200 characters).
    pub title: String,

    /// Full description of the issue (may be empty).
    pub description: String,

    /// Current lifecycle status.
    pub status: IssueStatus,

    /// Priority level.
    pub priority: IssuePriority,

    /// Labels/tags attached to this issue.
    pub labels: Vec<String>,

    /// UTC timestamp when the issue was created.
    pub created_at: DateTime<Utc>,

    /// UTC timestamp of the most recent update.
    pub updated_at: DateTime<Utc>,
}

impl Issue {
    /// Create a new `Issue` with `Open` status and `Normal` priority.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::InvalidField`] when `title` is empty or exceeds
    /// 200 characters.
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<Self, AgendaError> {
        let title = title.into();
        if title.is_empty() {
            return Err(AgendaError::InvalidField(
                "title must not be empty".to_string(),
            ));
        }
        if title.len() > 200 {
            return Err(AgendaError::InvalidField(
                "title must not exceed 200 characters".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            title,
            description: description.into(),
            status: IssueStatus::Open,
            priority: IssuePriority::Normal,
            labels: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Return the issue title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Transition this issue to a new [`IssueStatus`].
    ///
    /// Allowed transitions:
    /// - `Open` → `InProgress`
    /// - `Open` → `Closed`
    /// - `InProgress` → `Closed`
    /// - `Closed` → `Open` (reopen)
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::InvalidTransition`] for disallowed transitions
    /// (e.g. `InProgress` → `Open`).
    pub fn transition(&mut self, next: IssueStatus) -> Result<(), AgendaError> {
        let allowed = matches!(
            (&self.status, &next),
            (IssueStatus::Open, IssueStatus::InProgress)
                | (IssueStatus::Open, IssueStatus::Closed)
                | (IssueStatus::InProgress, IssueStatus::Closed)
                | (IssueStatus::Closed, IssueStatus::Open)
        );
        if !allowed {
            return Err(AgendaError::InvalidTransition(format!(
                "{:?} → {:?}",
                self.status, next
            )));
        }
        self.status = next;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Add a label to the issue (no-op if the label is already present).
    pub fn add_label(&mut self, label: impl Into<String>) {
        let label = label.into();
        if !self.labels.contains(&label) {
            self.labels.push(label);
            self.updated_at = Utc::now();
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_issue_has_open_status_and_normal_priority() {
        let issue = Issue::new("test", "desc").unwrap();
        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(issue.priority, IssuePriority::Normal);
    }

    #[test]
    fn new_issue_rejects_empty_title() {
        assert!(matches!(
            Issue::new("", "desc"),
            Err(AgendaError::InvalidField(_))
        ));
    }

    #[test]
    fn new_issue_rejects_title_over_200_chars() {
        let long: String = "a".repeat(201);
        assert!(matches!(
            Issue::new(long, ""),
            Err(AgendaError::InvalidField(_))
        ));
    }

    #[test]
    fn transition_open_to_in_progress_succeeds() {
        let mut issue = Issue::new("t", "d").unwrap();
        assert!(issue.transition(IssueStatus::InProgress).is_ok());
        assert_eq!(issue.status, IssueStatus::InProgress);
    }

    #[test]
    fn transition_in_progress_to_open_is_rejected() {
        let mut issue = Issue::new("t", "d").unwrap();
        issue.transition(IssueStatus::InProgress).unwrap();
        assert!(matches!(
            issue.transition(IssueStatus::Open),
            Err(AgendaError::InvalidTransition(_))
        ));
    }

    #[test]
    fn transition_closed_to_open_reopens_issue() {
        let mut issue = Issue::new("t", "d").unwrap();
        issue.transition(IssueStatus::Closed).unwrap();
        assert!(issue.transition(IssueStatus::Open).is_ok());
        assert_eq!(issue.status, IssueStatus::Open);
    }

    #[test]
    fn add_label_is_idempotent() {
        let mut issue = Issue::new("t", "d").unwrap();
        issue.add_label("bug");
        issue.add_label("bug");
        assert_eq!(issue.labels.len(), 1);
    }
}
