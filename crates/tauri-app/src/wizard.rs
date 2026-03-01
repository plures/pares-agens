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

use tauri::State;
use tracing::warn;

use crate::state::{AppState, Settings};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Port used by Docker Model Runner's OpenAI-compatible endpoint.
const DOCKER_RUNNER_ADDR: &str = "127.0.0.1:12434";

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
    let url = models_endpoint(&provider)
        .ok_or_else(|| format!("Unknown provider: {provider}"))?;

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

    let resp = req.send().await.map_err(|e| format!("Network error: {e}"))?;

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

/// Mark the wizard as completed and persist the chosen settings.
///
/// The frontend is responsible for writing `localStorage("wizard_completed")`
/// so that the wizard is suppressed on the next launch without an IPC call.
#[tauri::command]
pub async fn complete_wizard(
    settings: Settings,
    state: State<'_, AppState>,
) -> Result<(), String> {
    *state.settings.lock().await = settings;
    *state.wizard_completed.lock().await = true;
    Ok(())
}
