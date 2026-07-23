//! Per-chat active-turn registry — the shared seam for mid-turn steering (S2)
//! and control buttons (S3).
//!
//! Both features need to reach the *currently running turn for a chat*. A
//! [`TurnHandle`] carries the real [`SteeringTx`] (reused from
//! `pares_agens_core::delegation::steering`, not reinvented), a
//! [`CancellationToken`] for cooperative stop, and the turn's `request_id`.
//!
//! Concurrency choice: `Arc<Mutex<HashMap<ChatId, TurnHandle>>>` rather than
//! `dashmap`. Justification — registrations are keyed per chat and are already
//! serialized by teloxide's per-chat dispatch; the map is small and touched
//! only at turn start/end and on the occasional callback, so a single async
//! mutex has negligible contention and avoids adding a dependency. (If profiling
//! ever shows lock contention, swapping to `DashMap` is a drop-in change behind
//! this type's API.)

use std::collections::HashMap;
use std::sync::Arc;

use pares_agens_core::delegation::steering::{channel as steering_channel, SteeringRx, SteeringTx};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Telegram chat id (raw `i64`, matching `msg.chat.id.0`).
pub type ChatId = i64;

/// Handle to a live turn for one chat. Cloneable so the message branch and the
/// callback branch can both act on the same turn.
#[derive(Debug, Clone)]
pub struct TurnHandle {
    /// Injects a mid-turn user message into the running turn (option b: steer,
    /// don't restart).
    pub steering_tx: SteeringTx,
    /// Cooperative cancel for the Stop button.
    pub cancel: CancellationToken,
    /// The turn's request_id (the inbound message id, `chat_id:msg_id` form),
    /// used to target `stop:{request_id}` callbacks at the right turn.
    pub request_id: String,
}

/// Concurrency-safe per-chat registry of live turns.
#[derive(Debug, Clone, Default)]
pub struct ActiveTurns {
    inner: Arc<Mutex<HashMap<ChatId, TurnHandle>>>,
}

/// Outcome of routing an inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A live turn exists — steer it (message injected via SteeringTx).
    Steer {
        /// The live turn's request_id.
        request_id: String,
    },
    /// No live turn — start a new turn.
    NewTurn,
}

impl ActiveTurns {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new live turn for `chat`, returning the [`SteeringRx`] the
    /// turn driver must drain to receive steered messages. Replaces (and thus
    /// supersedes) any stale handle for the same chat.
    pub async fn register(
        &self,
        chat: ChatId,
        cancel: CancellationToken,
        request_id: impl Into<String>,
    ) -> SteeringRx {
        let (tx, rx) = steering_channel();
        let handle = TurnHandle {
            steering_tx: tx,
            cancel,
            request_id: request_id.into(),
        };
        self.inner.lock().await.insert(chat, handle);
        rx
    }

    /// Look up the live turn for `chat`, if any.
    pub async fn get(&self, chat: ChatId) -> Option<TurnHandle> {
        self.inner.lock().await.get(&chat).cloned()
    }

    /// Remove (deregister) the live turn for `chat`.
    pub async fn remove(&self, chat: ChatId) {
        self.inner.lock().await.remove(&chat);
    }

    /// Number of live turns (test/introspection helper).
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// Whether no turns are live.
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    /// Decide how to route an inbound message for `chat`: steer the live turn
    /// or start a new one. Pure routing decision (drives the S2 policy).
    pub async fn route(&self, chat: ChatId) -> Route {
        match self.get(chat).await {
            Some(h) => Route::Steer {
                request_id: h.request_id,
            },
            None => Route::NewTurn,
        }
    }

    /// If a live turn exists for `chat`, inject `text` into it via steering and
    /// return `true`. Otherwise return `false` (caller starts a new turn).
    pub async fn steer(&self, chat: ChatId, text: impl Into<String>) -> bool {
        if let Some(h) = self.get(chat).await {
            h.steering_tx.send(text.into()).await;
            true
        } else {
            false
        }
    }

    /// Cancel the live turn for `chat` whose request_id matches `request_id`.
    /// Returns `true` if a matching live turn was cancelled.
    pub async fn cancel(&self, chat: ChatId, request_id: &str) -> bool {
        if let Some(h) = self.get(chat).await {
            if h.request_id == request_id {
                h.cancel.cancel();
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_lookup_remove() {
        let reg = ActiveTurns::new();
        assert!(reg.is_empty().await);

        let token = CancellationToken::new();
        let _rx = reg.register(42, token.clone(), "42:100").await;
        assert_eq!(reg.len().await, 1);

        let h = reg.get(42).await.expect("turn registered");
        assert_eq!(h.request_id, "42:100");

        reg.remove(42).await;
        assert!(reg.get(42).await.is_none());
        assert!(reg.is_empty().await);
    }

    #[tokio::test]
    async fn route_live_steers_none_starts_new() {
        let reg = ActiveTurns::new();
        // No turn → new.
        assert_eq!(reg.route(7).await, Route::NewTurn);

        let _rx = reg.register(7, CancellationToken::new(), "7:1").await;
        assert_eq!(
            reg.route(7).await,
            Route::Steer {
                request_id: "7:1".into()
            }
        );
    }

    #[tokio::test]
    async fn steer_injects_into_running_turn() {
        let reg = ActiveTurns::new();
        let rx = reg.register(5, CancellationToken::new(), "5:1").await;

        // Live turn → steered.
        assert!(reg.steer(5, "adapt to this").await);
        // No turn for a different chat → not steered.
        assert!(!reg.steer(999, "nobody home").await);

        // The steered message is really in the turn's SteeringRx.
        let drained = rx.drain().await;
        assert_eq!(drained, vec!["adapt to this".to_string()]);
    }

    #[tokio::test]
    async fn cancel_matches_request_id_only() {
        let reg = ActiveTurns::new();
        let token = CancellationToken::new();
        let _rx = reg.register(3, token.clone(), "3:1").await;

        // Wrong request_id → no cancel.
        assert!(!reg.cancel(3, "3:999").await);
        assert!(!token.is_cancelled());

        // Right request_id → cancels.
        assert!(reg.cancel(3, "3:1").await);
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn concurrent_access_is_safe() {
        let reg = ActiveTurns::new();
        let mut handles = Vec::new();
        for i in 0..50i64 {
            let r = reg.clone();
            handles.push(tokio::spawn(async move {
                let _rx = r.register(i, CancellationToken::new(), format!("{i}:1")).await;
                let _ = r.get(i).await;
                r.remove(i).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert!(reg.is_empty().await);
    }

    #[tokio::test]
    async fn re_register_supersedes_stale_handle() {
        let reg = ActiveTurns::new();
        let _rx1 = reg.register(1, CancellationToken::new(), "1:1").await;
        let _rx2 = reg.register(1, CancellationToken::new(), "1:2").await;
        assert_eq!(reg.len().await, 1);
        assert_eq!(reg.get(1).await.unwrap().request_id, "1:2");
    }
}
