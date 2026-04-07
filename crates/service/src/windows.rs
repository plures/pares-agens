//! # Windows Service Manager
//!
//! Manages the Pares Agens background service through the Windows Service
//! Control Manager (SCM).
//!
//! ## Service Start Type
//!
//! The service is registered with `ServiceStartType::AutoStart`, meaning it
//! starts automatically when Windows boots.  If on-demand startup is
//! preferred, modify the registry entry after installation or use the Services
//! GUI (`services.msc`) to change the startup type to **Manual**.
//!
//! ## Required Permissions
//!
//! Most lifecycle operations require **Administrator** (elevated) privileges.
//! The table below lists the minimum rights needed for each operation:
//!
//! | Operation   | SCM right required          | Service right required        |
//! |-------------|-----------------------------|-------------------------------|
//! | `install`   | `SC_MANAGER_CREATE_SERVICE` | —                             |
//! | `start`     | `SC_MANAGER_CONNECT`        | `SERVICE_START`               |
//! | `stop`      | `SC_MANAGER_CONNECT`        | `SERVICE_STOP`                |
//! | `status`    | `SC_MANAGER_CONNECT`        | `SERVICE_QUERY_STATUS`        |
//! | `uninstall` | `SC_MANAGER_CONNECT`        | `SERVICE_STOP` + `DELETE`     |
//!
//! Querying service **status** does not require elevation — all authenticated
//! users have `SERVICE_QUERY_STATUS` by default.
//!
//! ## Install Steps
//!
//! 1. Build the release binary:
//!    ```text
//!    cargo build --release --target x86_64-pc-windows-msvc
//!    ```
//! 2. Copy `pares-agens.exe` to a permanent location, e.g.
//!    `C:\Program Files\PareAgens\pares-agens.exe`.
//! 3. Open an **elevated** PowerShell or Command Prompt.
//! 4. Install the service:
//!    ```text
//!    pares-agens service install
//!    ```
//! 5. Start the service:
//!    ```text
//!    pares-agens service start
//!    ```
//! 6. Verify it is running:
//!    ```text
//!    pares-agens service status
//!    ```
//!
//! To remove the service (also requires elevation):
//! ```text
//! pares-agens service uninstall
//! ```
//!
//! You can also manage the service through the Windows Services GUI
//! (`services.msc`) or PowerShell (`Get-Service`, `Start-Service`,
//! `Stop-Service`).

use crate::error::ServiceError;
use crate::{ServiceInfo, ServiceManager, ServiceStatus};
use std::ffi::OsString;
use std::path::PathBuf;
use windows_service::{
    service::{
        ServiceAccess, ServiceErrorControl, ServiceInfo as WinServiceInfo, ServiceStartType,
        ServiceState, ServiceType,
    },
    service_manager::{ServiceManager as WinScm, ServiceManagerAccess},
};

/// Windows service name as registered with the SCM.
const SERVICE_NAME: &str = "ParesAgens";

/// Human-readable display name shown in `services.msc`.
const SERVICE_DISPLAY_NAME: &str = "Pares Agens AI Assistant";

// ── Win32 error codes ────────────────────────────────────────────────────────

/// `ERROR_SERVICE_DOES_NOT_EXIST` — service not present in the SCM database.
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
/// `ERROR_SERVICE_EXISTS` — service is already registered.
const ERROR_SERVICE_EXISTS: i32 = 1073;

// ── SCM abstraction ──────────────────────────────────────────────────────────

/// Abstracts all Windows SCM calls so that [`WindowsServiceManager`] can be
/// unit-tested with a [`MockScmBackend`] without touching the real SCM.
pub(crate) trait ScmBackend: Send + Sync {
    /// Return the current service state from the SCM.
    fn query_state(&self) -> Result<ServiceInfo, ServiceError>;

    /// Create a new Windows service entry pointing at `exec_path`.
    fn do_install(&self, exec_path: &str) -> Result<(), ServiceError>;

    /// Send the `SERVICE_CONTROL_START` command.
    fn do_start(&self) -> Result<(), ServiceError>;

    /// Send the `SERVICE_CONTROL_STOP` command.
    fn do_stop(&self) -> Result<(), ServiceError>;

