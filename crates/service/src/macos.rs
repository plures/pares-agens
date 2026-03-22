use crate::error::ServiceError;
use crate::{ServiceInfo, ServiceManager, ServiceStatus};
use std::process::Command;

const SERVICE_LABEL: &str = "com.plures.pares-agens";
const PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exec_path}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_dir}/pares-agens.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/pares-agens-err.log</string>
</dict>
</plist>
"#;

/// Manages the Pares Agens service via launchd user agents.
pub struct MacosServiceManager {
    exec_path: String,
}

impl MacosServiceManager {
    pub fn new() -> Self {
        let exec_path = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "pares-agens".to_owned());
        Self { exec_path }
    }

    fn plist_path(&self) -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        std::path::PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{SERVICE_LABEL}.plist"))
    }

    fn log_dir(&self) -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        format!("{home}/Library/Logs")
    }

    fn launchctl(&self, args: &[&str]) -> Result<std::process::Output, ServiceError> {
        let mut cmd = Command::new("launchctl");
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

impl Default for MacosServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for MacosServiceManager {
    fn install(&self) -> Result<(), ServiceError> {
        let plist_path = self.plist_path();
        if plist_path.exists() {
            return Err(ServiceError::AlreadyInstalled);
        }
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log_dir = self.log_dir();
        let plist_content = PLIST_TEMPLATE
            .replace("{label}", SERVICE_LABEL)
            .replace("{exec_path}", &self.exec_path)
            .replace("{log_dir}", &log_dir);
        std::fs::write(&plist_path, plist_content)?;
        let path_str = plist_path.to_str().ok_or_else(|| {
            ServiceError::ParseError("plist path contains invalid UTF-8".to_owned())
        })?;
        self.launchctl(&["load", path_str])?;
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
        self.launchctl(&["start", SERVICE_LABEL])?;
        Ok(())
    }

    fn stop(&self) -> Result<(), ServiceError> {
        let info = self.status()?;
        if info.status != ServiceStatus::Running {
            return Err(ServiceError::NotRunning);
        }
        self.launchctl(&["stop", SERVICE_LABEL])?;
        Ok(())
    }

    fn status(&self) -> Result<ServiceInfo, ServiceError> {
        let plist_path = self.plist_path();
        if !plist_path.exists() {
            return Ok(ServiceInfo {
                status: ServiceStatus::NotInstalled,
                pid: None,
                description: "Launch agent plist not found".to_owned(),
            });
        }

        let output = Command::new("launchctl")
            .args(["list", SERVICE_LABEL])
            .output()?;

        if !output.status.success() {
            return Ok(ServiceInfo {
                status: ServiceStatus::Stopped,
                pid: None,
                description: "Service is not loaded".to_owned(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let pid = parse_launchctl_pid(&stdout);
        let (status, description) = if pid.is_some() {
            (ServiceStatus::Running, "Service is running".to_owned())
        } else {
            (ServiceStatus::Stopped, "Service is loaded but not running".to_owned())
        };

        Ok(ServiceInfo { status, pid, description })
    }

    fn uninstall(&self) -> Result<(), ServiceError> {
        let plist_path = self.plist_path();
        if !plist_path.exists() {
            return Err(ServiceError::NotInstalled);
        }
        let _ = self.launchctl(&["stop", SERVICE_LABEL]);
        let path_str = plist_path.to_str().ok_or_else(|| {
            ServiceError::ParseError("plist path contains invalid UTF-8".to_owned())
        })?;
        self.launchctl(&["unload", path_str])?;
        std::fs::remove_file(&plist_path)?;
        Ok(())
    }
}

fn parse_launchctl_pid(output: &str) -> Option<u32> {
    // `launchctl list <label>` returns JSON with a "PID" field when running
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with('"') && line.contains("PID") {
            // Try a simple text extraction: "PID" = <num>
            if let Some(eq) = line.find('=') {
                let value = line[eq + 1..].trim().trim_matches(';');
                if let Ok(pid) = value.parse::<u32>() {
                    if pid > 0 {
                        return Some(pid);
                    }
                }
            }
        }
    }
    // Try JSON-style: "PID" : 12345
    for line in output.lines() {
        if line.contains("\"PID\"") {
            if let Some(colon) = line.find(':') {
                let value = line[colon + 1..].trim().trim_matches(',');
                if let Ok(pid) = value.parse::<u32>() {
                    if pid > 0 {
                        return Some(pid);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_path_is_under_launch_agents() {
        let mgr = MacosServiceManager::new();
        let path = mgr.plist_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("LaunchAgents"),
            "expected LaunchAgents path, got {path_str}"
        );
        assert!(path_str.ends_with(".plist"), "expected .plist suffix");
    }

    #[test]
    fn plist_template_contains_label_and_exec() {
        let mgr = MacosServiceManager {
            exec_path: "/usr/local/bin/pares-agens".to_owned(),
        };
        let content = PLIST_TEMPLATE
            .replace("{label}", SERVICE_LABEL)
            .replace("{exec_path}", &mgr.exec_path)
            .replace("{log_dir}", "/tmp/logs");
        assert!(content.contains(SERVICE_LABEL));
        assert!(content.contains("/usr/local/bin/pares-agens"));
    }

    #[test]
    fn parse_launchctl_pid_returns_none_for_empty() {
        assert_eq!(parse_launchctl_pid(""), None);
    }

    #[test]
    fn parse_launchctl_pid_returns_none_for_zero() {
        let output = r#"{"PID": 0, "Label": "com.plures.pares-agens"}"#;
        assert_eq!(parse_launchctl_pid(output), None);
    }

    #[test]
    fn parse_launchctl_pid_returns_pid_when_running() {
        let output = r#"{
        "PID" : 12345,
        "Label" : "com.plures.pares-agens"
    }"#;
        assert_eq!(parse_launchctl_pid(output), Some(12345));
    }
}
