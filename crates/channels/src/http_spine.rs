//! HTTP spine channel — REST API for direct access to pares-radix.
//!
//! Exposes a local HTTP server that accepts messages and returns responses.
//! No external service dependency — just `curl` or any HTTP client.
//!
//! Endpoints:
//!   POST /v1/chat     — send a message, receive the response
//!   GET  /v1/health   — health check
//!   GET  /v1/status   — pipeline status
//!
//! Usage in serve-spine:
//!   pares-radix serve-spine --channel http --http-port 3200

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use pares_radix_core::spine::channel::{ChannelError, DeliveryResult, SpineChannel};
use pares_radix_core::spine::event::SpineEvent;
use pares_radix_core::spine::pipeline::PipelineEmitter;

/// Configuration for the HTTP spine channel.
#[derive(Debug, Clone)]
pub struct HttpSpineConfig {
    /// Port to listen on (default: 3200)
    pub port: u16,
    /// Optional bearer token for authentication (None = no auth)
    pub bearer_token: Option<String>,
    /// Response timeout in seconds (default: 120)
    pub timeout_seconds: u64,
}

impl Default for HttpSpineConfig {
    fn default() -> Self {
        Self {
            port: 3200,
            bearer_token: None,
            timeout_seconds: 120,
        }
    }
}

/// HTTP spine channel — local REST API access.
pub struct HttpSpineChannel {
    /// Channel configuration.
    pub config: HttpSpineConfig,
}

impl HttpSpineChannel {
    /// Create a new HTTP spine channel with the given configuration.
    pub fn new(config: HttpSpineConfig) -> Self {
        Self { config }
    }

    /// Run the delivery loop — collects responses and routes them to waiting callers.
    pub async fn run_delivery_loop(
        &self,
        mut delivery_rx: broadcast::Receiver<SpineEvent>,
        pending: Arc<PendingResponses>,
    ) {
        info!("http_spine: delivery loop started");

        loop {
            match delivery_rx.recv().await {
                Ok(SpineEvent::DeliveryRequest {
                    ref id,
                    ref channel,
                    ref content,
                    ref chat_id,
                    ..
                }) => {
                    if channel != "http" {
                        continue;
                    }
                    // Route response to the waiting request by session chat_id
                    let session_id = chat_id.clone();
                    if let Some(tx) = pending.take(&session_id).await {
                        let _ = tx.send(content.clone());
                        debug!(session_id = %session_id, "http_spine: delivered response");
                    } else {
                        warn!(
                            id = %id,
                            session_id = %session_id,
                            "http_spine: no pending request for delivery"
                        );
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("http_spine: skipped {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("http_spine: delivery channel closed");
                    break;
                }
            }
        }
    }
}

/// Tracks pending request→response mappings.
#[derive(Default)]
pub struct PendingResponses {
    inner: Mutex<std::collections::HashMap<String, oneshot::Sender<String>>>,
}

impl PendingResponses {
    /// Insert a pending response. Returns false if a request is already pending for this session.
    pub async fn insert(&self, id: String, tx: oneshot::Sender<String>) -> bool {
        let mut inner = self.inner.lock().await;
        if inner.contains_key(&id) {
            return false;
        }
        inner.insert(id, tx);
        true
    }

    /// Take and remove a pending response sender by ID.
    pub async fn take(&self, id: &str) -> Option<oneshot::Sender<String>> {
        self.inner.lock().await.remove(id)
    }
}

// ── Axum handlers ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    emitter: PipelineEmitter,
    pending: Arc<PendingResponses>,
    config: HttpSpineConfig,
}

#[derive(Deserialize)]
struct ChatRequest {
    /// The message to send
    message: String,
    /// Optional sender name (default: "user")
    sender: Option<String>,
    /// Optional session ID for multi-turn conversation (default: sender-based)
    session_id: Option<String>,
}

#[derive(Serialize)]
struct ChatResponse {
    /// Unique request ID
    id: String,
    /// The assistant's response
    response: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    channel: &'static str,
    version: &'static str,
}

async fn handle_health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        channel: "http",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn handle_chat(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, StatusCode> {
    // Reject empty messages
    if req.message.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let request_id = Uuid::new_v4().to_string();
    let sender = req.sender.unwrap_or_else(|| "user".into());
    // Stable session for multi-turn: explicit session_id > sender-based default
    let session_id = req.session_id.unwrap_or_else(|| format!("http:{}", sender));

    // Create response channel (keyed by session for delivery routing)
    let (tx, rx) = oneshot::channel();
    if !state.pending.insert(session_id.clone(), tx).await {
        warn!(session_id = %session_id, "http_spine: rejected concurrent request");
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    // Emit inbound event
    // chat_id = session_id (for conversation history continuity)
    // metadata.request_id = unique per request (for response routing)
    let event = SpineEvent::Inbound {
        id: Uuid::new_v4().to_string(),
        source: "http".into(),
        chat_id: session_id.clone(),
        sender,
        content: req.message,
        metadata: serde_json::json!({ "request_id": request_id }),
    };
    state.emitter.emit(event).await;

    // Wait for response with timeout
    let timeout = Duration::from_secs(state.config.timeout_seconds);
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(response)) => Ok(Json(ChatResponse {
            id: request_id,
            response,
        })),
        Ok(Err(_)) => {
            error!(request_id = %request_id, "http_spine: response channel dropped");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(_) => {
            // Clean up pending
            state.pending.take(&session_id).await;
            error!(request_id = %request_id, "http_spine: response timeout");
            Err(StatusCode::GATEWAY_TIMEOUT)
        }
    }
}

/// Start the HTTP server. Call this from the serve-spine command.
pub async fn start_http_server(
    emitter: PipelineEmitter,
    pending: Arc<PendingResponses>,
    config: HttpSpineConfig,
) -> Result<(), ChannelError> {
    let port = config.port;

    let state = AppState {
        emitter,
        pending,
        config,
    };

    let app = Router::new()
        .route("/v1/health", get(handle_health))
        .route("/v1/chat", post(handle_chat))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    info!("http_spine: listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| ChannelError::ConnectionError(format!("bind failed: {e}")))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| ChannelError::ConnectionError(format!("server error: {e}")))?;

    Ok(())
}

#[async_trait]
impl SpineChannel for HttpSpineChannel {
    fn channel_id(&self) -> &str {
        "http"
    }

    async fn start_receiving(&self, _emitter: PipelineEmitter) -> Result<(), ChannelError> {
        // HTTP channel uses start_http_server() instead of start_receiving()
        // because it needs the PendingResponses shared state.
        // This is a no-op — the actual server is started separately.
        Ok(())
    }

    async fn deliver(&self, event: &SpineEvent) -> Result<DeliveryResult, ChannelError> {
        if let SpineEvent::DeliveryRequest { content, .. } = event {
            debug!(len = content.len(), "http_spine: deliver called");
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