    /// Delete the service from the SCM database (stops it first if running).
    fn do_uninstall(&self) -> Result<(), ServiceError>;
}

// ── Real Windows SCM backend ─────────────────────────────────────────────────

/// Backend that calls the real Windows Service Control Manager.
struct RealScmBackend;

/// Convert a `windows_service::Error` into our [`ServiceError`].
fn map_err(e: windows_service::Error) -> ServiceError {
    match e {
        windows_service::Error::Winapi(io_err) => ServiceError::CommandFailed {
            status: io_err.raw_os_error().unwrap_or(-1),
            message: io_err.to_string(),
        },
        other => ServiceError::CommandFailed {
            status: -1,
            message: other.to_string(),
        },
    }
}

fn is_not_found(e: &windows_service::Error) -> bool {
    matches!(
        e,
        windows_service::Error::Winapi(io_err)
        if io_err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST)
    )
}

fn is_already_exists(e: &windows_service::Error) -> bool {
    matches!(
        e,
        windows_service::Error::Winapi(io_err)
        if io_err.raw_os_error() == Some(ERROR_SERVICE_EXISTS)
    )
}

impl ScmBackend for RealScmBackend {
    fn query_state(&self) -> Result<ServiceInfo, ServiceError> {
        let scm =
            WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(map_err)?;

        let service = match scm.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
            Ok(s) => s,
            Err(ref e) if is_not_found(e) => {
                return Ok(ServiceInfo {
                    status: ServiceStatus::NotInstalled,
                    pid: None,
                    description: "Service is not installed".to_owned(),
                });
            }
            Err(e) => return Err(map_err(e)),
        };

        let win_status = service.query_status().map_err(map_err)?;

        let (status, description) = match win_status.current_state {
            ServiceState::Running => (ServiceStatus::Running, "Service is running".to_owned()),
            ServiceState::Stopped => (ServiceStatus::Stopped, "Service is stopped".to_owned()),
            // Transitional states: map to the nearest stable state so that
            // callers do not attempt conflicting control operations while the
            // SCM is in the middle of a state change.  A `StopPending` service
            // is treated as `Stopped` (prevents issuing a duplicate `stop()`),
            // and a `StartPending` service is treated as `Running` (prevents
            // issuing a duplicate `start()`).
            ServiceState::StopPending => (ServiceStatus::Stopped, "Service is stopping".to_owned()),
            ServiceState::StartPending => {
                (ServiceStatus::Running, "Service is starting".to_owned())
            }
            // Paused services cannot be started without first being resumed;
            // `Stopped` is the closest available variant in the current enum.
            ServiceState::Paused | ServiceState::PausePending | ServiceState::ContinuePending => {
                (ServiceStatus::Stopped, "Service is paused".to_owned())
            }
        };

        Ok(ServiceInfo {
            pid: if status == ServiceStatus::Running {
                win_status.process_id
            } else {
                None
            },
            status,
            description,
        })
    }

    fn do_install(&self, exec_path: &str) -> Result<(), ServiceError> {
        let scm = WinScm::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )
        .map_err(map_err)?;

        let info = WinServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: PathBuf::from(exec_path),
            launch_arguments: vec![],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };

        match scm.create_service(&info, ServiceAccess::ALL_ACCESS) {
            Ok(_) => Ok(()),
            Err(ref e) if is_already_exists(e) => Err(ServiceError::AlreadyInstalled),
            Err(e) => Err(map_err(e)),
        }
    }

    fn do_start(&self) -> Result<(), ServiceError> {
        let scm =
            WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(map_err)?;
        let service = scm
            .open_service(SERVICE_NAME, ServiceAccess::START)
            .map_err(map_err)?;
        let no_args: Vec<OsString> = vec![];
        service.start(&no_args).map_err(map_err)?;
        Ok(())
    }

    fn do_stop(&self) -> Result<(), ServiceError> {
        let scm =
            WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(map_err)?;
        let service = scm
            .open_service(SERVICE_NAME, ServiceAccess::STOP)
            .map_err(map_err)?;
        service.stop().map_err(map_err)?;
        Ok(())
    }

    fn do_uninstall(&self) -> Result<(), ServiceError> {
        let scm =
            WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(map_err)?;
        let service = scm
            .open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE)
            .map_err(map_err)?;
        // Attempt to stop first; ignore errors (service may already be stopped).
        let _ = service.stop();
        service.delete().map_err(map_err)?;
        Ok(())
    }
}

