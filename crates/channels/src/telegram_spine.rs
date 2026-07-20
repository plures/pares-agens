//! Thin Telegram channel adapter for the spine pipeline.
//!
//! This replaces the fat Telegram adapter. It does ONLY:
//! 1. Receive Telegram updates → emit SpineEvent::Inbound
//! 2. Subscribe to DeliveryRequest events → send via Telegram Bot API
//!
//! All logic (model calls, slash commands, cerebellum, history, tools)
//! lives in spine procedures. This adapter is interchangeable.

use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{ChatId, ReplyParameters};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use pares_radix_core::model::StreamDelta;
use pares_radix_core::spine::channel::{ChannelError, DeliveryResult, SpineChannel};
use pares_radix_core::spine::event::SpineEvent;
use pares_radix_core::spine::pipeline::PipelineEmitter;

/// Configuration for the thin Telegram spine channel.
pub struct TelegramSpineConfig {
    /// Bot token from BotFather.
    pub token: String,
}

/// Thin Telegram channel — input/output only.
///
/// Optionally subscribes to model streaming deltas for progressive message editing.
pub struct TelegramSpineChannel {
    config: TelegramSpineConfig,
    /// When set, enables progressive streaming: tokens are broadcast as they arrive
    /// from model_complete, and the channel edits the placeholder message.
    stream_tx: Option<broadcast::Sender<StreamDelta>>,
}

impl TelegramSpineChannel {
    /// Create a new thin Telegram channel.
    pub fn new(config: TelegramSpineConfig) -> Self {
        Self { config, stream_tx: None }
    }

    /// Create a new thin Telegram channel with progressive streaming enabled.
    pub fn with_stream(config: TelegramSpineConfig, stream_tx: broadcast::Sender<StreamDelta>) -> Self {
        Self { config, stream_tx: Some(stream_tx) }
    }

