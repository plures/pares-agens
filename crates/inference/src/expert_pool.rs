//! CPU expert pool for multi-model BitNet inference.
//!
//! Provides:
//! - Expert-role routing (deployment/routing/compliance/monitoring/capacity)
//! - Shared KV cache accounting across experts
//! - RAM-aware admission to avoid OOM loading

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use crate::{client::ModelClient, error::InferenceError, params::GenParams};

/// Domain-specialized BitNet expert roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CpuExpert {
    /// Deployment and release operations.
    Deployment,
    /// Routing and traffic-management questions.
    Routing,
    /// Compliance and policy questions.
    Compliance,
    /// Monitoring and observability questions.
    Monitoring,
    /// Capacity planning and sizing questions.
    Capacity,
}

impl CpuExpert {
    /// Return the stable lowercase identifier for this expert role.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deployment => "deployment",
            Self::Routing => "routing",
            Self::Compliance => "compliance",
            Self::Monitoring => "monitoring",
            Self::Capacity => "capacity",
        }
    }
}

/// CPU expert-pool configuration.
#[derive(Debug, Clone)]
pub struct CpuExpertPoolConfig {
    /// Total RAM budget available to the expert pool, in MiB.
    pub ram_budget_mb: u64,
    /// Maximum number of experts loaded concurrently.
    pub max_experts: usize,
    /// RAM reserved for the shared KV cache, in MiB.
    pub kv_cache_mb: u64,
}

impl Default for CpuExpertPoolConfig {
    fn default() -> Self {
        Self {
            // 128 GiB server class default.
            ram_budget_mb: 131_072,
            // Practical target from issue guidance.
            max_experts: 8,
            kv_cache_mb: 8_192,
        }
    }
}

#[derive(Clone)]
struct LoadedExpert {
    model: Arc<dyn ModelClient>,
    ram_mb: u64,
}

/// Shared KV cache budget manager across all experts.
#[derive(Debug)]
pub struct SharedKvCacheManager {
    budget_mb: u64,
    allocations: Mutex<HashMap<u64, u64>>,
    request_counter: AtomicU64,
}

impl SharedKvCacheManager {
    /// Create a manager with the given total budget, in MiB.
    pub fn new(budget_mb: u64) -> Self {
        Self {
            budget_mb,
            allocations: Mutex::new(HashMap::new()),
            request_counter: AtomicU64::new(1),
        }
    }

    fn allocate(&self, needed_mb: u64) -> Result<u64, InferenceError> {
        if needed_mb == 0 {
            return Ok(0);
        }

        let mut allocations = self
            .allocations
            .lock()
            .expect("kv cache allocation mutex poisoned");
        let used_mb: u64 = allocations.values().copied().sum();
        let available_mb = self.budget_mb.saturating_sub(used_mb);

        if needed_mb > available_mb {
            return Err(InferenceError::KvCacheExhausted {
                needed_mb,
                available_mb,
            });
        }

        let request_id = self.request_counter.fetch_add(1, Ordering::Relaxed);
        allocations.insert(request_id, needed_mb);
        Ok(request_id)
    }

    fn free(&self, request_id: u64) {
        if request_id == 0 {
            return;
        }

        let mut allocations = self
            .allocations
            .lock()
            .expect("kv cache allocation mutex poisoned");
        allocations.remove(&request_id);
    }

    /// Current KV cache usage in MiB.
    pub fn used_mb(&self) -> u64 {
        let allocations = self
            .allocations
            .lock()
            .expect("kv cache allocation mutex poisoned");
        allocations.values().copied().sum()
    }

    /// Remaining KV cache capacity in MiB.
    pub fn available_mb(&self) -> u64 {
        self.budget_mb.saturating_sub(self.used_mb())
    }
}

/// CPU pool for loading and running multiple specialized BitNet experts.
pub struct CpuExpertPool {
    config: CpuExpertPoolConfig,
    loaded: HashMap<CpuExpert, LoadedExpert>,
    ram_used_mb: u64,
    kv_cache: Arc<SharedKvCacheManager>,
}

impl std::fmt::Debug for CpuExpertPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuExpertPool")
            .field("config", &self.config)
            .field("loaded_experts", &self.loaded.len())
            .field("ram_used_mb", &self.ram_used_mb)
            .field("kv_cache_used_mb", &self.kv_cache.used_mb())
            .finish()
    }
}

impl CpuExpertPool {
    /// Create an empty CPU expert pool.
    pub fn new(config: CpuExpertPoolConfig) -> Self {
        let kv_cache = Arc::new(SharedKvCacheManager::new(config.kv_cache_mb));
        Self {
            config,
            loaded: HashMap::new(),
            ram_used_mb: 0,
            kv_cache,
        }
    }

