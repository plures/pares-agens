//! First-run wizard — backend IPC commands.
//!
//! Provides:
//! - [`detect_docker_runner`] — TCP probe for Docker Model Runner at `localhost:12434`
//! - [`validate_api_key`]     — validates a cloud-provider API key via a models-list request
//! - [`is_wizard_completed`]  — returns whether the wizard has been completed this session
//! - [`complete_wizard`]      — marks the wizard as completed and applies wizard settings
//!
//! Durable completion state ("never show again") is persisted by the frontend
//! using `localStorage`; the backend flag covers in-process checks only.

use std::time::Duration;

use pares_agens_core::memory::entry::{MemoryCategory, MemoryEntry};
use pares_agens_core::memory::store::{
    generate_sync_shared_key, generate_sync_topic_key_hex, parse_sync_topic_key_hex,
    validate_sync_shared_key, MemoryStore, PluresDbStore,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::warn;

use crate::state::{rebuild_model_router, AppState, Settings, SwarmSettings};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Port used by Docker Model Runner's OpenAI-compatible endpoint.
const DOCKER_RUNNER_ADDR: &str = "127.0.0.1:12434";
const SWARM_SHARED_KEY_SECRET: &str = "swarm:shared_key";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmInvite {
    pub topic: String,
    pub shared_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmSetupInput {
    pub mode: String,
    pub topic: String,
    pub shared_key: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Endpoint URLs for the models-list API of each supported cloud provider.
fn models_endpoint(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("https://api.openai.com/v1/models"),
        "anthropic" => Some("https://api.anthropic.com/v1/models"),
        "google" => Some("https://generativelanguage.googleapis.com/v1beta/models"),
        _ => None,
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Check whether Docker Model Runner is accessible at `localhost:12434`.
///
/// Performs a non-blocking TCP connect with a one-second timeout.
/// Returns `true` on success, `false` otherwise.
#[tauri::command]
pub async fn detect_docker_runner() -> Result<bool, String> {
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(DOCKER_RUNNER_ADDR),
    )
    .await;

    match result {
        Ok(Ok(_)) => Ok(true),
        Ok(Err(e)) => {
            warn!("Docker Model Runner not reachable: {e}");
            Ok(false)
        }
        Err(_) => {
            warn!("Docker Model Runner probe timed out");
            Ok(false)
        }
    }
}

/// Validate an API key for a cloud model provider.
///
/// Hits the provider's models-list endpoint using the supplied key as a
/// Bearer token (or `x-api-key` header for Anthropic).
///
/// Returns:
/// - `Ok(true)`  — key is valid (HTTP 2xx)
/// - `Ok(false)` — key is invalid (HTTP 401 / 403)
/// - `Err(_)`    — provider error or network failure (retryable)
#[tauri::command]
pub async fn validate_api_key(provider: String, api_key: String) -> Result<bool, String> {
    let url = models_endpoint(&provider).ok_or_else(|| format!("Unknown provider: {provider}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let req = if provider == "anthropic" {
        client
            .get(url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
    } else {
        client.get(url).bearer_auth(&api_key)
    };

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    match resp.status().as_u16() {
        200..=299 => Ok(true),
        401 | 403 => Ok(false),
        status => Err(format!("Provider returned HTTP {status}")),
    }
}

/// Return whether the first-run wizard has been completed in this process.
#[tauri::command]
pub async fn is_wizard_completed(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(*state.wizard_completed.lock().await)
}

/// Generate first-run Hyperswarm pairing material for a brand-new swarm.
#[tauri::command]
pub async fn generate_swarm_invite() -> Result<SwarmInvite, String> {
    Ok(SwarmInvite {
        topic: generate_sync_topic_key_hex(),
        shared_key: generate_sync_shared_key().map_err(|e| e.to_string())?,
    })
}

/// Verify that supplied swarm topic + shared key can decrypt a synced record.
#[tauri::command]
pub async fn verify_swarm_join(topic: String, shared_key: String) -> Result<(), String> {
    let topic_key = parse_sync_topic_key_hex(&topic).map_err(|e| e.to_string())?;
    validate_sync_shared_key(&shared_key).map_err(|e| e.to_string())?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!(
        "pares-radix-swarm-verify-{}-{stamp}",
        std::process::id()
    ));
    let dir_a = base.join("a");
    let dir_b = base.join("b");
    std::fs::create_dir_all(&dir_a).map_err(|e| format!("failed to create probe dir A: {e}"))?;
    std::fs::create_dir_all(&dir_b).map_err(|e| format!("failed to create probe dir B: {e}"))?;

    let result = async {
        let store_a =
            PluresDbStore::open_with_sync(&dir_a, &topic_key, &shared_key).map_err(|e| e.to_string())?;
        let store_b =
            PluresDbStore::open_with_sync(&dir_b, &topic_key, &shared_key).map_err(|e| e.to_string())?;

        let probe_id = format!("swarm-probe-{}", stamp);
        store_a
            .insert(MemoryEntry {
                id: probe_id.clone(),
                content: "swarm verification probe".to_string(),
                category: MemoryCategory::Decision,
                tags: vec!["wizard".to_string(), "swarm-verify".to_string()],
                embedding: vec![0.1_f32, 0.2, 0.3],
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            })
            .await
            .map_err(|e| e.to_string())?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let entries = store_b.all().await.map_err(|e| e.to_string())?;
            if entries.iter().any(|entry| entry.id == probe_id) {
                break Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                break Err(
                    "Unable to verify swarm credentials. Check the topic and shared key, then try again."
                        .to_string(),
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    .await;

    let _ = std::fs::remove_dir_all(&base);
    result
}

/// Mark the wizard as completed and persist the chosen settings.
///
/// The frontend is responsible for writing `localStorage("wizard_completed")`
/// so that the wizard is suppressed on the next launch without an IPC call.
#[tauri::command]
pub async fn complete_wizard(
    mut settings: Settings,
    swarm: Option<SwarmSetupInput>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(swarm) = swarm {
        let mode = swarm.mode.trim().to_lowercase();
        if mode != "new" && mode != "join" {
            return Err("swarm mode must be either 'new' or 'join'".to_string());
        }

        parse_sync_topic_key_hex(&swarm.topic).map_err(|e| e.to_string())?;
        validate_sync_shared_key(&swarm.shared_key).map_err(|e| e.to_string())?;
        state
            .secret_store
            .set(SWARM_SHARED_KEY_SECRET, swarm.shared_key.trim())
            .await
            .map_err(|e| e.to_string())?;
        settings.swarm = Some(SwarmSettings {
            mode,
            topic: swarm.topic.trim().to_string(),
        });
    } else {
        settings.swarm = None;
        state
            .secret_store
            .delete(SWARM_SHARED_KEY_SECRET)
            .await
            .map_err(|e| e.to_string())?;
    }

    *state.settings.lock().await = settings;
    *state.wizard_completed.lock().await = true;

    // Rebuild the model router so wizard-entered provider settings take
    // effect on the very first message without requiring a separate
    // settings mutation.
    rebuild_model_router(&state).await;

    Ok(())
}