// ── WindowsServiceManager ────────────────────────────────────────────────────

/// Manages the Pares Agens service via the Windows Service Control Manager.
///
/// See the [module-level documentation](self) for required permissions and
/// step-by-step installation instructions.
pub struct WindowsServiceManager {
    exec_path: String,
    backend: Box<dyn ScmBackend>,
}

impl WindowsServiceManager {
    /// Create a new manager backed by the real Windows SCM.
    ///
    /// The service binary path defaults to the current executable.
    pub fn new() -> Self {
        Self {
            exec_path: std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "pares-agens.exe".to_owned()),
            backend: Box::new(RealScmBackend),
        }
    }

    /// Construct with an injected [`ScmBackend`] (used in unit tests).
    #[cfg(test)]
    fn with_backend(exec_path: impl Into<String>, backend: Box<dyn ScmBackend>) -> Self {
        Self {
            exec_path: exec_path.into(),
            backend,
        }
    }
}

impl Default for WindowsServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for WindowsServiceManager {
    fn install(&self) -> Result<(), ServiceError> {
        let info = self.backend.query_state()?;
        if info.status != ServiceStatus::NotInstalled {
            return Err(ServiceError::AlreadyInstalled);
        }
        self.backend.do_install(&self.exec_path)
    }

    fn start(&self) -> Result<(), ServiceError> {
        let info = self.backend.query_state()?;
        match info.status {
            ServiceStatus::NotInstalled => Err(ServiceError::NotInstalled),
            ServiceStatus::Running => Err(ServiceError::AlreadyRunning),
            _ => self.backend.do_start(),
        }
    }

    fn stop(&self) -> Result<(), ServiceError> {
        let info = self.backend.query_state()?;
        if info.status != ServiceStatus::Running {
            return Err(ServiceError::NotRunning);
        }
        self.backend.do_stop()
    }

    fn status(&self) -> Result<ServiceInfo, ServiceError> {
        self.backend.query_state()
    }