    /// RAM available for expert model weights, in MiB.
    pub fn ram_available_mb(&self) -> u64 {
        self.config
            .ram_budget_mb
            .saturating_sub(self.config.kv_cache_mb)
            .saturating_sub(self.ram_used_mb)
    }

    /// RAM consumed by loaded expert model weights, in MiB.
    pub fn ram_used_mb(&self) -> u64 {
        self.ram_used_mb
    }

    /// Access the shared KV cache manager.
    pub fn kv_cache(&self) -> &SharedKvCacheManager {
        self.kv_cache.as_ref()
    }

    /// Load an expert model with the estimated RAM footprint.
    pub fn load_expert(
        &mut self,
        expert: CpuExpert,
        model: Arc<dyn ModelClient>,
        ram_mb: u64,
    ) -> Result<(), InferenceError> {
        if self.loaded.contains_key(&expert) {
            return Err(InferenceError::ExpertAlreadyLoaded {
                expert: expert.as_str().to_owned(),
            });
        }

        if self.loaded.len() >= self.config.max_experts {
            return Err(InferenceError::ExpertPoolFull {
                max_experts: self.config.max_experts,
            });
        }

        let available_mb = self.ram_available_mb();
        if ram_mb > available_mb {
            return Err(InferenceError::InsufficientRam {
                needed_mb: ram_mb,
                available_mb,
            });
        }

        self.loaded.insert(expert, LoadedExpert { model, ram_mb });
        self.ram_used_mb += ram_mb;
        Ok(())
    }

    /// Unload an expert model.
    pub fn unload_expert(&mut self, expert: CpuExpert) {
        if let Some(loaded) = self.loaded.remove(&expert) {
            self.ram_used_mb = self.ram_used_mb.saturating_sub(loaded.ram_mb);
        }
    }

    /// Check whether an expert is loaded.
    pub fn is_loaded(&self, expert: CpuExpert) -> bool {
        self.loaded.contains_key(&expert)
    }

    /// Route a query to the most relevant expert role.
    pub fn route_query(query: &str) -> CpuExpert {
        let lower = query.to_ascii_lowercase();

        if contains_any(&lower, &["policy", "compliance", "audit", "soc2", "gdpr", "regulat"]) {
            return CpuExpert::Compliance;
        }

        if contains_any(
            &lower,
            &["monitor", "observability", "metrics", "alerts", "logs", "tracing"],
        ) {
            return CpuExpert::Monitoring;
        }

        if contains_any(
            &lower,
            &["capacity", "sizing", "throughput", "scale", "oom", "memory", "cpu"],
        ) {
            return CpuExpert::Capacity;
        }

        if contains_any(
            &lower,
            &["route", "routing", "traffic", "load balancer", "ingress", "gateway"],
        ) {
            return CpuExpert::Routing;
        }

        if contains_any(
            &lower,
            &["deploy", "deployment", "release", "rollout", "kubernetes", "helm", "canary"],
        ) {
            return CpuExpert::Deployment;
        }

        // Default to routing when ambiguous.
        CpuExpert::Routing
    }

    /// Infer by automatically routing the prompt to an expert.
    pub async fn infer(
        &self,
        prompt: &str,
        params: GenParams,
        kv_cache_mb: u64,
    ) -> Result<(CpuExpert, String), InferenceError> {
        let expert = Self::route_query(prompt);
        let output = self.infer_with_expert(expert, prompt, params, kv_cache_mb).await?;
        Ok((expert, output))
    }

    /// Infer using a specific expert role.
    pub async fn infer_with_expert(
        &self,
        expert: CpuExpert,
        prompt: &str,
        params: GenParams,
        kv_cache_mb: u64,
    ) -> Result<String, InferenceError> {
        let loaded = self
            .loaded
            .get(&expert)
            .ok_or_else(|| InferenceError::ExpertNotLoaded {
                expert: expert.as_str().to_owned(),
            })?
            .clone();

        let request_id = self.kv_cache.allocate(kv_cache_mb)?;
        let _reservation = KvCacheReservation::new(self.kv_cache.as_ref(), request_id);
        complete_from_client(&loaded.model, prompt, params).await
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

struct KvCacheReservation<'a> {
    manager: &'a SharedKvCacheManager,
    request_id: u64,
}

impl<'a> KvCacheReservation<'a> {
    fn new(manager: &'a SharedKvCacheManager, request_id: u64) -> Self {
        Self {
            manager,
            request_id,
        }
    }
}

impl Drop for KvCacheReservation<'_> {
    fn drop(&mut self) {
        self.manager.free(self.request_id);
    }
}

