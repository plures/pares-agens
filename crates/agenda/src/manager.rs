//! AgendaManager — CRUD for issues and pull requests.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    issue::{Issue, IssuePriority, IssueStatus},
    AgendaError,
};

// ── PrStatus ──────────────────────────────────────────────────────────────────

/// Lifecycle state of a [`PullRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrStatus {
    /// The PR is open and under review.
    Open,
    /// The PR has been merged.
    Merged,
    /// The PR has been closed without merging.
    Closed,
}

// ── PullRequest ───────────────────────────────────────────────────────────────

/// A lightweight pull-request record tracked by the Agenda.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    /// Unique identifier (UUID v4).
    pub id: String,

    /// Short title describing the change.
    pub title: String,

    /// Source branch name.
    pub source_branch: String,

    /// Target branch name.
    pub target_branch: String,

    /// Current lifecycle status.
    pub status: PrStatus,

    /// Optional ID of the issue this PR resolves.
    pub resolves_issue: Option<String>,

    /// UTC timestamp when the PR was opened.
    pub created_at: DateTime<Utc>,

    /// UTC timestamp of the most recent update.
    pub updated_at: DateTime<Utc>,
}

impl PullRequest {
    /// Create a new `PullRequest` with `Open` status.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::InvalidField`] when `title`, `source_branch`,
    /// or `target_branch` is empty, or when `source_branch == target_branch`.
    pub fn new(
        title: impl Into<String>,
        source_branch: impl Into<String>,
        target_branch: impl Into<String>,
    ) -> Result<Self, AgendaError> {
        let title = title.into();
        let source_branch = source_branch.into();
        let target_branch = target_branch.into();

        if title.is_empty() {
            return Err(AgendaError::InvalidField(
                "PR title must not be empty".to_string(),
            ));
        }
        if source_branch.is_empty() {
            return Err(AgendaError::InvalidField(
                "source branch must not be empty".to_string(),
            ));
        }
        if target_branch.is_empty() {
            return Err(AgendaError::InvalidField(
                "target branch must not be empty".to_string(),
            ));
        }
        if source_branch == target_branch {
            return Err(AgendaError::InvalidField(
                "source and target branches must differ".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            title,
            source_branch,
            target_branch,
            status: PrStatus::Open,
            resolves_issue: None,
            created_at: now,
            updated_at: now,
        })
    }
}

// ── AgendaManager ─────────────────────────────────────────────────────────────

/// CRUD manager for [`Issue`]s and [`PullRequest`]s.
///
/// All data is held in memory.  A future version will persist to PluresDB.
///
/// # Example
///
/// ```
/// use pares_agens_agenda::manager::AgendaManager;
///
/// let mut mgr = AgendaManager::new();
/// let id = mgr.create_issue("Implement Arca", "Local storage MVP").unwrap();
/// let issue = mgr.get_issue(&id).unwrap();
/// assert_eq!(issue.title(), "Implement Arca");
/// ```
#[derive(Debug, Default)]
pub struct AgendaManager {
    issues: HashMap<String, Issue>,
    pull_requests: HashMap<String, PullRequest>,
}

impl AgendaManager {
    /// Create a new, empty `AgendaManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── Issues ────────────────────────────────────────────────────────────────

