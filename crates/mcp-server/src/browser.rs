//! Browser automation via Chrome DevTools Protocol (CDP).
//!
//! Connects to a running Chrome/Chromium instance via its CDP HTTP and WebSocket
//! endpoints. Provides navigate, snapshot, screenshot, click, type, and status.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

/// CDP connection state.
pub struct BrowserClient {
    /// CDP HTTP endpoint (e.g. http://localhost:9222).
    endpoint: String,
    /// HTTP client for CDP requests.
    http: reqwest::Client,
    /// Cached WebSocket debugger URL for the active page.
    ws_url: Mutex<Option<String>>,
    /// Monotonic command id for CDP messages.
    cmd_id: Mutex<i64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code, missing_docs)]
pub struct CdpTarget {
    id: String,
    #[serde(rename = "type")]
    target_type: String,
    title: String,
    url: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct CdpCommand {
    id: i64,
    method: String,
    params: Value,
}

impl BrowserClient {
    /// Create a new CDP client pointing at the given endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            ws_url: Mutex::new(None),
            cmd_id: Mutex::new(1),
        }
    }

    #[allow(dead_code)]
    fn next_id(&self) -> i64 {
        // We only need uniqueness within a connection, so just increment.
        // For concurrent use we'd need atomic, but Mutex is fine here.
        let mut id = self.cmd_id.blocking_lock();
        let current = *id;
        *id += 1;
        current
    }

    async fn next_id_async(&self) -> i64 {
        let mut id = self.cmd_id.lock().await;
        let current = *id;
        *id += 1;
        current
    }

    /// Check if a browser is reachable at the endpoint.
    pub async fn is_available(&self) -> bool {
        self.http
            .get(format!("{}/json/version", self.endpoint))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Get browser version info.
    pub async fn version(&self) -> Result<Value, String> {
        let resp = self
            .http
            .get(format!("{}/json/version", self.endpoint))
            .send()
            .await
            .map_err(|e| format!("connection failed: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("failed to parse version: {e}"))
    }

    /// List open tabs/targets.
    pub async fn list_targets(&self) -> Result<Vec<CdpTarget>, String> {
        let resp = self
            .http
            .get(format!("{}/json/list", self.endpoint))
            .send()
            .await
            .map_err(|e| format!("connection failed: {e}"))?;
        resp.json::<Vec<CdpTarget>>()
            .await
            .map_err(|e| format!("failed to parse targets: {e}"))
    }

    /// Get the WebSocket URL for the first page target, or a specific target id.
    async fn get_ws_url(&self, target_id: Option<&str>) -> Result<String, String> {
        // Check cache first
        if target_id.is_none() {
            let cached = self.ws_url.lock().await;
            if let Some(url) = cached.as_ref() {
                return Ok(url.clone());
            }
        }

        let targets = self.list_targets().await?;
        let target = if let Some(tid) = target_id {
            targets.into_iter().find(|t| t.id == tid)
        } else {
            targets.into_iter().find(|t| t.target_type == "page")
        };

        match target {
            Some(t) => {
                let ws = t
                    .web_socket_debugger_url
                    .ok_or_else(|| "target has no WebSocket URL".to_string())?;
                if target_id.is_none() {
                    *self.ws_url.lock().await = Some(ws.clone());
                }
                Ok(ws)
            }
            None => Err("no page target found".to_string()),
        }
    }

    /// Send a CDP command over WebSocket and return the result.
    async fn send_command(
        &self,
        method: &str,
        params: Value,
        target_id: Option<&str>,
    ) -> Result<Value, String> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;

        let ws_url = self.get_ws_url(target_id).await?;
        let (mut ws, _) = connect_async(&ws_url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?;

        let id = self.next_id_async().await;
        let cmd = json!({
            "id": id,
            "method": method,
            "params": params,
        });

        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            cmd.to_string().into(),
        ))
        .await
        .map_err(|e| format!("WebSocket send failed: {e}"))?;

        // Read responses until we get ours (with timeout)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let msg = tokio::time::timeout_at(deadline, ws.next())
                .await
                .map_err(|_| "CDP command timed out after 30s".to_string())?
                .ok_or_else(|| "WebSocket closed".to_string())?
                .map_err(|e| format!("WebSocket read error: {e}"))?;

            if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                if let Ok(resp) = serde_json::from_str::<Value>(&text) {
                    if resp.get("id").and_then(|v| v.as_i64()) == Some(id) {
                        if let Some(err) = resp.get("error") {
                            return Err(format!(
                                "CDP error: {}",
                                err.get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("unknown")
                            ));
                        }
                        return Ok(resp.get("result").cloned().unwrap_or(json!({})));
                    }
                }
            }
        }
    }

    /// Navigate to a URL.
    pub async fn navigate(&self, url: &str) -> Result<Value, String> {
        // Invalidate ws_url cache since navigation may change state
        self.send_command("Page.navigate", json!({"url": url}), None)
            .await
    }

    /// Get the page's accessibility tree snapshot.
    pub async fn snapshot(&self) -> Result<String, String> {
        // Use Accessibility.getFullAXTree for a rich snapshot
        let result = self
            .send_command("Accessibility.getFullAXTree", json!({}), None)
            .await;

        match result {
            Ok(tree) => {
                // Format the AX tree nodes into a readable text
                if let Some(nodes) = tree.get("nodes").and_then(|n| n.as_array()) {
                    let mut output = Vec::new();
                    for node in nodes.iter().take(500) {
                        // Limit to 500 nodes
                        let role = node
                            .get("role")
                            .and_then(|r| r.get("value"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let name = node
                            .get("name")
                            .and_then(|n| n.get("value"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if !role.is_empty() && role != "none" && role != "generic" {
                            if name.is_empty() {
                                output.push(format!("[{role}]"));
                            } else {
                                output.push(format!("[{role}] {name}"));
                            }
                        }
                    }
                    Ok(output.join("\n"))
                } else {
                    // Fallback: return raw JSON
                    Ok(serde_json::to_string_pretty(&tree).unwrap_or_default())
                }
            }
            Err(_) => {
                // Fallback to DOM extraction
                debug!("AX tree failed, falling back to DOM text extraction");
                let result = self
                    .send_command(
                        "Runtime.evaluate",
                        json!({
                            "expression": "document.body?.innerText?.substring(0, 50000) || document.title || ''",
                            "returnByValue": true,
                        }),
                        None,
                    )
                    .await?;
                let text = result
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(text)
            }
        }
    }

    /// Take a screenshot of the current page.
    pub async fn screenshot(&self, format: Option<&str>) -> Result<String, String> {
        let fmt = format.unwrap_or("png");
        let result = self
            .send_command("Page.captureScreenshot", json!({"format": fmt}), None)
            .await?;
        result
            .get("data")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "no screenshot data returned".to_string())
    }

    /// Click an element by CSS selector or coordinates.
    pub async fn click(
        &self,
        selector: Option<&str>,
        x: Option<f64>,
        y: Option<f64>,
    ) -> Result<String, String> {
        let (click_x, click_y) = if let Some(sel) = selector {
            // Resolve selector to coordinates
            let result = self
                .send_command(
                    "Runtime.evaluate",
                    json!({
                        "expression": format!(
                            r#"(() => {{
                                const el = document.querySelector({sel});
                                if (!el) return null;
                                const rect = el.getBoundingClientRect();
                                return {{ x: rect.x + rect.width/2, y: rect.y + rect.height/2 }};
                            }})()"#,
                            sel = serde_json::to_string(sel).unwrap_or_default()
                        ),
                        "returnByValue": true,
                    }),
                    None,
                )
                .await?;

            let coords = result
                .get("result")
                .and_then(|r| r.get("value"))
                .ok_or_else(|| format!("element not found: {sel}"))?;

            if coords.is_null() {
                return Err(format!("element not found: {sel}"));
            }

            let cx = coords.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let cy = coords.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            (cx, cy)
        } else if let (Some(cx), Some(cy)) = (x, y) {
            (cx, cy)
        } else {
            return Err("provide either 'selector' or 'x'+'y' coordinates".to_string());
        };

        // Mouse down + up
        self.send_command(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mousePressed",
                "x": click_x,
                "y": click_y,
                "button": "left",
                "clickCount": 1,
            }),
            None,
        )
        .await?;

        self.send_command(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseReleased",
                "x": click_x,
                "y": click_y,
                "button": "left",
                "clickCount": 1,
            }),
            None,
        )
        .await?;

        Ok(format!("clicked at ({click_x}, {click_y})"))
    }

    /// Type text, optionally into a focused element or after clicking a selector.
    pub async fn type_text(&self, text: &str, selector: Option<&str>) -> Result<String, String> {
        // If selector provided, click it first to focus
        if let Some(sel) = selector {
            self.click(Some(sel), None, None).await?;
            // Small delay for focus
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Use Input.insertText for the full string (handles unicode properly)
        self.send_command("Input.insertText", json!({"text": text}), None)
            .await?;

        Ok(format!("typed {} chars", text.len()))
    }
}

/// Default CDP endpoint — localhost:9222 is the standard Chrome debugging port.
pub const DEFAULT_CDP_ENDPOINT: &str = "http://localhost:9222";

/// Create a browser client with optional endpoint override.
pub fn create_client(endpoint: Option<&str>) -> BrowserClient {
    BrowserClient::new(endpoint.unwrap_or(DEFAULT_CDP_ENDPOINT))
}
