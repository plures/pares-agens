//! IPC layer — Unix socket (Linux/macOS) and named pipe (Windows) abstraction.
//!
//! The Tauri front-end connects to a running service instance through this IPC
//! channel to avoid embedding the full agent stack inside the GUI process.

use crate::error::ServiceError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A message sent from a client to the service over the IPC channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    /// Query the current service health status.
    Health,
    /// Gracefully shut down the service.
    Shutdown,
    /// Ping — the service responds with Pong.
    Ping,
}

/// A message returned by the service in response to an [`IpcRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    /// Response to a Health request.
    Health {
        /// Current health status string (e.g. `"ok"` or `"degraded"`).
        status: String,
    },
    /// Acknowledgment of a shutdown request.
    ShutdownAck,
    /// Response to a Ping.
    Pong,
    /// An error occurred while processing the request.
    Error {
        /// Human-readable description of the error.
        message: String,
    },
}

/// Platform-agnostic IPC transport abstraction.
pub trait IpcTransport: Send + Sync {
    /// Return the socket path / pipe name used for communication.
    fn path(&self) -> &str;
}

/// IPC transport backed by a Unix domain socket (Linux and macOS).
#[cfg(unix)]
pub struct UnixSocketTransport {
    socket_path: String,
}

#[cfg(unix)]
impl UnixSocketTransport {
    /// Create a transport using the default runtime socket path.
    ///
    /// The socket lives under `$XDG_RUNTIME_DIR` (Linux) or
    /// `$TMPDIR` (macOS), falling back to `/tmp`.
    pub fn new() -> Self {
        let dir = unix_runtime_dir();
        let path = dir.join("pares-agens.sock");
        let socket_path = path.to_str()
            .unwrap_or("/tmp/pares-agens.sock")
            .to_owned();
        Self { socket_path }
    }

    /// Create a transport using an explicit socket path.
    pub fn with_path(path: impl Into<String>) -> Self {
        Self { socket_path: path.into() }
    }
}

#[cfg(unix)]
impl Default for UnixSocketTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl IpcTransport for UnixSocketTransport {
    fn path(&self) -> &str {
        &self.socket_path
    }
}

#[cfg(unix)]
fn unix_runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("TMPDIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from("/tmp")
}

/// Named-pipe IPC transport for Windows (stub).
#[cfg(windows)]
pub struct NamedPipeTransport {
    pipe_name: String,
}

#[cfg(windows)]
impl NamedPipeTransport {
    pub fn new() -> Self {
        Self {
            pipe_name: r"\\.\pipe\pares-agens".to_owned(),
        }
    }
}

#[cfg(windows)]
impl Default for NamedPipeTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl IpcTransport for NamedPipeTransport {
    fn path(&self) -> &str {
        &self.pipe_name
    }
}

/// Return the platform-default [`IpcTransport`].
pub fn default_transport() -> Box<dyn IpcTransport> {
    #[cfg(unix)]
    return Box::new(UnixSocketTransport::new());

    #[cfg(windows)]
    return Box::new(NamedPipeTransport::new());

    #[cfg(not(any(unix, windows)))]
    {
        Box::new(FallbackTransport)
    }
}

/// Serialize an [`IpcRequest`] to JSON bytes.
pub fn encode_request(req: &IpcRequest) -> Result<Vec<u8>, ServiceError> {
    serde_json::to_vec(req).map_err(|e| ServiceError::Ipc(e.to_string()))
}

/// Deserialize an [`IpcResponse`] from JSON bytes.
pub fn decode_response(data: &[u8]) -> Result<IpcResponse, ServiceError> {
    serde_json::from_slice(data).map_err(|e| ServiceError::Ipc(e.to_string()))
}

#[cfg(not(any(unix, windows)))]
struct FallbackTransport;

#[cfg(not(any(unix, windows)))]
impl IpcTransport for FallbackTransport {
    fn path(&self) -> &str {
        "/tmp/pares-agens.sock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let req = IpcRequest::Ping;
        let encoded = encode_request(&req).expect("encode failed");
        // Decode as an IpcRequest to verify round-trip
        let decoded: IpcRequest =
            serde_json::from_slice(&encoded).expect("decode failed");
        assert!(matches!(decoded, IpcRequest::Ping));
    }

    #[test]
    fn encode_health_request() {
        let req = IpcRequest::Health;
        let encoded = encode_request(&req).unwrap();
        let json = String::from_utf8(encoded).unwrap();
        assert!(json.contains("health"), "expected 'health' in {json}");
    }

    #[test]
    fn decode_pong_response() {
        let json = br#"{"type":"pong"}"#;
        let resp = decode_response(json).unwrap();
        assert!(matches!(resp, IpcResponse::Pong));
    }

    #[test]
    fn decode_error_response() {
        let json = br#"{"type":"error","message":"something went wrong"}"#;
        let resp = decode_response(json).unwrap();
        if let IpcResponse::Error { message } = resp {
            assert_eq!(message, "something went wrong");
        } else {
            panic!("expected Error response");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_path_ends_with_sock() {
        let transport = UnixSocketTransport::new();
        assert!(
            transport.path().ends_with(".sock"),
            "expected .sock suffix, got {}",
            transport.path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_with_explicit_path() {
        let transport = UnixSocketTransport::with_path("/tmp/test.sock");
        assert_eq!(transport.path(), "/tmp/test.sock");
    }}
