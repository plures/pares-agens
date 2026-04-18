use std::sync::Arc;

use pares_agens_core::telemetry::{TelemetryAggregate, TelemetrySnapshot};
use pares_agens_core::StateStore;
use tokio::sync::Mutex;

const TELEMETRY_KEY: &str = "telemetry.aggregate.v1";

pub struct TelemetryService {
    store: Arc<dyn StateStore>,
    write_lock: Mutex<()>,
}

impl TelemetryService {
    pub fn new(store: Arc<dyn StateStore>) -> Self {
        Self {
            store,
            write_lock: Mutex::new(()),
        }
    }

    pub async fn record_model_call(&self, latency_ms: u64) {
        let _guard = self.write_lock.lock().await;
        let mut aggregate = self.load().await;
        aggregate.record_model_call(latency_ms);
        self.save(&aggregate).await;
    }

    pub async fn record_tool_usage(&self, tool_name: &str) {
        let _guard = self.write_lock.lock().await;
        let mut aggregate = self.load().await;
        aggregate.record_tool_usage(tool_name);
        self.save(&aggregate).await;
    }

    pub async fn snapshot(&self) -> TelemetrySnapshot {
        self.load().await.snapshot()
    }

    pub async fn mark_uploaded(&self) {
        let _guard = self.write_lock.lock().await;
        let mut aggregate = self.load().await;
        aggregate.mark_uploaded_now();
        self.save(&aggregate).await;
    }

    async fn load(&self) -> TelemetryAggregate {
        self.store
            .get(TELEMETRY_KEY)
            .await
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    async fn save(&self, aggregate: &TelemetryAggregate) {
        if let Ok(value) = serde_json::to_value(aggregate) {
            self.store.set(TELEMETRY_KEY, value).await;
        }
    }
}
