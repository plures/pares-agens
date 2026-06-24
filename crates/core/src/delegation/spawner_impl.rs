//! Cognition → platform bridge: implement the platform [`SubAgentSpawner`]
//! seam for the cognition [`SubAgentManager`].
//!
//! This is the `COGNITION -> PLATFORM` direction (allowed): cognition depends
//! on the platform-owned trait/DTOs in [`pares_radix_core::subagent_spawn`] and adapts its
//! own internal session model onto them. Platform code (`spine::subagent_actor`)
//! depends only on the trait, never on `delegation`.
//!
//! The conversion maps the delegation-internal [`SpawnOptions`] /
//! [`SessionStatus`] / [`SessionInfo`] onto the platform DTOs. Delegation's own
//! types are left untouched — other callers continue to use them directly.

use async_trait::async_trait;

use crate::delegation::manager::{SessionStatus, SpawnOptions, SubAgentManager};
use pares_radix_core::subagent_spawn::{
    SessionStatus as PlatSessionStatus, SpawnOptions as PlatSpawnOptions, SpawnedInfo,
    SubAgentSpawner,
};

/// Convert platform spawn options into the delegation-internal options.
fn to_internal_options(opts: PlatSpawnOptions) -> SpawnOptions {
    SpawnOptions {
        label: opts.label,
        timeout: opts.timeout,
        parent_context: opts.parent_context,
    }
}

/// Convert a delegation-internal session status into the platform status.
fn to_platform_status(status: &SessionStatus) -> PlatSessionStatus {
    match status {
        SessionStatus::Running => PlatSessionStatus::Running,
        SessionStatus::Completed => PlatSessionStatus::Completed,
        SessionStatus::Failed(e) => PlatSessionStatus::Failed(e.clone()),
        SessionStatus::TimedOut => PlatSessionStatus::TimedOut,
        SessionStatus::Killed => PlatSessionStatus::Killed,
    }
}

#[async_trait]
impl SubAgentSpawner for SubAgentManager {
    async fn spawn(&self, agent: &str, prompt: &str, options: PlatSpawnOptions) -> String {
        SubAgentManager::spawn(self, agent, prompt, to_internal_options(options)).await
    }

    async fn get(&self, session_id: &str) -> Option<SpawnedInfo> {
        SubAgentManager::get(self, session_id).await.map(|info| SpawnedInfo {
            status: to_platform_status(&info.status),
            output: info.output,
        })
    }
}
