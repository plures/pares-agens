//! Stdio transport: communicates with an MCP server process via stdin/stdout.

use async_trait::async_trait;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

use crate::{
    error::{McpError, Result},
    protocol::{JsonRpcRequest, JsonRpcResponse},
};

use super::Transport;

/// Spawns a process and communicates with it over stdin/stdout using
/// newline-delimited JSON-RPC 2.0.
///
/// Notifications (requests without an `id`) are sent without waiting for a
/// response; requests with an `id` block until the matching response line
/// arrives.
///
/// The child process is killed (best-effort) when this struct is dropped.
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    /// Spawn `program` with `args` and return a transport connected to it.
    pub async fn spawn(program: &str, args: &[&str]) -> Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("failed to open stdin for child process".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("failed to open stdout for child process".into()))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Best-effort: send SIGKILL so the server process doesn't linger.
        let _ = self.child.start_kill();
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        // Notifications have no id and expect no response from the server.
        if request.id.is_none() {
            return Ok(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: serde_json::Value::Null,
                result: Some(serde_json::json!({})),
                error: None,
            });
        }

        let mut response_line = String::new();
        let n = self.stdout.read_line(&mut response_line).await?;
        if n == 0 {
            return Err(McpError::Transport(
                "child process closed stdout (EOF) while waiting for response".into(),
            ));
        }

        let response: JsonRpcResponse = serde_json::from_str(response_line.trim())?;
        Ok(response)
    }
}
