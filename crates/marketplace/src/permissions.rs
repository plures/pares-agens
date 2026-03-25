//! Permission declarations for sandboxed marketplace procedures.
//!
//! [`Permission`] describes a single capability a procedure requires.
//! [`PermissionSet`] aggregates permissions declared by a procedure manifest.
//! [`PermissionGuard`] enforces that only declared permissions are exercised at
//! runtime.
//!
//! # Sandboxing model
//!
//! Before a procedure is executed the runtime calls
//! [`PermissionGuard::check`] with the [`Permission`] the execution step
//! requires.  If the permission was not declared in the procedure's
//! [`PermissionSet`] the guard returns
//! [`MarketplaceError::SecurityViolation`] and execution is aborted.
//!
//! This provides a capability-based, deny-by-default sandbox without
//! requiring OS-level containers or separate processes.

use crate::MarketplaceError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ── Permission ────────────────────────────────────────────────────────────────

/// A discrete capability that a marketplace procedure may require.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// Read access to the local filesystem (scoped to the install directory).
    FileSystemRead,
    /// Write access to the local filesystem (scoped to the install directory).
    FileSystemWrite,
    /// Outbound HTTP/HTTPS network requests.
    NetworkFetch,
    /// Access to environment variables.
    Environment,
    /// Access to the local PluresDB instance.
    Database,
    /// Spawn sub-processes.
    Process,
    /// Access the system clipboard.
    Clipboard,
    /// Display notifications.
    Notification,
    /// A named custom permission defined by the procedure author.
    Custom(String),
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileSystemRead => write!(f, "filesystem:read"),
            Self::FileSystemWrite => write!(f, "filesystem:write"),
            Self::NetworkFetch => write!(f, "network:fetch"),
            Self::Environment => write!(f, "environment"),
            Self::Database => write!(f, "database"),
            Self::Process => write!(f, "process"),
            Self::Clipboard => write!(f, "clipboard"),
            Self::Notification => write!(f, "notification"),
            Self::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

// ── PermissionSet ─────────────────────────────────────────────────────────────

/// The complete set of permissions declared by an installed procedure.
///
/// Build one from a list of [`Permission`] values with
/// [`PermissionSet::from_permissions`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionSet {
    granted: HashSet<Permission>,
}

impl PermissionSet {
    /// Create an empty (deny-all) [`PermissionSet`].
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create a [`PermissionSet`] from an iterator of [`Permission`] values.
    #[must_use]
    pub fn from_permissions(permissions: impl IntoIterator<Item = Permission>) -> Self {
        Self {
            granted: permissions.into_iter().collect(),
        }
    }

    /// Return `true` when `permission` has been granted.
    #[must_use]
    pub fn contains(&self, permission: &Permission) -> bool {
        self.granted.contains(permission)
    }

    /// Return the number of granted permissions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.granted.len()
    }

    /// Return `true` when no permissions have been granted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }

    /// Return an iterator over all granted permissions.
    pub fn iter(&self) -> impl Iterator<Item = &Permission> {
        self.granted.iter()
    }
}

// ── PermissionGuard ───────────────────────────────────────────────────────────

/// Runtime enforcer for a procedure's declared [`PermissionSet`].
///
/// Wrap the permission set from a procedure's manifest in a `PermissionGuard`
/// and call [`check`](Self::check) before each capability-gated operation.
#[derive(Debug)]
pub struct PermissionGuard {
    granted: PermissionSet,
}

impl PermissionGuard {
    /// Create a new guard from a declared [`PermissionSet`].
    #[must_use]
    pub fn new(granted: PermissionSet) -> Self {
        Self { granted }
    }

    /// Assert that `permission` was declared by the procedure.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::SecurityViolation`] when `permission` was
    /// not declared in the granted set.
    pub fn check(&self, permission: &Permission) -> Result<(), MarketplaceError> {
        if self.granted.contains(permission) {
            Ok(())
        } else {
            Err(MarketplaceError::SecurityViolation(format!(
                "permission '{permission}' was not declared by this procedure"
            )))
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn read_write_set() -> PermissionSet {
        PermissionSet::from_permissions([Permission::FileSystemRead, Permission::FileSystemWrite])
    }

    // ── Permission::Display ───────────────────────────────────────────────────

    #[test]
    fn display_filesystem_read() {
        assert_eq!(Permission::FileSystemRead.to_string(), "filesystem:read");
    }

    #[test]
    fn display_custom_permission() {
        assert_eq!(
            Permission::Custom("my-perm".to_string()).to_string(),
            "custom:my-perm"
        );
    }

    // ── PermissionSet ─────────────────────────────────────────────────────────

    #[test]
    fn empty_set_grants_nothing() {
        let set = PermissionSet::empty();
        assert!(set.is_empty());
        assert!(!set.contains(&Permission::NetworkFetch));
    }

    #[test]
    fn from_permissions_contains_all_granted() {
        let set = read_write_set();
        assert!(set.contains(&Permission::FileSystemRead));
        assert!(set.contains(&Permission::FileSystemWrite));
        assert!(!set.contains(&Permission::NetworkFetch));
    }

    #[test]
    fn len_reflects_granted_count() {
        let set = read_write_set();
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn iter_yields_all_permissions() {
        let set = read_write_set();
        let permissions: Vec<_> = set.iter().collect();
        assert_eq!(permissions.len(), 2);
    }

    // ── PermissionGuard ───────────────────────────────────────────────────────

    #[test]
    fn check_passes_for_granted_permission() {
        let guard = PermissionGuard::new(read_write_set());
        assert!(guard.check(&Permission::FileSystemRead).is_ok());
    }

    #[test]
    fn check_fails_for_undeclared_permission() {
        let guard = PermissionGuard::new(read_write_set());
        assert!(matches!(
            guard.check(&Permission::NetworkFetch),
            Err(MarketplaceError::SecurityViolation(_))
        ));
    }

    #[test]
    fn deny_all_guard_rejects_everything() {
        let guard = PermissionGuard::new(PermissionSet::empty());
        assert!(matches!(
            guard.check(&Permission::Database),
            Err(MarketplaceError::SecurityViolation(_))
        ));
    }

    #[test]
    fn custom_permission_can_be_granted_and_checked() {
        let perm = Permission::Custom("upload-logs".to_string());
        let set = PermissionSet::from_permissions([perm.clone()]);
        let guard = PermissionGuard::new(set);
        assert!(guard.check(&perm).is_ok());
    }

    #[test]
    fn check_error_message_names_the_permission() {
        let guard = PermissionGuard::new(PermissionSet::empty());
        let err = guard.check(&Permission::NetworkFetch).unwrap_err();
        assert!(err.to_string().contains("network:fetch"));
    }
}
