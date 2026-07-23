//! Stdio spine channel — read lines from stdin, write responses to stdout.
//!
//! This enables testing the full spine pipeline without any external service.
//! Each line on stdin becomes an inbound message; delivery events are printed to stdout.

use async_trait::async_trait;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{debug, info};
use uuid::Uuid;

use pares_radix_core::spine::channel::{ChannelError, DeliveryResult, SpineChannel};
use pares_radix_core::spine::event::SpineEvent;
use pares_radix_core::spine::pipeline::PipelineEmitter;

/// Stdio spine channel — stdin in, stdout out.
pub struct StdioSpineChannel {
    /// User identity for messages (defaults to "user").
    pub sender: String,
    /// Chat ID (defaults to "stdio").
    pub chat_id: String,
}

impl Default for StdioSpineChannel {
    fn default() -> Self {
        Self {
            sender: "user".into(),
            chat_id: "stdio".into(),
        }
    }
}

impl StdioSpineChannel {
    /// Create a new StdioSpineChannel with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the sender name for outbound messages.
    pub fn with_sender(mut self, sender: impl Into<String>) -> Self {
        self.sender = sender.into();
        self
    }

    /// Run the delivery loop — prints responses to stdout.
    pub async fn run_delivery_loop(
        &self,
        mut delivery_rx: tokio::sync::broadcast::Receiver<SpineEvent>,
    ) {
        info!("stdio_spine: delivery loop started");

        loop {
            match delivery_rx.recv().await {
                Ok(SpineEvent::DeliveryRequest {
                    channel, content, ..
                }) => {
                    if channel != "stdio" {
                        continue;
                    }
                    // Print response with a visual separator
                    println!("\n{content}\n");
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[stdio: skipped {n} events]");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("stdio_spine: delivery channel closed");
                    break;
                }
            }
        }
    }
}

#[async_trait]
impl SpineChannel for StdioSpineChannel {
    fn channel_id(&self) -> &str {
        "stdio"
    }

    async fn start_receiving(&self, emitter: PipelineEmitter) -> Result<(), ChannelError> {
        info!("stdio_spine: reading from stdin (type messages, press Enter)");
        eprintln!("pares-radix [stdio] ready. Type a message:");

        let stdin = io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        let sender = self.sender.clone();
        let chat_id = self.chat_id.clone();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if line == "/quit" || line == "/exit" {
                info!("stdio_spine: user requested exit");
                break;
            }

            let event = SpineEvent::Inbound {
                id: Uuid::new_v4().to_string(),
                source: "stdio".into(),
                chat_id: chat_id.clone(),
                sender: sender.clone(),
                content: line,
                metadata: serde_json::json!({}),
            };

            debug!(event_id = %event.id(), "stdio_spine: emitting inbound");
            emitter.emit(event).await;
        }

        Ok(())
    }

    async fn deliver(&self, event: &SpineEvent) -> Result<DeliveryResult, ChannelError> {
        if let SpineEvent::DeliveryRequest { content, .. } = event {
            println!("{content}");
            Ok(DeliveryResult {
                success: true,
                platform_message_id: None,
            })
        } else {
            Ok(DeliveryResult {
                success: false,
                platform_message_id: None,
            })
        }
    }
}
