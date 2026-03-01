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
    Ok(matches!(result, Ok(Ok(_))))
}

/// Validate a cloud provider API key by probing the provider's models endpoint.
///
/// Supported `provider` values: `"openai"`, `"anthropic"`, `"google"`.
///
/// Returns `true` when the server responds with HTTP 2xx (key accepted),
/// `false` when it responds with 4xx (key rejected), or an `Err` on network
/// failure.
///
/// The API key is used only for the validation request and is never stored or
/// logged by this function.
#[tauri::command]
pub async fn validate_api_key(provider: String, api_key: String) -> Result<bool, String> {
    let url = match models_endpoint(&provider) {
        Some(u) => u,
        None => {
            warn!("validate_api_key: unknown provider {:?}", provider);
            return Err(format!("unknown provider: {provider}"));
        }
    };

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let req = match provider.as_str() {
        "openai" => client
            .get(url)
            .header("Authorization", format!("Bearer {api_key}")),
        "anthropic" => client
            .get(url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01"),
        "google" => client.get(url).query(&[("key", &api_key)]),
        _ => unreachable!("models_endpoint already filtered unknown providers"),
    };

    let status = req.send().await.map_err(|e| e.to_string())?.status();
    Ok(status.is_success())
}

/// Return whether the first-run wizard has been completed in this session.
#[tauri::command]
pub async fn is_wizard_completed(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(*state.wizard_completed.lock().await)
}

/// Mark the wizard as completed and apply the final wizard settings.
///
/// `settings` contains the model/endpoint/system-prompt choices collected
/// during the wizard flow.
#[tauri::command]
pub async fn complete_wizard(
    settings: Settings,
    state: State<'_, AppState>,
) -> Result<(), String> {
    *state.settings.lock().await = settings;
    *state.wizard_completed.lock().await = true;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_endpoint_known_providers() {
        assert!(models_endpoint("openai").is_some());
        assert!(models_endpoint("anthropic").is_some());
        assert!(models_endpoint("google").is_some());
    }

    #[test]
    fn models_endpoint_unknown_provider() {
        assert!(models_endpoint("unknown").is_none());
        assert!(models_endpoint("").is_none());
    }

    #[test]
    fn models_endpoint_returns_https_urls() {
        for p in ["openai", "anthropic", "google"] {
            let url = models_endpoint(p).unwrap();
            assert!(url.starts_with("https://"), "expected https for {p}");
        }
    }

    #[tokio::test]
    async fn detect_docker_runner_returns_bool() {
        // We do not assert the result since Docker Model Runner may or may not
        // be running in the test environment; we only verify the command does
        // not panic and returns a valid Result.
        let result = detect_docker_runner().await;
        assert!(result.is_ok());
    }
}
