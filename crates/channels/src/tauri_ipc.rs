//! Tauri IPC channel adapter.
//!
//! Bridges the Tauri desktop frontend to the Pares Agens core event loop via
//! in-process tokio channels, keeping this crate free of any Tauri dependency.
//!
//! # Usage
//!
//! ```rust,no_run
//! use pares_agens_channels::tauri_ipc::tauri_ipc_channel;
//!
//! let (adapter, handle) = tauri_ipc_channel("user");
//! // Pass `adapter` to the agent executor's run loop.
//! // Keep `handle` in the Tauri `AppState` to forward frontend messages.
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::sync::{oneshot, Mutex};

use pares_agens_core::Event;

use crate::adapter::{ChannelAdapter, ChannelError};

/// A single request dispatched from the Tauri frontend to the agent runtime.
pub struct TauriIpcMessage {
    /// Text content typed by the user.
    pub content: String,
    /// Frontend-assigned request identifier used to correlate streaming events.
    ///
    /// The adapter sets this as the [`Event::Message`] `id`, enabling the adapter
    /// callback to emit `model-chunk` / `model-error` Tauri events keyed to the
    /// same ID that the UI uses for its response placeholder.
    pub request_id: String,
    /// One-shot sender for the agent's response event.
    pub response_tx: oneshot::Sender<Option<Event>>,
}

/// Handle held by the Tauri app to send messages into the agent runtime.
///
/// Cheaply cloneable — the underlying [`mpsc::Sender`] is `Clone`.
pub struct TauriIpcHandle {
    /// Send a [`TauriIpcMessage`] to the adapter's receive loop.
    pub input_tx: mpsc::Sender<TauriIpcMessage>,
}

/// Channel adapter that receives messages from the Tauri IPC layer and forwards
/// them to the core event loop.
///
/// Created together with a [`TauriIpcHandle`] via [`tauri_ipc_channel`].
pub struct TauriIpcAdapter {
    from: String,
    input_rx: Arc<Mutex<mpsc::Receiver<TauriIpcMessage>>>,
}

/// Create a paired `(adapter, handle)`.
///
/// `from` is the sender name attached to every [`Event::Message`] emitted by the adapter.
pub fn tauri_ipc_channel(from: impl Into<String>) -> (TauriIpcAdapter, TauriIpcHandle) {
    let (input_tx, input_rx) = mpsc::channel(64);
    let adapter = TauriIpcAdapter {
        from: from.into(),
        input_rx: Arc::new(Mutex::new(input_rx)),
    };
    let handle = TauriIpcHandle { input_tx };
    (adapter, handle)
}

#[async_trait]
impl ChannelAdapter for TauriIpcAdapter {
    fn name(&self) -> &str {
        "tauri"
    }

    async fn run(
        &self,
        on_event: impl Fn(Event) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Event>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), ChannelError> {
        let mut rx = self.input_rx.lock().await;
        while let Some(TauriIpcMessage {
            content,
            request_id,
            response_tx,
        }) = rx.recv().await
        {
            let event = Event::Message {
                id: request_id,
                channel: "tauri".to_string(),
                sender: self.from.clone(),
                content,
            };
            let response = on_event(event).await;
            // Best-effort send — ignore if the caller already timed out or dropped.
            let _ = response_tx.send(response);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_name_is_tauri() {
        let (adapter, _handle) = tauri_ipc_channel("user");
        assert_eq!(adapter.name(), "tauri");
    }

    #[tokio::test]
    async fn forwards_message_and_returns_response() {
        let (adapter, handle) = tauri_ipc_channel("tester");

        // Spawn the adapter run loop with an echo handler.
        tokio::spawn(async move {
            adapter
                .run(|event| {
                    Box::pin(async move {
                        if let Event::Message { id, content, .. } = event {
                            Some(Event::ModelResponse {
                                request_id: id,
                                model: "echo".into(),
                                content: format!("Echo: {content}"),
                            })
                        } else {
                            None
                        }
                    })
                })
                .await
                .unwrap();
        });

        let (response_tx, response_rx) = oneshot::channel();
        handle
            .input_tx
            .send(TauriIpcMessage {
                content: "hello".into(),
                request_id: "test-req-1".into(),
                response_tx,
            })
            .await
            .unwrap();

        let response = response_rx.await.unwrap();
        match response {
            Some(Event::ModelResponse { content, .. }) => {
                assert_eq!(content, "Echo: hello");
            }
            other => panic!("expected ModelResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn closes_cleanly_when_handle_dropped() {
        let (adapter, handle) = tauri_ipc_channel("user");

        let run_task = tokio::spawn(async move {
            adapter
                .run(|_| Box::pin(async move { None }))
                .await
                .unwrap();
        });

        // Drop the handle — the adapter's receive loop should drain and exit.
        drop(handle);

        // Task should complete without panic.
        tokio::time::timeout(std::time::Duration::from_secs(1), run_task)
            .await
            .expect("adapter did not shut down after handle was dropped")
            .unwrap();
    }
}
