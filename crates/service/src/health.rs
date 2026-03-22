//! Health endpoint — lightweight HTTP/socket health check for the service.
//!
//! The health server binds on a local Unix socket or named pipe and answers
//! [`HealthStatus`] queries. External monitors and the Tauri front-end use
//! this to decide whether to start the embedded agent fallback.

use crate::error::ServiceError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Reported liveness level of the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthLevel {
    /// All subsystems are running normally.
    Ok,
    /// One or more subsystems are degraded but the service is still running.
    Degraded,
    /// The service is not operating correctly.
    Critical,
}

/// Snapshot of the service's health at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    /// Overall health level.
    pub level: HealthLevel,
    /// Human-readable summary.
    pub message: String,
    /// Service uptime in seconds, if available.
    pub uptime_secs: Option<u64>,
    /// Individual subsystem statuses (name → ok/degraded).
    pub subsystems: Vec<SubsystemStatus>,
}

/// Health of a single named subsystem within the service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemStatus {
    /// Subsystem name (e.g. "memory", "cerebellum", "channels").
    pub name: String,
    /// Whether the subsystem is operating normally.
    pub healthy: bool,
    /// Optional detail about the subsystem's state.
    pub detail: Option<String>,
}

impl HealthStatus {
    /// Create a fully healthy status with the given uptime.
    pub fn ok(uptime_secs: Option<u64>) -> Self {
        Self {
            level: HealthLevel::Ok,
            message: "All systems operational".to_owned(),
            uptime_secs,
            subsystems: Vec::new(),
        }
    }

    /// Create a critical status with an error message.
    pub fn critical(message: impl Into<String>) -> Self {
        Self {
            level: HealthLevel::Critical,
            message: message.into(),
            uptime_secs: None,
            subsystems: Vec::new(),
        }
    }

    /// Derive the overall [`HealthLevel`] from the subsystem list.
    pub fn derive_level(subsystems: Vec<SubsystemStatus>) -> HealthLevel {
        let any_unhealthy = subsystems.iter().any(|s| !s.healthy);
        if any_unhealthy { HealthLevel::Degraded } else { HealthLevel::Ok }
    }
}

/// Configuration for the embedded health server.
#[derive(Debug, Clone)]
pub struct HealthServerConfig {
    /// Interval between automatic health check refreshes.
    pub refresh_interval: Duration,
    /// Maximum number of queued health requests.
    pub backlog: usize,
}

impl Default for HealthServerConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(30),
            backlog: 16,
        }
    }
}

/// Lightweight health-check server embedded in the service process.
///
/// The server maintains a cached [`HealthStatus`] that is refreshed
/// periodically and served on demand to any connecting client.
pub struct HealthServer {
    config: HealthServerConfig,
    current_status: std::sync::Mutex<HealthStatus>,
}

impl HealthServer {
    /// Create a new health server with the provided configuration.
    pub fn new(config: HealthServerConfig) -> Self {
        Self {
            config,
            current_status: std::sync::Mutex::new(HealthStatus::ok(None)),
        }
    }

    /// Return the most recently cached health status.
    pub fn get_status(&self) -> Result<HealthStatus, ServiceError> {
        let guard = self
            .current_status
            .lock()
            .map_err(|_| ServiceError::Ipc("health lock poisoned".to_owned()))?;
        Ok(guard.clone())
    }

    /// Update the cached health status.
    pub fn set_status(&self, status: HealthStatus) -> Result<(), ServiceError> {
        let mut guard = self
            .current_status
            .lock()
            .map_err(|_| ServiceError::Ipc("health lock poisoned".to_owned()))?;
        *guard = status;
        Ok(())
    }

    /// Return the refresh interval configured for this server.
    pub fn refresh_interval(&self) -> Duration {
        self.config.refresh_interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_status_has_ok_level() {
        let status = HealthStatus::ok(Some(3600));
        assert_eq!(status.level, HealthLevel::Ok);
        assert_eq!(status.uptime_secs, Some(3600));
    }

    #[test]
    fn critical_status_has_critical_level() {
        let status = HealthStatus::critical("database connection lost");
        assert_eq!(status.level, HealthLevel::Critical);
        assert!(status.message.contains("database"));
    }

    #[test]
    fn derive_level_ok_when_all_healthy() {
        let subsystems = vec![
            SubsystemStatus { name: "memory".into(), healthy: true, detail: None },
            SubsystemStatus { name: "cerebellum".into(), healthy: true, detail: None },
        ];
        assert_eq!(HealthStatus::derive_level(subsystems), HealthLevel::Ok);
    }

    #[test]
    fn derive_level_degraded_when_any_unhealthy() {
        let subsystems = vec![
            SubsystemStatus { name: "memory".into(), healthy: true, detail: None },
            SubsystemStatus { name: "channels".into(), healthy: false, detail: Some("timeout".into()) },
        ];
        assert_eq!(HealthStatus::derive_level(subsystems), HealthLevel::Degraded);
    }

    #[test]
    fn health_server_get_set_roundtrip() {
        let server = HealthServer::new(HealthServerConfig::default());
        let updated = HealthStatus::critical("test failure");
        server.set_status(updated.clone()).unwrap();
        let retrieved = server.get_status().unwrap();
        assert_eq!(retrieved.level, HealthLevel::Critical);
        assert_eq!(retrieved.message, "test failure");
    }

    #[test]
    fn default_config_has_reasonable_interval() {
        let config = HealthServerConfig::default();
        assert!(config.refresh_interval.as_secs() > 0);
        assert!(config.backlog > 0);
    }
}