    /// Run the delivery loop — subscribes to pipeline delivery events
    /// and sends them via Telegram. When a placeholder_id is present in metadata,
    /// edits the placeholder instead of sending a new message.
    pub async fn run_delivery_loop(&self, mut delivery_rx: broadcast::Receiver<SpineEvent>) {
        let bot = Bot::new(&self.config.token);
        info!("telegram_spine: delivery loop started");

        loop {
            match delivery_rx.recv().await {
                Ok(SpineEvent::DeliveryRequest {
                    id,
                    channel,
                    chat_id,
                    content,
                    metadata,
                    ..
                }) => {
                    if channel != "telegram" {
                        continue;
                    }

                    let chat_id_parsed: i64 = match chat_id.parse() {
                        Ok(id) => id,
                        Err(_) => {
                            warn!(event_id = %id, chat_id = %chat_id, "telegram_spine: invalid chat_id");
                            continue;
                        }
                    };

                    debug!(event_id = %id, chat_id = chat_id_parsed, "telegram_spine: delivering");

                    // Check if we have a placeholder to edit
                    let placeholder_id = metadata
                        .get("placeholder_id")
                        .and_then(|v| v.as_i64())
                        .map(|id| teloxide::types::MessageId(id as i32));

                    if let Some(pid) = placeholder_id {
                        // Edit placeholder with final formatted response
                        match bot.edit_message_text(
                            ChatId(chat_id_parsed),
                            pid,
                            &content,
                        ).await {
                            Ok(_) => {
                                debug!(event_id = %id, "telegram_spine: delivered via edit");
                            }
                            Err(e) => {
                                // Fall back to sending a new message
                                warn!(error = %e, "telegram_spine: edit failed, sending new message");
                                match bot.send_message(ChatId(chat_id_parsed), &content).await {
                                    Ok(msg) => debug!(message_id = msg.id.0, "telegram_spine: fallback delivered"),
                                    Err(e2) => error!(error = %e2, "telegram_spine: delivery failed"),
                                }
                            }
                        }
                    } else {
                        // No placeholder — send new message
                        match bot.send_message(ChatId(chat_id_parsed), &content).await {
                            Ok(msg) => {
                                debug!(message_id = msg.id.0, "telegram_spine: delivered");
                            }
                            Err(e) => {
                                error!(error = %e, "telegram_spine: delivery failed");
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "telegram_spine: delivery receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("telegram_spine: pipeline closed, stopping");
                    break;
                }
            }
        }
    }
}

#[async_trait]
impl SpineChannel for TelegramSpineChannel {
    fn channel_id(&self) -> &str {
        "telegram"
    }

    async fn start_receiving(&self, emitter: PipelineEmitter) -> Result<(), ChannelError> {
        let bot = Bot::new(&self.config.token);

        // Validate token upfront — teloxide's dispatch() panics on invalid tokens
        // (teloxide-0.17 dispatcher.rs:385 unwraps get_me). Catch it here cleanly.
        match bot.get_me().await {
            Ok(me) => {
                info!(
                    bot_username = %me.username(),
                    "telegram_spine: token validated, starting receiver"
                );
            }
            Err(e) => {
                return Err(ChannelError::ConnectionError(format!(
                    "Telegram token validation failed: {e}. Check your TELEGRAM_TOKEN."
                )));
            }
        }

        let stream_tx = self.stream_tx.clone();

        let handler = Update::filter_message().endpoint(move |bot: Bot, msg: Message| {
            let emitter = emitter.clone();
            let stream_tx = stream_tx.clone();
            async move {
                let text = msg.text().unwrap_or("").to_string();
                if text.is_empty() {
                    return Ok::<(), teloxide::RequestError>(());
                }

                let sender = msg
                    .from
                    .as_ref()
                    .map(|u| u.username.clone().unwrap_or_else(|| u.first_name.clone()))
                    .unwrap_or_else(|| "unknown".to_string());

                // Send placeholder and start progressive streaming
                let placeholder_msg = bot
                    .send_message(msg.chat.id, "\u{23f3}")
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .await;

                if let (Ok(placeholder), Some(ref stx)) = (&placeholder_msg, &stream_tx) {
                    let mut stream_rx = stx.subscribe();
                    let edit_bot = bot.clone();
                    let edit_chat_id = msg.chat.id;
                    let pid = placeholder.id;
                    tokio::spawn(async move {
                        let mut accumulated = String::new();
                        let mut last_edit = tokio::time::Instant::now();
                        let debounce = std::time::Duration::from_millis(500);
                        let min_chars = 20;

                        loop {
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(120),
                                stream_rx.recv(),
                            ).await {
                                Ok(Ok(StreamDelta::Content(chunk))) => {
                                    accumulated.push_str(&chunk);
                                    if last_edit.elapsed() >= debounce && accumulated.len() >= min_chars {
                                        let display = format!("{}\u{25cf}", accumulated);
                                        let _ = edit_bot.edit_message_text(edit_chat_id, pid, &display).await;
                                        last_edit = tokio::time::Instant::now();
                                    }
                                }
                                Ok(Ok(StreamDelta::Done)) => break,
                                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                                Ok(Ok(_)) => {} // ToolCall variants — ignore
                                Err(_) => break, // Timeout
                            }
                        }
                    });
                }

                // Emit inbound to pipeline (spine procedures handle the rest)
                emitter
                    .emit(SpineEvent::Inbound {
                        id: SpineEvent::new_id(),
                        source: "telegram".into(),
                        chat_id: msg.chat.id.0.to_string(),
                        sender,
                        content: text,
                        metadata: serde_json::json!({
                            "message_id": msg.id.0,
                            "placeholder_id": placeholder_msg.as_ref().ok().map(|m| m.id.0),
                        }),
                    })
                    .await;

                Ok(())
            }
        });

        teloxide::dispatching::Dispatcher::builder(bot, handler)
            .build()
            .dispatch()
            .await;

        Ok(())
    }

    async fn deliver(&self, event: &SpineEvent) -> Result<DeliveryResult, ChannelError> {
        let SpineEvent::DeliveryRequest {
            chat_id, content, ..
        } = event
        else {
            return Ok(DeliveryResult {
                success: false,
                platform_message_id: None,
            });
        };

        let chat_id_parsed: i64 = chat_id
            .parse()
            .map_err(|_| ChannelError::DeliveryFailed("invalid chat_id".into()))?;

        let bot = Bot::new(&self.config.token);
        let msg = bot
            .send_message(ChatId(chat_id_parsed), content)
            .await
            .map_err(|e| ChannelError::DeliveryFailed(e.to_string()))?;

        Ok(DeliveryResult {
            success: true,
            platform_message_id: Some(msg.id.0.to_string()),
        })
    }
}
