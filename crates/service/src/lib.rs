//! # pares-agens-service
//!
//! Assistant-mode service backend for Pares Agens. Manages the full lifecycle
//! of the agent as a long-running background service using the native service
//! manager for each platform:
//!
//! | Platform | Backend                   |
//! |----------|---------------------------|
//! | Linux    | systemd user unit         |
//! | macOS    | launchd agent             |
//! | Windows  | Windows Service (SCM)     |
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_service::{ServiceManager, ServiceStatus};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let manager = pares_agens_service::platform_manager();
//! let info = manager.status()?;
//! println!("Service is {:?}", info.status);
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod cli;
mod error;
pub mod health;
pub mod ipc;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod stub;
#[cfg(target_os = "windows")]
mod windows;

pub use error::ServiceError;

use serde::{Deserialize, Serialize};

/// Lifecycle state of the background service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    /// Service is actively running.
    Running,
    /// Service is installed but not currently active.
    Stopped,
    /// Service is not installed on this system.
    NotInstalled,
    /// Service installation or state is unknown.
    Unknown,
}

/// Runtime metadata about the installed service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Current lifecycle state.
    pub status: ServiceStatus,
    /// Process ID when the service is running.
    pub pid: Option<u32>,
    /// Human-readable description of the current state.
    pub description: String,
}

/// Platform-agnostic service lifecycle manager.
pub trait ServiceManager: Send + Sync {
    /// Install the service into the platform service manager.
    fn install(&self) -> Result<(), ServiceError>;

    /// Start the installed service.
    fn start(&self) -> Result<(), ServiceError>;

    /// Stop the running service.
    fn stop(&self) -> Result<(), ServiceError>;

    /// Query the current status of the service.
    fn status(&self) -> Result<ServiceInfo, ServiceError>;

    /// Remove the service from the platform service manager.
    fn uninstall(&self) -> Result<(), ServiceError>;
}

/// Return the platform-native [`ServiceManager`] for the current OS.
///
/// On Linux this returns a systemd user-unit manager; on macOS a launchd
/// agent; on Windows the Windows Service Control Manager backend.
pub fn platform_manager() -> Box<dyn ServiceManager> {
    #[cfg(target_os = "linux")]
    return Box::new(linux::LinuxServiceManager::new());

    #[cfg(target_os = "macos")]
    return Box::new(macos::MacosServiceManager::new());

    #[cfg(target_os = "windows")]
    return Box::new(windows::WindowsServiceManager::new());

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Box::new(stub::StubServiceManager::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ServiceStatus ────────────────────────────────────────────────────────

    #[test]
    fn service_status_variants_are_distinct() {
        assert_ne!(ServiceStatus::Running, ServiceStatus::Stopped);
        assert_ne!(ServiceStatus::NotInstalled, ServiceStatus::Unknown);
    }

    #[test]
    fn service_status_clone_and_debug() {
        let s = ServiceStatus::Running;
        let s2 = s.clone();
        assert_eq!(s, s2);
        assert!(!format!("{s:?}").is_empty());
    }

    #[test]
    fn service_status_serde_round_trip() {
        let statuses = [
            ServiceStatus::Running,
            ServiceStatus::Stopped,
            ServiceStatus::NotInstalled,
            ServiceStatus::Unknown,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).expect("serialize");
            let back: ServiceStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*status, back);
        }
    }

    // ── ServiceInfo ──────────────────────────────────────────────────────────

    #[test]
    fn service_info_construction_and_clone() {
        let info = ServiceInfo {
            status: ServiceStatus::Running,
            pid: Some(1234),
            description: "running fine".to_string(),
        };
        let info2 = info.clone();
        assert_eq!(info.pid, info2.pid);
        assert_eq!(info.description, info2.description);
    }

    #[test]
    fn service_info_serde_round_trip() {
        let info = ServiceInfo {
            status: ServiceStatus::Stopped,
            pid: None,
            description: "stopped".to_string(),
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let back: ServiceInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.status, ServiceStatus::Stopped);
        assert!(back.pid.is_none());
        assert_eq!(back.description, "stopped");
    }

    // ── platform_manager ────────────────────────────────────────────────────

    #[test]
    fn platform_manager_returns_a_manager() {
        // Just verify it constructs without panicking.
        let _mgr = platform_manager();
    }
}
