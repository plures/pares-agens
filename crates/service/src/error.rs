use thiserror::Error;

/// Errors produced by the service lifecycle manager.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// The requested operation is not supported on this platform.
    #[error("operation not supported on this platform")]
    Unsupported,

    /// The service is not installed and must be installed first.
    #[error("service is not installed")]
    NotInstalled,

    /// The service is already installed.
    #[error("service is already installed")]
    AlreadyInstalled,

    /// The service is already running.
    #[error("service is already running")]
    AlreadyRunning,

    /// The service is not running.
    #[error("service is not running")]
    NotRunning,

    /// An I/O error occurred while interacting with the service manager.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A subprocess invoked by the manager returned a non-zero exit code.
    #[error("command failed with status {status}: {message}")]
    CommandFailed {
        /// The exit status code returned by the subprocess.
        status: i32,
        /// Human-readable description of the failure.
        message: String,
    },

    /// Failed to parse the service manager's output.
    #[error("failed to parse service manager output: {0}")]
    ParseError(String),

    /// IPC communication error.
    #[error("IPC error: {0}")]
    Ipc(String),
}
