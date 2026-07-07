//! Shared host-runtime helper layer for the pares-agens host binaries.
//!
//! ADR-0010 (no duplicated operational logic): these helpers were previously
//! copy-pasted into both the `cli` (`agens-host` bin) and `agens-plugin` host
//! crates. They now live here, once, and both crates depend on this crate.
//!
//! Scope: runtime log-level control, tool-call argument/result formatting for
//! the pares-manus bridge, verbose-tool telegram markers, host connection
//! single-connection conflict detection, and the systemd watchdog / process
//! memory monitor. Behavior is identical to the previous per-crate copies.
//!
//! Two helpers (`spawn_memory_monitor`, `spawn_systemd_watchdog`) take the
//! build-time commit hash as a `commit: &'static str` argument, because the
//! `GIT_COMMIT_HASH` compile env is set by each *binary* crate's `build.rs`
//! (via `cargo:rustc-env`) and is not visible to this library crate. Callers
//! pass `env!("GIT_COMMIT_HASH")`, preserving the original behavior exactly.

use std::time::Duration;

use pares_agens_channels::telegram::TELEGRAM_VERBOSE_TOOL_DETAILS_MARKER;
use pares_agens_core::memory::store::HostAdapterRecord;
use tracing_subscriber::EnvFilter;

// Process RSS diagnostics live once in `core::diagnostics` (ADR-0010). Re-exported
// here so host binaries that already import them via this crate keep working, and
// so `spawn_memory_monitor` below can sample memory.
pub use pares_agens_core::diagnostics::{current_process_rss_kib, parse_vm_rss_kib};

/// Preview cap (chars) for tool *arguments* rendered into verbose tool traces.
pub const VERBOSE_TOOL_ARGS_PREVIEW_CHARS: usize = 240;
/// Preview cap (chars) for tool *results* rendered into verbose tool traces.
pub const VERBOSE_TOOL_RESULT_PREVIEW_CHARS: usize = 500;
/// Interval (seconds) between process-memory (`VmRSS`) log samples.
pub const MEMORY_MONITOR_INTERVAL_SECS: u64 = 60;

/// A single recorded tool invocation, used to render verbose tool-detail
/// summaries for channels that opt into them.
#[derive(Clone, Debug)]
pub struct ToolCallTrace {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub result: String,
    pub is_error: bool,
}

/// A detected single-connection conflict: the same `connection_id` for a given
/// adapter `kind` is claimed by more than one host, one of which is the local
/// host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleConnectionConflict {
    pub kind: String,
    pub connection_id: String,
    pub hosts: Vec<String>,
}

/// Normalize a user-supplied log level to a canonical lowercase directive.
pub fn normalize_log_level(value: &str) -> Result<String, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" | "debug" | "info" | "warn" | "error" => Ok(value.trim().to_ascii_lowercase()),
        _ => Err("log level must be one of: trace, debug, info, warn, error".to_string()),
    }
}

/// Build a tracing `EnvFilter` for a normalized log level.
pub fn build_env_filter(level: &str) -> Result<EnvFilter, String> {
    let level = normalize_log_level(level)?;
    let directive = level
        .parse()
        .map_err(|e| format!("failed to parse '{level}' as tracing directive: {e}"))?;
    Ok(EnvFilter::from_default_env().add_directive(directive))
}

/// Reload the process-wide tracing filter to a new runtime log level.
pub fn apply_runtime_log_level(
    handle: &tracing_subscriber::reload::Handle<EnvFilter, tracing_subscriber::Registry>,
    level: &str,
) -> Result<String, String> {
    let normalized = normalize_log_level(level)?;
    let filter = build_env_filter(&normalized)?;
    handle
        .reload(filter)
        .map_err(|e| format!("failed to reload log filter: {e}"))?;
    Ok(normalized)
}

/// Default for whether deep-escalation is enabled when unset.
pub fn default_deep_escalation_enabled() -> bool {
    true
}

/// Coerce an arbitrary JSON value into tool-content string form.
pub fn value_to_tool_content(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| value.to_string())
}

