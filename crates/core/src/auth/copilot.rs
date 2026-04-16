//! GitHub Copilot device flow authentication and model client.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::model::{ChatMessage, ChatOptions, ModelClient, ModelCompletion, ToolDefinition, ToolCall};

const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const OAUTH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const DEFAULT_API_BASE: &str = "https://api.individual.githubcopilot.com";
const EDITOR_VERSION: &str = "vscode/1.96.2";
const USER_AGENT: &str = "GitHubCopilotChat/0.26.7";
const API_VERSION: &str = "2025-04-01";
const INTEGRATION_ID: &str = "vscode-chat";

/// Errors emitted during Copilot authentication or token refresh.
#[derive(Debug, Error)]
pub enum CopilotAuthError {
    /// HTTP request failed.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// JSON serialization/deserialization failed.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// Response was missing required fields.
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    /// OAuth endpoint returned an error.
    #[error("oauth error: {0}")]
    OAuth(String),
}

/// Tracks OAuth and Copilot session tokens.
#[derive(Debug, Clone)]
pub struct CopilotAuth {
    #[allow(dead_code)]
    client_id: String,
    oauth_token: Option<String>,
    session_token: Option<String>,
    session_expires_at: u64,
    api_base_url: String,
    #[allow(dead_code)]
    client: reqwest::Client,
}

impl CopilotAuth {
    /// Create a new Copilot auth state using an existing OAuth token.
    pub fn new(oauth_token: String) -> Self {
        Self {
            client_id: COPILOT_CLIENT_ID.to_string(),
            oauth_token: Some(oauth_token),
            session_token: None,
            session_expires_at: 0,
            api_base_url: DEFAULT_API_BASE.to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Start the Copilot device flow.
    pub async fn device_flow_start() -> Result<(String, String, String), CopilotAuthError> {
        #[derive(Deserialize)]
        struct DeviceCodeResponse {
            device_code: String,
            user_code: String,
            verification_uri: String,
        }

        let client = reqwest::Client::new();
        let response = client
            .post(DEVICE_CODE_URL)
            .header(ACCEPT, "application/json")
            .form(&[
                ("client_id", COPILOT_CLIENT_ID),
                ("scope", "copilot"),
            ])
            .send()
            .await?
            .error_for_status()?;

        let payload: DeviceCodeResponse = response.json().await?;
        Ok((
            payload.device_code,
            payload.user_code,
            payload.verification_uri,
        ))
    }

    /// Poll the device flow until an OAuth token is issued.
    pub async fn device_flow_poll(device_code: &str) -> Result<String, CopilotAuthError> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: Option<String>,
            error: Option<String>,
            error_description: Option<String>,
        }

        let client = reqwest::Client::new();
        let mut interval = Duration::from_secs(5);
        loop {
            let response = client
                .post(OAUTH_TOKEN_URL)
                .header(ACCEPT, "application/json")
                .form(&[
                    ("client_id", COPILOT_CLIENT_ID),
                    ("device_code", device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await?
                .error_for_status()?;

            let payload: TokenResponse = response.json().await?;
            if let Some(token) = payload.access_token {
                return Ok(token);
            }

            if let Some(error) = payload.error {
                match error.as_str() {
                    "authorization_pending" => {
                        sleep(interval).await;
                        continue;
                    }
                    "slow_down" => {
                        interval += Duration::from_secs(5);
                        sleep(interval).await;
                        continue;
                    }
                    _ => {
                        let detail = payload
                            .error_description
                            .unwrap_or_else(|| "unknown error".into());
                        return Err(CopilotAuthError::OAuth(format!(
                            "{error}: {detail}"
                        )));
                    }
                }
            }

            return Err(CopilotAuthError::InvalidResponse(
                "missing access_token".into(),
            ));
        }
    }

    /// Exchange the OAuth token for a Copilot session token.
    pub async fn exchange_copilot_token(
        oauth_token: &str,
    ) -> Result<(String, u64, String), CopilotAuthError> {
        #[derive(Deserialize)]
        struct CopilotTokenResponse {
            token: String,
            expires_at: Value,
        }

        let client = reqwest::Client::new();
        let response = client
            .get(COPILOT_TOKEN_URL)
            .header(AUTHORIZATION, format!("Bearer {oauth_token}"))
            .header("Editor-Version", EDITOR_VERSION)
            .header("User-Agent", USER_AGENT)
            .header("X-Github-Api-Version", API_VERSION)
            .send()
            .await?
            .error_for_status()?;

        let payload: CopilotTokenResponse = response.json().await?;
        let expires_at = match payload.expires_at {
            Value::Number(num) => num.as_u64().ok_or_else(|| {
                CopilotAuthError::InvalidResponse("invalid expires_at".into())
            })?,
            Value::String(s) => s.parse::<u64>().map_err(|_| {
                CopilotAuthError::InvalidResponse("invalid expires_at".into())
            })?,
            _ => {
                return Err(CopilotAuthError::InvalidResponse(
                    "invalid expires_at".into(),
                ))
            }
        };

        let api_base = extract_api_base_url(&payload.token)
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());

        Ok((payload.token, expires_at, api_base))
    }

    /// Ensure the session token is fresh; refresh if needed.
    pub async fn ensure_fresh_token(&mut self) -> Result<&str, CopilotAuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CopilotAuthError::InvalidResponse("time error".into()))?
            .as_secs();

        let needs_refresh = match self.session_token {
            Some(_) => now + 60 >= self.session_expires_at,
            None => true,
        };

        if needs_refresh {
            let oauth_token = self.oauth_token.clone().ok_or_else(|| {
                CopilotAuthError::InvalidResponse("missing oauth token".into())
            })?;
            let (session_token, expires_at, api_base) =
                Self::exchange_copilot_token(&oauth_token).await?;
            self.session_token = Some(session_token);
            self.session_expires_at = expires_at;
            self.api_base_url = api_base;
        }

        Ok(self
            .session_token
            .as_deref()
            .ok_or_else(|| CopilotAuthError::InvalidResponse("missing session token".into()))?)
    }

