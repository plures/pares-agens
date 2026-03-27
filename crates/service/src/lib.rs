//! # pares-agens-service
//!
//! Assistant-mode service backend for Pares Agens. Manages the full lifecycle
//! of the agent as a long-running background service using the native service
//! manager for each platform:
//!
//! | Platform | Backend           |
//! |----------|-------------------|
//! | Linux    | systemd user unit |
//! | macOS    | launchd agent     |
//! | Windows  | stub (future)     |
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
/// agent; on Windows a stub that returns [`ServiceError::Unsupported`].
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
