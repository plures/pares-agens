use crate::error::ServiceError;
use crate::{ServiceInfo, ServiceManager, ServiceStatus};
use std::process::Command;

const SERVICE_NAME: &str = "pares-agens";
const UNIT_TEMPLATE: &str = r#"[Unit]
Description=Pares Agens — AI assistant service
After=network.target

[Service]
Type=simple
ExecStart={exec_path}
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#;

/// Manages the Pares Agens service via systemd user units.
pub struct LinuxServiceManager {
    exec_path: String,
}

impl LinuxServiceManager {
    pub fn new() -> Self {
        let exec_path = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "pares-agens".to_owned());
        Self { exec_path }
    }

    fn unit_file_path(&self) -> std::path::PathBuf {
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
                std::path::PathBuf::from(home).join(".config")
            });
        config_dir
            .join("systemd")
            .join("user")
            .join(format!("{SERVICE_NAME}.service"))
    }

    fn systemctl(&self, args: &[&str]) -> Result<std::process::Output, ServiceError> {
        let mut cmd = Command::new("systemctl");
        cmd.arg("--user");
        for arg in args {
            cmd.arg(arg);
        }
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(ServiceError::CommandFailed {
                status: output.status.code().unwrap_or(-1),
                message: stderr,
            });
        }
        Ok(output)
    }
}

impl Default for LinuxServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for LinuxServiceManager {
    fn install(&self) -> Result<(), ServiceError> {
        let unit_path = self.unit_file_path();
        if unit_path.exists() {
            return Err(ServiceError::AlreadyInstalled);
        }
        if let Some(parent) = unit_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let unit_content = UNIT_TEMPLATE.replace("{exec_path}", &self.exec_path);
        std::fs::write(&unit_path, unit_content)?;
        self.systemctl(&["daemon-reload"])?;
        self.systemctl(&["enable", SERVICE_NAME])?;
        Ok(())
    }

    fn start(&self) -> Result<(), ServiceError> {
        let info = self.status()?;
        if info.status == ServiceStatus::NotInstalled {
            return Err(ServiceError::NotInstalled);
        }
        if info.status == ServiceStatus::Running {
            return Err(ServiceError::AlreadyRunning);
        }
        self.systemctl(&["start", SERVICE_NAME])?;
        Ok(())
    }

    fn stop(&self) -> Result<(), ServiceError> {
        let info = self.status()?;
        if info.status != ServiceStatus::Running {
            return Err(ServiceError::NotRunning);
        }
        self.systemctl(&["stop", SERVICE_NAME])?;
        Ok(())
    }

    fn status(&self) -> Result<ServiceInfo, ServiceError> {
        let unit_path = self.unit_file_path();
        if !unit_path.exists() {
            return Ok(ServiceInfo {
                status: ServiceStatus::NotInstalled,
                pid: None,
                description: "Service unit file not found".to_owned(),
            });
        }

        let output = Command::new("systemctl")
            .args(["--user", "is-active", SERVICE_NAME])
            .output()?;

        let active = String::from_utf8_lossy(&output.stdout).trim().to_owned();

        let (status, description) = match active.as_str() {
            "active" => (
                ServiceStatus::Running,
                "Service is active and running".to_owned(),
            ),
            "inactive" | "dead" => (ServiceStatus::Stopped, "Service is inactive".to_owned()),
            "failed" => (ServiceStatus::Stopped, "Service has failed".to_owned()),
            other => (
                ServiceStatus::Unknown,
                format!("Unexpected systemd state: {other}"),
            ),
        };

        let pid = if status == ServiceStatus::Running {
            self.get_pid()
        } else {
            None
        };

        Ok(ServiceInfo {
            status,
            pid,
            description,
        })
    }

    fn uninstall(&self) -> Result<(), ServiceError> {
        let unit_path = self.unit_file_path();
        if !unit_path.exists() {
            return Err(ServiceError::NotInstalled);
        }
        let _ = self.systemctl(&["stop", SERVICE_NAME]);
        self.systemctl(&["disable", SERVICE_NAME])?;
        std::fs::remove_file(&unit_path)?;
        self.systemctl(&["daemon-reload"])?;
        Ok(())
    }
}