    fn uninstall(&self) -> Result<(), ServiceError> {
        let info = self.backend.query_state()?;
        if info.status == ServiceStatus::NotInstalled {
            return Err(ServiceError::NotInstalled);
        }
        self.backend.do_uninstall()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ── Mock SCM backend ─────────────────────────────────────────────────────

    struct MockState {
        installed: bool,
        running: bool,
        pid: Option<u32>,
    }

    impl MockState {
        fn not_installed() -> Self {
            Self {
                installed: false,
                running: false,
                pid: None,
            }
        }

        fn stopped() -> Self {
            Self {
                installed: true,
                running: false,
                pid: None,
            }
        }

        fn running() -> Self {
            Self {
                installed: true,
                running: true,
                pid: Some(1234),
            }
        }
    }

    struct MockScm(Mutex<MockState>);

    impl MockScm {
        fn new(state: MockState) -> Self {
            Self(Mutex::new(state))
        }
    }

    impl ScmBackend for MockScm {
        fn query_state(&self) -> Result<ServiceInfo, ServiceError> {
            let s = self.0.lock().unwrap();
            Ok(if !s.installed {
                ServiceInfo {
                    status: ServiceStatus::NotInstalled,
                    pid: None,
                    description: "not installed".to_owned(),
                }
            } else if s.running {
                ServiceInfo {
                    status: ServiceStatus::Running,
                    pid: s.pid,
                    description: "running".to_owned(),
                }
            } else {
                ServiceInfo {
                    status: ServiceStatus::Stopped,
                    pid: None,
                    description: "stopped".to_owned(),
                }
            })
        }

        fn do_install(&self, _exec_path: &str) -> Result<(), ServiceError> {
            let mut s = self.0.lock().unwrap();
            s.installed = true;
            Ok(())
        }

        fn do_start(&self) -> Result<(), ServiceError> {
            let mut s = self.0.lock().unwrap();
            s.running = true;
            s.pid = Some(5678);
            Ok(())
        }

        fn do_stop(&self) -> Result<(), ServiceError> {
            let mut s = self.0.lock().unwrap();
            s.running = false;
            s.pid = None;
            Ok(())
        }

        fn do_uninstall(&self) -> Result<(), ServiceError> {
            let mut s = self.0.lock().unwrap();
            s.installed = false;
            s.running = false;
            s.pid = None;
            Ok(())
        }
    }

    // ── Constructor / constants ───────────────────────────────────────────────

    #[test]
    fn new_returns_valid_manager() {
        let mgr = WindowsServiceManager::new();
        assert!(!mgr.exec_path.is_empty());
    }

    #[test]
    fn default_uses_new() {
        let mgr = WindowsServiceManager::default();
        assert!(!mgr.exec_path.is_empty());
    }

    #[test]
    fn service_name_is_non_empty() {
        assert!(!SERVICE_NAME.is_empty());
    }

    #[test]
    fn service_display_name_is_non_empty() {
        assert!(!SERVICE_DISPLAY_NAME.is_empty());
    }

    // ── install ───────────────────────────────────────────────────────────────

    #[test]
    fn install_succeeds_when_not_installed() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::not_installed())),
        );
        assert!(mgr.install().is_ok());
    }

    #[test]
    fn install_returns_already_installed_when_service_exists() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::stopped())),
        );
        assert!(matches!(mgr.install(), Err(ServiceError::AlreadyInstalled)));
    }

    #[test]
    fn install_returns_already_installed_when_service_is_running() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::running())),
        );
        assert!(matches!(mgr.install(), Err(ServiceError::AlreadyInstalled)));
    }

    // ── start ─────────────────────────────────────────────────────────────────

    #[test]
    fn start_succeeds_when_stopped() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::stopped())),
        );
        assert!(mgr.start().is_ok());
    }

    #[test]
    fn start_returns_not_installed_when_not_installed() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::not_installed())),
        );
        assert!(matches!(mgr.start(), Err(ServiceError::NotInstalled)));
    }

    #[test]
    fn start_returns_already_running_when_running() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::running())),
        );
        assert!(matches!(mgr.start(), Err(ServiceError::AlreadyRunning)));
    }

    // ── stop ──────────────────────────────────────────────────────────────────

    #[test]
    fn stop_succeeds_when_running() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::running())),
        );
        assert!(mgr.stop().is_ok());
    }

    #[test]
    fn stop_returns_not_running_when_stopped() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::stopped())),
        );
        assert!(matches!(mgr.stop(), Err(ServiceError::NotRunning)));
    }

    #[test]
    fn stop_returns_not_running_when_not_installed() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::not_installed())),
        );
        assert!(matches!(mgr.stop(), Err(ServiceError::NotRunning)));
    }

    // ── status ────────────────────────────────────────────────────────────────

    #[test]
    fn status_reflects_not_installed_state() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::not_installed())),
        );
        let info = mgr.status().unwrap();
        assert_eq!(info.status, ServiceStatus::NotInstalled);
        assert!(info.pid.is_none());
    }

    #[test]
    fn status_reflects_running_state_with_pid() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::running())),
        );
        let info = mgr.status().unwrap();
        assert_eq!(info.status, ServiceStatus::Running);
        assert_eq!(info.pid, Some(1234));
    }

    #[test]
    fn status_reflects_stopped_state() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::stopped())),
        );
        let info = mgr.status().unwrap();
        assert_eq!(info.status, ServiceStatus::Stopped);
        assert!(info.pid.is_none());
    }

    // ── uninstall ─────────────────────────────────────────────────────────────

    #[test]
    fn uninstall_succeeds_when_stopped() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::stopped())),
        );
        assert!(mgr.uninstall().is_ok());
    }

    #[test]
    fn uninstall_succeeds_when_running() {
        // The real SCM backend stops the service before deleting it.
        // The mock simulates the same behaviour.
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::running())),
        );
        assert!(mgr.uninstall().is_ok());
    }

    #[test]
    fn uninstall_returns_not_installed_when_not_registered() {
        let mgr = WindowsServiceManager::with_backend(
            "C:\\test\\pares-agens.exe",
            Box::new(MockScm::new(MockState::not_installed())),
        );
        assert!(matches!(mgr.uninstall(), Err(ServiceError::NotInstalled)));
    }
}
