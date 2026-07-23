//! Steering channel — allows injecting messages into a running sub-agent's
//! model loop between turns.
//!
//! The parent session pushes steering messages via [`SteeringTx::send`].
//! The executing task drains them via [`SteeringRx::drain`] between model turns,
//! injecting them as additional user messages into the conversation context.

use std::sync::Arc;
use tokio::sync::Mutex;

/// Sender half — held by the SubAgentManager for injecting steering messages.
#[derive(Debug, Clone)]
pub struct SteeringTx {
    queue: Arc<Mutex<Vec<String>>>,
}

/// Receiver half — held by the executing broker task to drain pending messages.
#[derive(Debug, Clone)]
pub struct SteeringRx {
    queue: Arc<Mutex<Vec<String>>>,
}

/// Create a linked steering channel pair.
pub fn channel() -> (SteeringTx, SteeringRx) {
    let queue = Arc::new(Mutex::new(Vec::new()));
    (
        SteeringTx {
            queue: Arc::clone(&queue),
        },
        SteeringRx { queue },
    )
}

impl SteeringTx {
    /// Push a steering message into the channel.
    pub async fn send(&self, message: String) {
        self.queue.lock().await.push(message);
    }
}

impl SteeringRx {
    /// Drain all pending steering messages. Returns an empty vec if none are queued.
    pub async fn drain(&self) -> Vec<String> {
        let mut q = self.queue.lock().await;
        std::mem::take(&mut *q)
    }

    /// Check if there are pending messages without consuming them.
    pub async fn has_pending(&self) -> bool {
        !self.queue.lock().await.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_and_drain() {
        let (tx, rx) = channel();
        tx.send("hello".into()).await;
        tx.send("world".into()).await;

        let msgs = rx.drain().await;
        assert_eq!(msgs, vec!["hello", "world"]);

        // Second drain should be empty.
        let msgs = rx.drain().await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn has_pending_reflects_state() {
        let (tx, rx) = channel();
        assert!(!rx.has_pending().await);

        tx.send("msg".into()).await;
        assert!(rx.has_pending().await);

        rx.drain().await;
        assert!(!rx.has_pending().await);
    }

    #[tokio::test]
    async fn clone_tx_shares_queue() {
        let (tx, rx) = channel();
        let tx2 = tx.clone();

        tx.send("from tx1".into()).await;
        tx2.send("from tx2".into()).await;

        let msgs = rx.drain().await;
        assert_eq!(msgs.len(), 2);
    }
}
