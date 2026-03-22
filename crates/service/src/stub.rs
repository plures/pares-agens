use crate::error::ServiceError;
use crate::{ServiceInfo, ServiceManager, ServiceStatus};

/// Stub service manager for platforms without a native backend.
pub struct StubServiceManager;

impl StubServiceManager {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for StubServiceManager {
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
            description: "Service management not supported on this platform".to_owned(),
        })
    }

    fn uninstall(&self) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported)
    }
}