    /// Create an issue and return its ID.
    ///
    /// # Errors
    ///
    /// Propagates [`AgendaError::InvalidField`] from [`Issue::new`].
    pub fn create_issue(
        &mut self,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<String, AgendaError> {
        let issue = Issue::new(title, description)?;
        let id = issue.id.clone();
        self.issues.insert(id.clone(), issue);
        Ok(id)
    }

    /// Retrieve an issue by ID.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::NotFound`] when no issue with `id` exists.
    pub fn get_issue(&self, id: &str) -> Result<&Issue, AgendaError> {
        self.issues
            .get(id)
            .ok_or_else(|| AgendaError::NotFound(id.to_string()))
    }

    /// Retrieve a mutable reference to an issue by ID.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::NotFound`] when no issue with `id` exists.
    pub fn get_issue_mut(&mut self, id: &str) -> Result<&mut Issue, AgendaError> {
        self.issues
            .get_mut(id)
            .ok_or_else(|| AgendaError::NotFound(id.to_string()))
    }

    /// Update the priority of an issue.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::NotFound`] when no issue with `id` exists.
    pub fn set_issue_priority(
        &mut self,
        id: &str,
        priority: IssuePriority,
    ) -> Result<(), AgendaError> {
        let issue = self.get_issue_mut(id)?;
        issue.priority = priority;
        issue.updated_at = Utc::now();
        Ok(())
    }

    /// Transition an issue to a new [`IssueStatus`].
    ///
    /// # Errors
    ///
    /// Propagates [`AgendaError::NotFound`] or [`AgendaError::InvalidTransition`].
    pub fn transition_issue(&mut self, id: &str, status: IssueStatus) -> Result<(), AgendaError> {
        self.get_issue_mut(id)?.transition(status)
    }

    /// Return all issues matching `status`, in insertion order (best-effort).
    pub fn list_issues_by_status(&self, status: &IssueStatus) -> Vec<&Issue> {
        self.issues
            .values()
            .filter(|i| &i.status == status)
            .collect()
    }

    /// Delete an issue by ID.  Returns `true` if the issue existed.
    pub fn delete_issue(&mut self, id: &str) -> bool {
        self.issues.remove(id).is_some()
    }

    // ── Pull Requests ─────────────────────────────────────────────────────────

    /// Open a new pull request and return its ID.
    ///
    /// # Errors
    ///
    /// Propagates [`AgendaError::InvalidField`] from [`PullRequest::new`].
    pub fn open_pr(
        &mut self,
        title: impl Into<String>,
        source_branch: impl Into<String>,
        target_branch: impl Into<String>,
    ) -> Result<String, AgendaError> {
        let pr = PullRequest::new(title, source_branch, target_branch)?;
        let id = pr.id.clone();
        self.pull_requests.insert(id.clone(), pr);
        Ok(id)
    }

    /// Retrieve a pull request by ID.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::NotFound`] when no PR with `id` exists.
    pub fn get_pr(&self, id: &str) -> Result<&PullRequest, AgendaError> {
        self.pull_requests
            .get(id)
            .ok_or_else(|| AgendaError::NotFound(id.to_string()))
    }

    /// Merge a pull request.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::NotFound`] or [`AgendaError::InvalidTransition`]
    /// if the PR is not open.
    pub fn merge_pr(&mut self, id: &str) -> Result<(), AgendaError> {
        let pr = self
            .pull_requests
            .get_mut(id)
            .ok_or_else(|| AgendaError::NotFound(id.to_string()))?;
        if pr.status != PrStatus::Open {
            return Err(AgendaError::InvalidTransition(format!(
                "PR {id} is not open (current: {:?})",
                pr.status
            )));
        }
        pr.status = PrStatus::Merged;
        pr.updated_at = Utc::now();
        Ok(())
    }

    /// Close a pull request without merging.
    ///
    /// # Errors
    ///
    /// Returns [`AgendaError::NotFound`] or [`AgendaError::InvalidTransition`]
    /// if the PR is not open.
    pub fn close_pr(&mut self, id: &str) -> Result<(), AgendaError> {
        let pr = self
            .pull_requests
            .get_mut(id)
            .ok_or_else(|| AgendaError::NotFound(id.to_string()))?;
        if pr.status != PrStatus::Open {
            return Err(AgendaError::InvalidTransition(format!(
                "PR {id} is not open (current: {:?})",
                pr.status
            )));
        }
        pr.status = PrStatus::Closed;
        pr.updated_at = Utc::now();
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_issue_round_trip() {
        let mut mgr = AgendaManager::new();
        let id = mgr.create_issue("title", "desc").unwrap();
        let issue = mgr.get_issue(&id).unwrap();
        assert_eq!(issue.title(), "title");
    }

    #[test]
    fn get_missing_issue_returns_not_found() {
        let mgr = AgendaManager::new();
        assert!(matches!(
            mgr.get_issue("no-such-id"),
            Err(AgendaError::NotFound(_))
        ));
    }

    #[test]
    fn transition_issue_changes_status() {
        let mut mgr = AgendaManager::new();
        let id = mgr.create_issue("t", "d").unwrap();
        mgr.transition_issue(&id, IssueStatus::InProgress).unwrap();
        assert_eq!(mgr.get_issue(&id).unwrap().status, IssueStatus::InProgress);
    }

    #[test]
    fn set_issue_priority_updates_priority() {
        let mut mgr = AgendaManager::new();
        let id = mgr.create_issue("t", "d").unwrap();
        mgr.set_issue_priority(&id, IssuePriority::Critical)
            .unwrap();
        assert_eq!(
            mgr.get_issue(&id).unwrap().priority,
            IssuePriority::Critical
        );
    }

    #[test]
    fn list_issues_by_status_filters_correctly() {
        let mut mgr = AgendaManager::new();
        let id1 = mgr.create_issue("open1", "").unwrap();
        let _id2 = mgr.create_issue("open2", "").unwrap();
        mgr.transition_issue(&id1, IssueStatus::Closed).unwrap();
        let open = mgr.list_issues_by_status(&IssueStatus::Open);
        assert_eq!(open.len(), 1);
        let closed = mgr.list_issues_by_status(&IssueStatus::Closed);
        assert_eq!(closed.len(), 1);
    }

    #[test]
    fn delete_issue_removes_it() {
        let mut mgr = AgendaManager::new();
        let id = mgr.create_issue("t", "d").unwrap();
        assert!(mgr.delete_issue(&id));
        assert!(matches!(mgr.get_issue(&id), Err(AgendaError::NotFound(_))));
    }

    #[test]
    fn open_and_merge_pr() {
        let mut mgr = AgendaManager::new();
        let id = mgr.open_pr("feat: add X", "feature/x", "main").unwrap();
        mgr.merge_pr(&id).unwrap();
        assert_eq!(mgr.get_pr(&id).unwrap().status, PrStatus::Merged);
    }

    #[test]
    fn merge_already_merged_pr_is_rejected() {
        let mut mgr = AgendaManager::new();
        let id = mgr.open_pr("feat", "feat/a", "main").unwrap();
        mgr.merge_pr(&id).unwrap();
        assert!(matches!(
            mgr.merge_pr(&id),
            Err(AgendaError::InvalidTransition(_))
        ));
    }

    #[test]
    fn pr_rejects_same_source_and_target_branch() {
        let mut mgr = AgendaManager::new();
        assert!(matches!(
            mgr.open_pr("feat", "main", "main"),
            Err(AgendaError::InvalidField(_))
        ));
    }

    #[test]
    fn close_pr_sets_closed_status() {
        let mut mgr = AgendaManager::new();
        let id = mgr.open_pr("feat", "feat/b", "main").unwrap();
        mgr.close_pr(&id).unwrap();
        assert_eq!(mgr.get_pr(&id).unwrap().status, PrStatus::Closed);
    }
}
