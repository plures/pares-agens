//! `mcp-client` — MCP (Model Context Protocol) client for tool orchestration.
//!
//! Connects to MCP servers over stdio (spawned subprocess) or HTTP and
//! provides tool discovery, caching, and conversion to OpenAI function-calling
//! format.
//!
//! # Quick start
//!
//! ```no_run
//! use mcp_client::{McpClient, transport::stdio::StdioTransport};
//!
//! #[tokio::main]
//! async fn main() -> mcp_client::Result<()> {
//!     let transport = StdioTransport::spawn("uvx", &["mcp-server-time"]).await?;
//!     let mut client = McpClient::new(transport);
//!     client.initialize().await?;
//!
//!     let tools = client.list_tools().await?;
//!     for tool in &tools {
//!         println!("{}: {}", tool.name, tool.description.as_deref().unwrap_or(""));
//!     }
//!
//!     // Convert to OpenAI function-calling format
//!     let openai_tools = client.openai_tools().await?;
//!     println!("{}", serde_json::to_string_pretty(&openai_tools)?);
//!
//!     Ok(())
//! }
//! ```

#![warn(missing_docs)]

pub mod client;
pub mod error;
pub mod openai;
pub mod protocol;
pub mod transport;

pub use client::McpClient;
pub use error::{McpError, Result};