/// Parse raw tool arguments (JSON text) into a value.
pub fn parse_tool_args(raw: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(raw).map_err(|e| format!("invalid tool arguments: {e}"))
}

/// Map a high-level tool call to a pares-manus request (method + params).
pub fn manus_request_for_tool(
    tool_name: &str,
    args: serde_json::Value,
) -> Result<(&'static str, serde_json::Value), String> {
    match tool_name {
        "browser_open" => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'url'".to_string())?;
            Ok(("browser.open", serde_json::json!({ "url": url })))
        }
        "browser_screenshot" => Ok(("browser.screenshot", serde_json::json!({}))),
        "browser_click" => {
            let x = args
                .get("x")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| "missing 'x'".to_string())?;
            let y = args
                .get("y")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| "missing 'y'".to_string())?;
            Ok(("gui.click", serde_json::json!({ "x": x, "y": y })))
        }
        "browser_type" => {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'text'".to_string())?;
            Ok(("gui.type", serde_json::json!({ "text": text })))
        }
        "screen_capture" => {
            let monitor = args.get("monitor").and_then(|v| v.as_u64());
            let window = args.get("window").and_then(|v| v.as_str());
            let mut params = serde_json::Map::new();
            if let Some(monitor) = monitor {
                params.insert("monitor".to_string(), serde_json::Value::from(monitor));
            }
            if let Some(window) = window {
                params.insert("window".to_string(), serde_json::Value::from(window));
            }
            Ok(("screen.capture", serde_json::Value::Object(params)))
        }
        "cdp_execute" => {
            let script = args
                .get("script")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'script'".to_string())?;
            Ok(("cdp.execute", serde_json::json!({ "script": script })))
        }
        _ => Err(format!("unsupported pares-manus tool '{tool_name}'")),
    }
}

/// Truncate a preview string to `max_chars` characters, appending an ellipsis
/// when content was elided.
pub fn truncate_verbose_preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}\u{2026}")
    } else {
        preview
    }
}

/// Detect and strip the telegram verbose-tool-details marker prefix.
pub fn extract_verbose_tool_marker(content: &str) -> (bool, String) {
    match content.strip_prefix(TELEGRAM_VERBOSE_TOOL_DETAILS_MARKER) {
        Some(stripped) => (true, stripped.to_string()),
        None => (false, content.to_string()),
    }
}

/// Render a set of tool-call traces into a human-readable verbose summary.
pub fn format_verbose_tool_traces(traces: &[ToolCallTrace]) -> String {
    use std::fmt::Write;

    if traces.is_empty() {
        return "Tool execution details:\n(no tool calls made)".to_string();
    }

    let mut output = String::from("Tool execution details:");
    for (idx, trace) in traces.iter().enumerate() {
        let status = if trace.is_error { "error" } else { "ok" };
        let args = truncate_verbose_preview(
            &trace.arguments.to_string(),
            VERBOSE_TOOL_ARGS_PREVIEW_CHARS,
        );
        let result = truncate_verbose_preview(&trace.result, VERBOSE_TOOL_RESULT_PREVIEW_CHARS);
        let _ = write!(
            output,
            "\n{}. {} [{}]\nargs: {}\nresult: {}",
            idx + 1,
            trace.tool_name,
            status,
            args,
            result
        );
    }
    output
}

/// Sanitize a raw hostname into a stable, filesystem/id-safe form.
pub fn sanitize_hostname(raw: &str) -> String {
    let mut value = String::new();
    let mut prev_underscore = false;
    for c in raw.trim().chars() {
        let mapped = if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            c
        } else {
            '_'
        };
        if mapped == '_' {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
        }
        value.push(mapped);
    }
    value = value.trim_matches('_').to_string();
    if value.is_empty() {
        value = "unknown-host".to_string();
    }
    value
}

/// Resolve the current hostname from env / `/etc/hostname`, sanitized.
pub fn current_hostname() -> String {
    if let Ok(value) = std::env::var("HOSTNAME") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    if let Ok(value) = std::env::var("COMPUTERNAME") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    #[cfg(unix)]
    if let Ok(value) = std::fs::read_to_string("/etc/hostname") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    "unknown-host".to_string()
}

