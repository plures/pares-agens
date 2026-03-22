//! CLI module — service sub-commands for the `pares-agens` binary.
//!
//! Adds an `assistant` (or `service`) sub-command group to the CLI:
//!
//! ```text
//! pares-agens service install
//! pares-agens service start
//! pares-agens service stop
//! pares-agens service status
//! pares-agens service uninstall
//! ```

use crate::error::ServiceError;
use crate::ServiceManager;
use std::str::FromStr;

/// Sub-commands available under the `service` CLI group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceCommand {
    /// Install the service into the platform service manager.
    Install,
    /// Start the installed service.
    Start,
    /// Stop the running service.
    Stop,
    /// Display the current service status.
    Status,
    /// Remove the service from the platform service manager.
    Uninstall,
}

/// Error returned when an unknown service command string is provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand(pub String);

impl std::fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown service command: '{}'", self.0)
    }
}

impl std::error::Error for UnknownCommand {}

impl FromStr for ServiceCommand {
    type Err = UnknownCommand;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "install" => Ok(Self::Install),
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            "status" => Ok(Self::Status),
            "uninstall" => Ok(Self::Uninstall),
            _ => Err(UnknownCommand(s.to_owned())),
        }
    }
}

impl ServiceCommand {
    /// Return a human-readable name for the command.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Status => "status",
            Self::Uninstall => "uninstall",
        }
    }
}

/// Dispatch a [`ServiceCommand`] against the provided [`ServiceManager`].
///
/// Writes a human-readable result to `stdout` and returns any error that
/// occurred during execution.
pub fn run_cli(cmd: &ServiceCommand, manager: &dyn ServiceManager) -> Result<(), ServiceError> {
    match cmd {
        ServiceCommand::Install => {
            manager.install()?;
            println!("✓ Service installed successfully.");
        }
        ServiceCommand::Start => {
            manager.start()?;
            println!("✓ Service started.");
        }
        ServiceCommand::Stop => {
            manager.stop()?;
            println!("✓ Service stopped.");
        }
        ServiceCommand::Status => {
            let info = manager.status()?;
            println!("Status:  {:?}", info.status);
            if let Some(pid) = info.pid {
                println!("PID:     {pid}");
            }
            println!("Details: {}", info.description);
        }
        ServiceCommand::Uninstall => {
            manager.uninstall()?;
            println!("✓ Service uninstalled.");
        }
    }
    Ok(())
}