async fn complete_from_client(
    model: &Arc<dyn ModelClient>,
    prompt: &str,
    params: GenParams,
) -> Result<String, InferenceError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    model.generate(prompt, params, tx).await?;

    let mut output = String::new();
    while let Some(piece) = rx.recv().await {
        output.push_str(&piece?);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::time::Duration;

    struct MockModelClient {
        id: String,
        prefix: String,
        delay_ms: u64,
    }

    impl MockModelClient {
        fn new(id: &str, prefix: &str, delay_ms: u64) -> Self {
            Self {
                id: id.to_owned(),
                prefix: prefix.to_owned(),
                delay_ms,
            }
        }
    }

    #[async_trait]
    impl ModelClient for MockModelClient {
        fn model_id(&self) -> &str {
            &self.id
        }

        async fn generate(
            &self,
            prompt: &str,
            _params: GenParams,
            tx: crate::client::TokenSender,
        ) -> Result<(), InferenceError> {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            tx.send(Ok(format!("{}:{prompt}", self.prefix)))
                .await
                .map_err(|_| InferenceError::ChannelClosed)?;
            Ok(())
        }
    }

    fn small_config() -> CpuExpertPoolConfig {
        CpuExpertPoolConfig {
            ram_budget_mb: 12_000,
            max_experts: 2,
            kv_cache_mb: 1_000,
        }
    }

    fn make_model(id: &str, prefix: &str, delay_ms: u64) -> Arc<dyn ModelClient> {
        Arc::new(MockModelClient::new(id, prefix, delay_ms))
    }

    #[test]
    fn routing_selects_expected_experts() {
        assert_eq!(
            CpuExpertPool::route_query("check SOC2 compliance controls"),
            CpuExpert::Compliance
        );
        assert_eq!(
            CpuExpertPool::route_query("set up monitoring alerts and tracing"),
            CpuExpert::Monitoring
        );
        assert_eq!(
            CpuExpertPool::route_query("capacity planning for memory and cpu"),
            CpuExpert::Capacity
        );
        assert_eq!(
            CpuExpertPool::route_query("route traffic through the ingress gateway"),
            CpuExpert::Routing
        );
        assert_eq!(
            CpuExpertPool::route_query("deployment rollout with kubernetes"),
            CpuExpert::Deployment
        );
    }

    #[test]
    fn load_expert_rejects_over_budget_model() {
        let mut pool = CpuExpertPool::new(small_config());
        let err = pool
            .load_expert(
                CpuExpert::Deployment,
                make_model("dep", "dep", 0),
                20_000, // exceeds available budget
            )
            .unwrap_err();
        assert!(matches!(err, InferenceError::InsufficientRam { .. }));
    }

    #[tokio::test]
    async fn infer_routes_and_uses_loaded_expert() {
        let mut pool = CpuExpertPool::new(small_config());
        pool.load_expert(
            CpuExpert::Compliance,
            make_model("compliance-8b", "compliance", 0),
            1_600,
        )
        .unwrap();

        let (expert, output) = pool
            .infer(
                "Need compliance guidance for SOC2 policy controls",
                GenParams::default(),
                128,
            )
            .await
            .unwrap();

        assert_eq!(expert, CpuExpert::Compliance);
        assert!(output.contains("compliance:"));
        assert_eq!(pool.kv_cache().used_mb(), 0, "KV cache should be released");
    }

    #[tokio::test]
    async fn infer_concurrently_across_experts() {
        let mut pool = CpuExpertPool::new(CpuExpertPoolConfig {
            ram_budget_mb: 16_000,
            max_experts: 5,
            kv_cache_mb: 2_000,
        });

        pool.load_expert(
            CpuExpert::Deployment,
            make_model("deployment-8b", "deploy", 20),
            1_600,
        )
        .unwrap();
        pool.load_expert(
            CpuExpert::Monitoring,
            make_model("monitoring-8b", "monitor", 20),
            1_600,
        )
        .unwrap();

        let deploy = pool.infer_with_expert(
            CpuExpert::Deployment,
            "deploy now",
            GenParams::default(),
            256,
        );
        let monitor = pool.infer_with_expert(
            CpuExpert::Monitoring,
            "monitor now",
            GenParams::default(),
            256,
        );

        let (deploy_out, monitor_out) = tokio::join!(deploy, monitor);
        assert!(deploy_out.unwrap().contains("deploy:"));
        assert!(monitor_out.unwrap().contains("monitor:"));
        assert_eq!(pool.kv_cache().used_mb(), 0, "all KV reservations must be freed");
    }
}