    /// Current API base URL derived from the session token.
    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }
}

/// Model client that talks directly to GitHub Copilot.
#[derive(Clone)]
pub struct CopilotModelClient {
    auth: Arc<Mutex<CopilotAuth>>,
    model: String,
    client: reqwest::Client,
}

impl CopilotModelClient {
    /// Create a Copilot model client for the given model.
    pub fn new(auth: CopilotAuth, model: impl Into<String>) -> Self {
        Self {
            auth: Arc::new(Mutex::new(auth)),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl ModelClient for CopilotModelClient {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ModelCompletion, String> {
        let (token, api_base) = {
            let mut auth = self.auth.lock().await;
            let token = auth.ensure_fresh_token().await.map_err(|e| e.to_string())?;
            (token.to_string(), auth.api_base_url().to_string())
        };

        let mut rendered_messages: Vec<Value> = Vec::with_capacity(messages.len());
        for message in messages {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), Value::String(message.role.clone()));
            obj.insert("content".into(), Value::String(message.content.clone()));
            if let Some(tool_call_id) = &message.tool_call_id {
                obj.insert("tool_call_id".into(), Value::String(tool_call_id.clone()));
            }
            if let Some(tool_calls) = &message.tool_calls {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|call| {
                        serde_json::json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.arguments.to_string(),
                            }
                        })
                    })
                    .collect();
                obj.insert("tool_calls".into(), Value::Array(calls));
            }
            rendered_messages.push(Value::Object(obj));
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": rendered_messages,
        });

        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = Value::Array(tool_defs);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = Value::Number(serde_json::Number::from_f64(temp).unwrap_or_else(|| serde_json::Number::from(0)));
        }
        if options.logprobs {
            body["logprobs"] = Value::Bool(true);
        }

        let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
        );
        headers.insert("Editor-Version", HeaderValue::from_static(EDITOR_VERSION));
        headers.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
        headers.insert("X-Github-Api-Version", HeaderValue::from_static(API_VERSION));
        headers.insert(
            "Copilot-Integration-Id",
            HeaderValue::from_static(INTEGRATION_ID),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        let payload: Value = response.json().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("copilot error ({status}): {payload}"));
        }

        let choice = payload
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .ok_or_else(|| "model returned no choices".to_string())?;

        let message = choice
            .get("message")
            .ok_or_else(|| "model returned no message".to_string())?;

        let content = message.get("content").and_then(|v| v.as_str()).map(|s| s.to_string());

        let tool_calls = message
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|call| {
                        let id = call.get("id")?.as_str()?.to_string();
                        let function = call.get("function")?;
                        let name = function.get("name")?.as_str()?.to_string();
                        let args_raw = function.get("arguments");
                        let args_value = args_raw
                            .and_then(|v| v.as_str())
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .unwrap_or_else(|| args_raw.cloned().unwrap_or(Value::Null));
                        Some(ToolCall {
                            id,
                            name,
                            arguments: args_value,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let logprobs = choice
            .get("logprobs")
            .and_then(|v| v.get("token_logprobs"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_f64())
                    .collect::<Vec<f64>>()
            });

        Ok(ModelCompletion {
            content,
            tool_calls,
            logprobs,
        })
    }
}

fn extract_api_base_url(token: &str) -> Option<String> {
    let mut parts = token.split('.');
    parts.next()?;
    let payload = parts.next()?;
    let decoded = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    let payload_json: Value = serde_json::from_slice(&decoded).ok()?;
    payload_json
        .get("vscu")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
