use crate::error::ServiceError;
use crate::{ServiceInfo, ServiceManager, ServiceStatus};

/// Windows Service Manager stub.
///
/// Full Windows Service integration requires the `windows-service` crate and
/// a dedicated Windows build target. This stub satisfies the trait contract
/// and allows the crate to compile cross-platform while the Windows backend
/// is developed.
pub struct WindowsServiceManager;

impl WindowsServiceManager {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for WindowsServiceManager {
    fn install(&self) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn start(&self) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn status(&self) -> Result<ServiceInfo, ServiceError> {
        Ok(ServiceInfo {
            status: ServiceStatus::Unknown,
            pid: None,
            description: "Windows Service backend not yet implemented".to_owned(),
        })
    }

    fn uninstall(&self) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_returns_unsupported() {
        let mgr = WindowsServiceManager::new();
        assert!(matches!(mgr.install(), Err(ServiceError::Unsupported)));
    }

    #[test]
    fn status_returns_unknown() {
        let mgr = WindowsServiceManager::new();
        let info = mgr.status().unwrap();
        assert_eq!(info.status, ServiceStatus::Unknown);
        assert!(info.pid.is_none());
    }
}