/// Parse a 32-byte sync topic key from a hex string (optionally `0x`-prefixed).
pub fn parse_sync_topic_key(raw: &str) -> Result<[u8; 32], String> {
    let trimmed = raw.trim();
    let value = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if value.len() != 64 {
        return Err("sync topic key must be 64 hex characters (32 bytes)".to_string());
    }

    let mut key = [0u8; 32];
    for i in 0..32 {
        let pair = &value[(i * 2)..(i * 2 + 2)];
        key[i] = u8::from_str_radix(pair, 16)
            .map_err(|_| format!("invalid hex byte at position {}: {pair}", i * 2))?;
    }
    Ok(key)
}

/// Redact a connection id for logging (keep first/last 4 chars).
pub fn redact_connection_id(value: &str) -> String {
    let len = value.chars().count();
    if len <= 8 {
        return "********".to_string();
    }
    let start: String = value.chars().take(4).collect();
    let end: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}\u{2026}{end}")
}

/// Detect single-connection conflicts affecting `local_host` across a set of
/// host adapter records.
pub fn detect_single_connection_conflicts(
    local_host: &str,
    records: &[HostAdapterRecord],
) -> Vec<SingleConnectionConflict> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut owners: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for record in records {
        for adapter in &record.adapters {
            if !adapter.single_connection || adapter.connection_id.trim().is_empty() {
                continue;
            }
            owners
                .entry((adapter.kind.clone(), adapter.connection_id.clone()))
                .or_default()
                .insert(record.host.clone());
        }
    }

    owners
        .into_iter()
        .filter_map(|((kind, connection_id), hosts)| {
            if hosts.len() < 2 || !hosts.contains(local_host) {
                return None;
            }
            Some(SingleConnectionConflict {
                kind,
                connection_id,
                hosts: hosts.into_iter().collect(),
            })
        })
        .collect()
}

/// Compute the systemd watchdog ping interval from `WATCHDOG_USEC`.
pub fn parse_watchdog_ping_interval(watchdog_usec: &str) -> Option<Duration> {
    let micros = watchdog_usec.trim().parse::<u64>().ok()?;
    if micros == 0 {
        return None;
    }
    let half = micros / 2;
    let ping_interval_micros = std::cmp::max(half, 1_000_000);
    Some(Duration::from_micros(ping_interval_micros))
}

/// Spawn a background task that periodically logs process memory usage.
///
/// `commit` is the build-time commit hash (`env!("GIT_COMMIT_HASH")` in the
/// calling binary crate).
pub fn spawn_memory_monitor(commit: &'static str) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(MEMORY_MONITOR_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if let Some(rss_kib) = current_process_rss_kib() {
                tracing::info!(
                    memory_rss_kib = rss_kib,
                    commit = commit,
                    "process memory usage"
                );
            }
        }
    })
}

/// Send a systemd notification datagram (`sd_notify`) if `NOTIFY_SOCKET` is set.
///
/// No-op (returns `Ok`) on non-unix targets or when `NOTIFY_SOCKET` is unset.
/// Public because host binaries also send lifecycle states directly (e.g.
/// `STOPPING=1` during shutdown), in addition to the watchdog pinger below.
pub fn systemd_notify(state: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixDatagram;

        let notify_socket = match std::env::var("NOTIFY_SOCKET") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return Ok(()),
        };

        let sock = UnixDatagram::unbound().map_err(|e| format!("sd_notify socket failed: {e}"))?;
        if notify_socket.starts_with('@') {
            return Err("abstract NOTIFY_SOCKET is not supported in this build".to_string());
        }

        sock.send_to(state.as_bytes(), &notify_socket)
            .map_err(|e| format!("sd_notify send failed: {e}"))?;

        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = state;
        Ok(())
    }
}