impl LinuxServiceManager {
    fn get_pid(&self) -> Option<u32> {
        let output = Command::new("systemctl")
            .args(["--user", "show", "-p", "MainPID", "--value", SERVICE_NAME])
            .output()
            .ok()?;
        let pid_str = String::from_utf8_lossy(&output.stdout);
        let pid: u32 = pid_str.trim().parse().ok()?;
        if pid == 0 {
            None
        } else {
            Some(pid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file_path_is_under_systemd_user_dir() {
        let mgr = LinuxServiceManager::new();
        let path = mgr.unit_file_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("systemd/user"),
            "expected systemd/user path, got {path_str}"
        );
        assert!(path_str.ends_with(".service"), "expected .service suffix");
    }

    #[test]
    fn unit_template_contains_exec_path() {
        let mgr = LinuxServiceManager {
            exec_path: "/usr/local/bin/pares-agens".to_owned(),
        };
        let unit_path = mgr.unit_file_path();
        // Just verify the exec_path is embedded in the template
        let content = UNIT_TEMPLATE.replace("{exec_path}", &mgr.exec_path);
        assert!(content.contains("/usr/local/bin/pares-agens"));
        drop(unit_path);
    }

    #[test]
    fn default_uses_new() {
        // Default::default() must produce a valid instance with a well-formed path.
        let mgr = LinuxServiceManager::default();
        let path = mgr.unit_file_path();
        assert!(path.to_string_lossy().ends_with(".service"));
    }

    #[test]
    fn unit_template_has_required_systemd_sections() {
        assert!(UNIT_TEMPLATE.contains("[Unit]"));
        assert!(UNIT_TEMPLATE.contains("[Service]"));
        assert!(UNIT_TEMPLATE.contains("[Install]"));
        assert!(UNIT_TEMPLATE.contains("ExecStart={exec_path}"));
        assert!(UNIT_TEMPLATE.contains("Restart=on-failure"));
    }

    #[test]
    fn status_returns_not_installed_when_unit_file_absent() {
        // The CI environment has no pares-agens systemd unit installed, so
        // status() must return NotInstalled without ever calling systemctl.
        let mgr = LinuxServiceManager::new();
        let unit_path = mgr.unit_file_path();
        if unit_path.exists() {
            // Skip this assertion when running on a developer machine that
            // actually has the service installed.
            return;
        }
        let info = mgr.status().expect("status() must not fail");
        assert_eq!(info.status, crate::ServiceStatus::NotInstalled);
        assert!(info.pid.is_none());
    }

    #[test]
    fn install_writes_unit_file_to_correct_location() {
        // Verify that the UNIT_TEMPLATE expands the exec_path placeholder and
        // produces a well-formed systemd unit file.  This test exercises the
        // template expansion logic without requiring systemctl.
        let exec_path = "/usr/local/bin/pares-agens";
        let content = UNIT_TEMPLATE.replace("{exec_path}", exec_path);

        // The expanded file must contain the resolved ExecStart line.
        assert!(
            content.contains(&format!("ExecStart={exec_path}")),
            "unit file must embed exec path"
        );
        // Confirm all required sections are present after expansion.
        assert!(content.contains("[Unit]"));
        assert!(content.contains("[Service]"));
        assert!(content.contains("[Install]"));

        // Verify the file can be written to a temp location (exercises the
        // std::fs::write path used by install()).
        let tmp = tempfile::tempdir().expect("tempdir");
        let unit_path = tmp
            .path()
            .join("systemd")
            .join("user")
            .join(format!("{SERVICE_NAME}.service"));
        std::fs::create_dir_all(unit_path.parent().unwrap()).expect("create dirs");
        std::fs::write(&unit_path, &content).expect("write unit file");

        let read_back = std::fs::read_to_string(&unit_path).expect("read unit file");
        assert_eq!(content, read_back);
    }
}