/// Print usage information for the service CLI group.
pub fn print_usage() {
    println!("Usage: pares-agens service <COMMAND>");
    println!();
    println!("Commands:");
    println!("  install    Install the service into the platform service manager");
    println!("  start      Start the installed service");
    println!("  stop       Stop the running service");
    println!("  status     Display the current service status");
    println!("  uninstall  Remove the service from the platform service manager");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ServiceError, ServiceInfo, ServiceManager, ServiceStatus};

    struct MockManager {
        status: ServiceStatus,
    }

    impl MockManager {
        fn running() -> Self {
            Self { status: ServiceStatus::Running }
        }
        fn stopped() -> Self {
            Self { status: ServiceStatus::Stopped }
        }
        fn not_installed() -> Self {
            Self { status: ServiceStatus::NotInstalled }
        }
    }

    impl ServiceManager for MockManager {
        fn install(&self) -> Result<(), ServiceError> {
            if self.status == ServiceStatus::NotInstalled {
                Ok(())
            } else {
                Err(ServiceError::AlreadyInstalled)
            }
        }
        fn start(&self) -> Result<(), ServiceError> {
            if self.status == ServiceStatus::Stopped {
                Ok(())
            } else if self.status == ServiceStatus::Running {
                Err(ServiceError::AlreadyRunning)
            } else {
                Err(ServiceError::NotInstalled)
            }
        }
        fn stop(&self) -> Result<(), ServiceError> {
            if self.status == ServiceStatus::Running {
                Ok(())
            } else {
                Err(ServiceError::NotRunning)
            }
        }
        fn status(&self) -> Result<ServiceInfo, ServiceError> {
            Ok(ServiceInfo {
                status: self.status.clone(),
                pid: if self.status == ServiceStatus::Running { Some(1234) } else { None },
                description: format!("{:?}", self.status),
            })
        }
        fn uninstall(&self) -> Result<(), ServiceError> {
            if self.status != ServiceStatus::NotInstalled {
                Ok(())
            } else {
                Err(ServiceError::NotInstalled)
            }
        }
    }

    #[test]
    fn from_str_parses_all_commands() {
        assert!(matches!("install".parse::<ServiceCommand>(), Ok(ServiceCommand::Install)));
        assert!(matches!("start".parse::<ServiceCommand>(), Ok(ServiceCommand::Start)));
        assert!(matches!("stop".parse::<ServiceCommand>(), Ok(ServiceCommand::Stop)));
        assert!(matches!("status".parse::<ServiceCommand>(), Ok(ServiceCommand::Status)));
        assert!(matches!("uninstall".parse::<ServiceCommand>(), Ok(ServiceCommand::Uninstall)));
    }

    #[test]
    fn from_str_case_insensitive() {
        assert!(matches!("INSTALL".parse::<ServiceCommand>(), Ok(ServiceCommand::Install)));
        assert!(matches!("Status".parse::<ServiceCommand>(), Ok(ServiceCommand::Status)));
    }

    #[test]
    fn from_str_returns_err_for_unknown() {
        assert!("restart".parse::<ServiceCommand>().is_err());
        assert!("".parse::<ServiceCommand>().is_err());
    }

    #[test]
    fn name_matches_from_str_input() {
        let commands = [
            ServiceCommand::Install,
            ServiceCommand::Start,
            ServiceCommand::Stop,
            ServiceCommand::Status,
            ServiceCommand::Uninstall,
        ];
        for cmd in &commands {
            let parsed = cmd.name().parse::<ServiceCommand>();
            assert!(parsed.is_ok(), "parse({}) should succeed", cmd.name());
        }
    }

    #[test]
    fn run_cli_install_ok_when_not_installed() {
        let mgr = MockManager::not_installed();
        assert!(run_cli(&ServiceCommand::Install, &mgr).is_ok());
    }

    #[test]
    fn run_cli_install_fails_when_already_installed() {
        let mgr = MockManager::stopped();
        assert!(matches!(
            run_cli(&ServiceCommand::Install, &mgr),
            Err(ServiceError::AlreadyInstalled)
        ));
    }

    #[test]
    fn run_cli_start_ok_when_stopped() {
        let mgr = MockManager::stopped();
        assert!(run_cli(&ServiceCommand::Start, &mgr).is_ok());
    }

    #[test]
    fn run_cli_start_fails_when_already_running() {
        let mgr = MockManager::running();
        assert!(matches!(
            run_cli(&ServiceCommand::Start, &mgr),
            Err(ServiceError::AlreadyRunning)
        ));
    }

    #[test]
    fn run_cli_stop_ok_when_running() {
        let mgr = MockManager::running();
        assert!(run_cli(&ServiceCommand::Stop, &mgr).is_ok());
    }

    #[test]
    fn run_cli_stop_fails_when_not_running() {
        let mgr = MockManager::stopped();
        assert!(matches!(
            run_cli(&ServiceCommand::Stop, &mgr),
            Err(ServiceError::NotRunning)
        ));
    }

    #[test]
    fn run_cli_status_returns_info() {
        let mgr = MockManager::running();
        assert!(run_cli(&ServiceCommand::Status, &mgr).is_ok());
    }

    #[test]
    fn run_cli_uninstall_ok_when_installed() {
        let mgr = MockManager::stopped();
        assert!(run_cli(&ServiceCommand::Uninstall, &mgr).is_ok());
    }

    #[test]
    fn run_cli_uninstall_fails_when_not_installed() {
        let mgr = MockManager::not_installed();
        assert!(matches!(
            run_cli(&ServiceCommand::Uninstall, &mgr),
            Err(ServiceError::NotInstalled)
        ));
    }
}
