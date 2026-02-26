//! Stdio transport: communicates with an MCP server process via stdin/stdout.

use async_trait::async_trait;
use serde_json::Value;
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
pub struct StdioTransport {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
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

        let stdin = child.stdin.take().ok_or_else(|| {
            McpError::Transport("failed to open stdin for child process".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpError::Transport("failed to open stdout for child process".into())
        })?;

        Ok(Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    fn next_id(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        Value::Number(id.into())
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut response_line = String::new();
        self.stdout.read_line(&mut response_line).await?;

        let response: JsonRpcResponse = serde_json::from_str(response_line.trim())?;
        Ok(response)
    }
}