/// Spawn the systemd watchdog pinger if running under systemd with a watchdog.
pub fn spawn_systemd_watchdog() -> Option<tokio::task::JoinHandle<()>> {
    let watchdog_usec = std::env::var("WATCHDOG_USEC").ok()?;
    let ping_interval = parse_watchdog_ping_interval(&watchdog_usec)?;

    if let Err(e) = systemd_notify("READY=1") {
        tracing::warn!("failed to send systemd READY=1: {e}");
    }

    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(ping_interval);
        loop {
            interval.tick().await;
            if let Err(e) = systemd_notify("WATCHDOG=1") {
                tracing::warn!("failed to send systemd WATCHDOG=1: {e}");
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pares_agens_core::memory::store::HostAdapterConfig;

    #[test]
    fn normalize_log_level_accepts_known_values() {
        assert_eq!(normalize_log_level("DEBUG").unwrap(), "debug");
        assert_eq!(normalize_log_level(" warn ").unwrap(), "warn");
    }

    #[test]
    fn normalize_log_level_rejects_unknown_values() {
        assert!(normalize_log_level("verbose").is_err());
    }

    #[test]
    fn parse_watchdog_ping_interval_has_safe_minimum() {
        let interval = parse_watchdog_ping_interval("1000").expect("watchdog interval");
        assert_eq!(interval, Duration::from_secs(1));
    }

    #[test]
    fn parse_watchdog_ping_interval_uses_half_of_watchdog_usec() {
        let interval = parse_watchdog_ping_interval("4000000").expect("watchdog interval");
        assert_eq!(interval, Duration::from_secs(2));
    }

    #[test]
    fn extract_verbose_tool_marker_detects_and_strips_prefix() {
        let (is_verbose, stripped) =
            extract_verbose_tool_marker("__PARES_VERBOSE_TOOL_DETAILS__:run diagnostics");
        assert!(is_verbose);
        assert_eq!(stripped, "run diagnostics");
    }

    #[test]
    fn extract_verbose_tool_marker_preserves_plain_content() {
        let (is_verbose, stripped) = extract_verbose_tool_marker("hello");
        assert!(!is_verbose);
        assert_eq!(stripped, "hello");
    }

    #[test]
    fn format_verbose_tool_traces_renders_tool_name_and_result() {
        let traces = vec![ToolCallTrace {
            tool_name: "web_search".to_string(),
            arguments: serde_json::json!({"q":"status"}),
            result: "{\"ok\":true}".to_string(),
            is_error: false,
        }];
        let formatted = format_verbose_tool_traces(&traces);
        assert!(formatted.contains("Tool execution details:"));
        assert!(formatted.contains("web_search [ok]"));
        assert!(formatted.contains("result: {\"ok\":true}"));
    }

    #[test]
    fn manus_request_maps_browser_click_to_gui_click() {
        let (method, params) =
            manus_request_for_tool("browser_click", serde_json::json!({"x": 21, "y": 34}))
                .expect("request should map");
        assert_eq!(method, "gui.click");
        assert_eq!(params, serde_json::json!({"x": 21, "y": 34}));
    }

    #[test]
    fn manus_request_maps_screen_capture_optional_fields() {
        let (method, params) = manus_request_for_tool(
            "screen_capture",
            serde_json::json!({"monitor": 1, "window": "Edge"}),
        )
        .expect("request should map");
        assert_eq!(method, "screen.capture");
        assert_eq!(params, serde_json::json!({"monitor": 1, "window": "Edge"}));
    }

    #[test]
    fn manus_request_requires_browser_open_url() {
        let err = manus_request_for_tool("browser_open", serde_json::json!({}))
            .expect_err("missing url should fail");
        assert!(err.contains("missing 'url'"));
    }

    #[test]
    fn detect_single_connection_conflicts_for_local_host() {
        let records = vec![
            HostAdapterRecord {
                host: "alpha".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, "telegram");
        assert_eq!(
            conflicts[0].hosts,
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn detect_single_connection_conflicts_ignores_non_local_conflicts() {
        let records = vec![
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
            HostAdapterRecord {
                host: "gamma".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn detect_single_connection_conflicts_ignores_non_single_connections() {
        let records = vec![
            HostAdapterRecord {
                host: "alpha".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "local".to_string(),
                    connection_id: "n/a".to_string(),
                    single_connection: false,
                }],
            },
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "local".to_string(),
                    connection_id: "n/a".to_string(),
                    single_connection: false,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert!(conflicts.is_empty());
    }
}
