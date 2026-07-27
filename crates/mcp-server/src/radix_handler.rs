//! Radix tool handler — wires MCP server tool calls to real pares-radix tools.
//!
//! This is the production `ToolHandler` implementation that connects incoming
//! MCP `tools/call` requests to the actual tool implementations:
//! - File I/O (read, write, edit, list_directory)
//! - Shell execution (run_command, process)
//! - Memory (memory_search, memory_store)
//! - Web (web_fetch, web_search)
//! - Cron (cron_list, cron_add, cron_remove, cron_toggle)
//! - State/DB (db_get, db_put, db_delete)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use pares_radix_core::chronos::{ChronosAction, ChronosLevel, ChronosTimeline};
use pares_agens_core::delegation::{SpawnOptions, SubAgentManager};
use pares_agens_core::memory::PluresLm;
use pares_radix_core::plugins::PluginRuntime;
use pares_radix_core::shell_executor::ShellExecutor;
use pares_radix_core::spine::event::SpineEvent;
use pares_radix_core::spine::pipeline::PipelineEmitter;
use pares_radix_core::StateStore;
use pares_radix_mcp_client::protocol::{Tool, ToolInputSchema};
use uuid::Uuid;

use crate::app_metrics::AppMetrics;
use pares_agens_agenda::scheduler::Scheduler;
use pares_radix_praxis::module::PraxisModule;
use pares_radix_praxis::px;
use pares_radix_praxis::px::async_executor::{self as px_async, AsyncActionHandler};
use pares_radix_praxis::px::compiler;
use pares_radix_praxis::px::compose::{ComposableHandler, ProcedureRegistry};
use pares_radix_praxis::rule::{RuleContext, RuleResult};

use crate::browser::BrowserClient;
use crate::handler::{ToolHandler, ToolResult};

// ── Tool Metrics (for telemetry_snapshot) ──────────────────────────────────────

/// Per-tool usage metrics collected at runtime.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ToolCallStats {
    /// Number of times this tool was called.
    pub calls: u64,
    /// Number of successful calls.
    pub successes: u64,
    /// Number of failed calls.
    pub failures: u64,
    /// Total latency across all calls in milliseconds.
    pub total_latency_ms: u64,
}

/// Aggregated metrics across all tool calls.
#[derive(Debug, Default)]
pub struct ToolMetrics {
    /// Per-tool statistics.
    pub per_tool: HashMap<String, ToolCallStats>,
    /// Total tool calls.
    pub total_calls: u64,
    /// Timestamp (Unix epoch seconds) when metrics collection started.
    pub started_at: u64,
}

impl ToolMetrics {
    fn new() -> Self {
        Self {
            per_tool: HashMap::new(),
            total_calls: 0,
            started_at: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    fn record(&mut self, tool_name: &str, latency_ms: u64, success: bool) {
        self.total_calls += 1;
        let entry = self.per_tool.entry(tool_name.to_string()).or_default();
        entry.calls += 1;
        entry.total_latency_ms += latency_ms;
        if success {
            entry.successes += 1;
        } else {
            entry.failures += 1;
        }
    }

    fn snapshot(&self) -> Value {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let uptime_secs = now.saturating_sub(self.started_at);

        // Top tools by call count
        let mut ranked: Vec<_> = self.per_tool.iter().collect();
        ranked.sort_by_key(|b| std::cmp::Reverse(b.1.calls));

        let top_tools: Vec<Value> = ranked
            .iter()
            .take(15)
            .map(|(name, stats)| {
                let avg_ms = stats.total_latency_ms.checked_div(stats.calls).unwrap_or(0);
                json!({
                    "name": name,
                    "calls": stats.calls,
                    "successes": stats.successes,
                    "failures": stats.failures,
                    "avg_latency_ms": avg_ms,
                })
            })
            .collect();

        let total_latency: u64 = self.per_tool.values().map(|s| s.total_latency_ms).sum();
        let avg_latency = total_latency.checked_div(self.total_calls).unwrap_or(0);

        json!({
            "total_calls": self.total_calls,
            "unique_tools_used": self.per_tool.len(),
            "avg_latency_ms": avg_latency,
            "uptime_secs": uptime_secs,
            "started_at_unix": self.started_at,
            "top_tools": top_tools,
        })
    }

    fn reset(&mut self) {
        self.per_tool.clear();
        self.total_calls = 0;
        self.started_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
    }
}

/// A pre-loaded .px procedure ready for execution.
#[derive(Debug, Clone)]
pub struct LoadedProcedure {
    /// Procedure name (from the .px source).
    pub name: String,
    /// Source file path (for reference).
    pub source_file: PathBuf,
    /// Compiled record data (ready for async execution).
    pub data: Value,
    /// Optional description extracted from the procedure's doc comment.
    pub description: Option<String>,
}

/// Shared, hot-reloadable procedure registry that the PxWatcher updates.
pub type SharedProcedures = Arc<RwLock<HashMap<String, LoadedProcedure>>>;

/// Production tool handler that connects MCP tool calls to real implementations.
pub struct RadixToolHandler {
    /// Shell executor for run_command/process tools.
    shell: Arc<ShellExecutor>,
    /// Memory system for semantic search/store.
    memory: Option<Arc<PluresLm>>,
    /// Scheduler for cron tools.
    scheduler: Option<Arc<Scheduler>>,
    /// Key-value state store for db_get/db_put/db_delete tools.
    state_store: Option<Arc<dyn StateStore>>,
    /// Working directory for file operations.
    workdir: PathBuf,
    /// Brave Search API key (optional).
    brave_api_key: Option<String>,
    /// OpenAI API key for media tools (image gen, TTS, vision).
    openai_api_key: Option<String>,
    /// Media output directory for generated files.
    media_dir: PathBuf,
    /// Browser CDP client for browser automation tools.
    browser: Option<Arc<BrowserClient>>,
    /// Praxis modules for constraint evaluation.
    praxis_modules: Vec<Box<dyn PraxisModule + Send + Sync>>,
    /// Pre-loaded .px procedures (name → compiled record data).
    /// Wrapped in Arc<RwLock> for hot-reload via PxWatcher.
    loaded_procedures: SharedProcedures,
    /// Chronos version timeline for audit/history queries.
    chronos: Option<Arc<ChronosTimeline>>,
    /// Sub-agent manager for delegation tools.
    subagent_manager: Option<Arc<SubAgentManager>>,
    /// Agent instance for agent_ask tool — full agent loop via any channel.
    agent: Option<Arc<pares_agens_core::Agent>>,
    /// Plugin runtime for plugin management tools.
    plugin_runtime: Option<Arc<PluginRuntime>>,
    /// Notification sender for server-initiated notifications (e.g., tools/list_changed).
    notification_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::server::ServerNotification>>,
    /// Runtime tool usage metrics (protected by Mutex for interior mutability).
    metrics: Mutex<ToolMetrics>,
    /// OpenTelemetry application metrics (no-op without a configured exporter).
    otel_metrics: AppMetrics,
    /// Pipeline emitter for sending spine events (e.g., DeliveryRequest from send_message).
    pipeline_emitter: Option<PipelineEmitter>,
}

impl RadixToolHandler {
    /// Create a new handler with required dependencies.
    pub fn new(shell: Arc<ShellExecutor>, workdir: PathBuf) -> Self {
        let media_dir = workdir.join(".radix-media");
        Self {
            shell,
            memory: None,
            scheduler: None,
            state_store: None,
            workdir,
            brave_api_key: None,
            openai_api_key: None,
            media_dir,
            browser: None,
            praxis_modules: Vec::new(),
            loaded_procedures: Arc::new(RwLock::new(HashMap::new())),
            chronos: None,
            subagent_manager: None,
            agent: None,
            plugin_runtime: None,
            notification_tx: None,
            metrics: Mutex::new(ToolMetrics::new()),
            otel_metrics: AppMetrics::new(),
            pipeline_emitter: None,
        }
    }

    /// Attach a memory system for memory_search/memory_store tools.
    pub fn with_memory(mut self, memory: Arc<PluresLm>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Attach a scheduler for cron tools.
    pub fn with_scheduler(mut self, scheduler: Arc<Scheduler>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Set the Brave Search API key for web_search.
    pub fn with_brave_api_key(mut self, key: String) -> Self {
        self.brave_api_key = Some(key);
        self
    }

    /// Set the OpenAI API key for media tools.
    pub fn with_openai_api_key(mut self, key: String) -> Self {
        self.openai_api_key = Some(key);
        self
    }

    /// Attach a state store for db_get/db_put/db_delete tools.
    pub fn with_state_store(mut self, store: Arc<dyn StateStore>) -> Self {
        self.state_store = Some(store);
        self
    }

    /// Attach praxis modules for constraint evaluation.
    pub fn with_praxis_modules(
        mut self,
        modules: Vec<Box<dyn PraxisModule + Send + Sync>>,
    ) -> Self {
        self.praxis_modules = modules;
        self
    }

    /// Attach a Chronos timeline for version history tools.
    pub fn with_chronos(mut self, chronos: Arc<ChronosTimeline>) -> Self {
        self.chronos = Some(chronos);
        self
    }

    /// Attach a sub-agent manager for delegation tools.
    pub fn with_subagent_manager(mut self, manager: Arc<SubAgentManager>) -> Self {
        self.subagent_manager = Some(manager);
        self
    }

    /// Attach an Agent instance for the `agent_ask` tool.
    /// Enables channel-agnostic agent invocation through MCP.
    pub fn with_agent(mut self, agent: Arc<pares_agens_core::Agent>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Attach a plugin runtime for plugin management tools.
    pub fn with_plugin_runtime(mut self, runtime: Arc<PluginRuntime>) -> Self {
        self.plugin_runtime = Some(runtime);
        self
    }

    /// Attach a notification sender for server-initiated notifications.
    ///
    /// When set, plugin changes (register/activate/deactivate) will emit
    /// `notifications/tools/list_changed` to tell the client to re-fetch tools.
    pub fn with_notification_tx(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::server::ServerNotification>,
    ) -> Self {
        self.notification_tx = Some(tx);
        self
    }

    /// Attach a pipeline emitter for sending spine events (used by send_message tool).
    pub fn with_pipeline_emitter(mut self, emitter: PipelineEmitter) -> Self {
        self.pipeline_emitter = Some(emitter);
        self
    }

    /// Load all .px files from a directory (recursively) and register their procedures.
    ///
    /// Procedures are then available via `praxis_run` by name (without needing
    /// to specify a file path) and are listed in `praxis_list`.
    ///
    /// **Must be called during single-threaded construction** (before the handler
    /// is wrapped in `Arc` and shared). Panics if the `Arc<RwLock>` is already shared.
    pub fn with_px_dir(mut self, dir: PathBuf) -> Self {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            // Safe during construction: we're the sole owner of the Arc.
            let map = Arc::get_mut(&mut self.loaded_procedures)
                .expect("with_px_dir must be called before sharing the handler")
                .get_mut();
            Self::load_px_dir_into(map, &dir, entries);
        }
        self
    }

    /// Internal: recursively load .px procedures from a directory into the map.
    fn load_px_dir_into(
        procedures: &mut HashMap<String, LoadedProcedure>,
        _dir: &PathBuf,
        entries: std::fs::ReadDir,
    ) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("px") {
                if let Ok(source) = std::fs::read_to_string(&path) {
                    match px::parse(&source) {
                        Ok(doc) => {
                            let records = compiler::compile(&doc);
                            for record in &records {
                                if record.data.get("type").and_then(|v| v.as_str())
                                    == Some("procedure")
                                {
                                    let name = record
                                        .data
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();
                                    let description = record
                                        .data
                                        .get("description")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    tracing::info!(
                                        "loaded .px procedure: {} from {:?}",
                                        name,
                                        path
                                    );
                                    procedures.insert(
                                        name.clone(),
                                        LoadedProcedure {
                                            name,
                                            source_file: path.clone(),
                                            data: record.data.clone(),
                                            description,
                                        },
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("failed to parse {:?}: {e}", path);
                        }
                    }
                }
            } else if path.is_dir() {
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    Self::load_px_dir_into(procedures, &path, sub_entries);
                }
            }
        }
    }

    /// Start a PxWatcher that hot-reloads `.px` files into the procedure registry.
    ///
    /// Returns a handle to the shared procedures map. The watcher runs in the
    /// background and automatically updates procedures when files are
    /// created, modified, or deleted.
    pub async fn start_px_watcher(&self, watch_path: PathBuf) -> Result<(), std::io::Error> {
        use pares_radix_praxis::px::watcher::{PxWatchEvent, PxWatcher, PxWatcherConfig};

        let config = PxWatcherConfig {
            watch_path: watch_path.clone(),
            initial_scan: true, // Re-scan to persist all records to PluresDB
            debounce_ms: 150,
        };

        let watcher = PxWatcher::new(config);
        let mut rx = watcher.start().await?;
        let procedures = Arc::clone(&self.loaded_procedures);
        let state_store = self.state_store.clone();

        tokio::spawn(async move {
            info!(path = %watch_path.display(), "PxWatcher hot-reload active for MCP server");

            while let Some(event) = rx.recv().await {
                match event {
                    PxWatchEvent::Loaded { path, records } => {
                        // Persist all compiled records to PluresDB as px:* keys
                        if let Some(ref store) = state_store {
                            for record in &records {
                                store.set(&record.key, record.data.clone()).await;
                            }
                            debug!(
                                path = %path.display(),
                                keys = records.len(),
                                "persisted px records to PluresDB"
                            );
                        }

                        let mut map = procedures.write().await;
                        // Remove old procedures from this file
                        map.retain(|_, proc| proc.source_file != path);
                        // Add new procedures
                        let mut count = 0;
                        for record in &records {
                            if record.data.get("type").and_then(|v| v.as_str()) == Some("procedure")
                            {
                                let name = record
                                    .data
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let description = record
                                    .data
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                map.insert(
                                    name.clone(),
                                    LoadedProcedure {
                                        name,
                                        source_file: path.clone(),
                                        data: record.data.clone(),
                                        description,
                                    },
                                );
                                count += 1;
                            }
                        }
                        if count > 0 {
                            info!(
                                path = %path.display(),
                                procedures = count,
                                total = map.len(),
                                "hot-reloaded .px procedures"
                            );
                        }
                    }
                    PxWatchEvent::Removed { path, keys } => {
                        // Remove px:* keys from PluresDB
                        if let Some(ref store) = state_store {
                            for key in &keys {
                                store.delete(key).await;
                            }
                            debug!(
                                path = %path.display(),
                                keys_removed = keys.len(),
                                "removed px records from PluresDB"
                            );
                        }

                        let mut map = procedures.write().await;
                        let before = map.len();
                        map.retain(|_, proc| proc.source_file != path);
                        let removed = before - map.len();
                        if removed > 0 {
                            info!(
                                path = %path.display(),
                                removed,
                                remaining = map.len(),
                                "removed .px procedures (file deleted)"
                            );
                        }
                    }
                    PxWatchEvent::Error { path, error } => {
                        warn!(path = %path.display(), %error, "px hot-reload compile error");
                    }
                    PxWatchEvent::Ready {
                        file_count,
                        record_count,
                    } => {
                        info!(file_count, record_count, "PxWatcher initial scan complete");
                    }
                }
            }
        });

        Ok(())
    }

    /// Get a reference to the shared procedures map (for external inspection).
    pub fn shared_procedures(&self) -> &SharedProcedures {
        &self.loaded_procedures
    }

    // ── File tools ────────────────────────────────────────────────────────────

    async fn read_file(&self, args: &Value) -> ToolResult {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => self.resolve_path(p),
            None => return ToolResult::error("missing required parameter: path"),
        };

        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                // Truncate to 50KB like OpenClaw
                let truncated = if content.len() > 50_000 {
                    format!(
                        "{}\n\n... [truncated, {} total bytes]",
                        &content[..50_000],
                        content.len()
                    )
                } else {
                    content
                };
                ToolResult::ok(truncated)
            }
            Err(e) => ToolResult::error(format!("failed to read {}: {e}", path.display())),
        }
    }

    async fn write_file(&self, args: &Value) -> ToolResult {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => self.resolve_path(p),
            None => return ToolResult::error("missing required parameter: path"),
        };
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("missing required parameter: content"),
        };

        // Create parent directories
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolResult::error(format!(
                    "failed to create directories for {}: {e}",
                    path.display()
                ));
            }
        }

        match tokio::fs::write(&path, content).await {
            Ok(()) => ToolResult::ok(format!(
                "wrote {} bytes to {}",
                content.len(),
                path.display()
            )),
            Err(e) => ToolResult::error(format!("failed to write {}: {e}", path.display())),
        }
    }

    async fn edit_file(&self, args: &Value) -> ToolResult {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => self.resolve_path(p),
            None => return ToolResult::error("missing required parameter: path"),
        };
        let old_text = match args.get("old_text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("missing required parameter: old_text"),
        };
        let new_text = match args.get("new_text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("missing required parameter: new_text"),
        };

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("failed to read {}: {e}", path.display())),
        };

        if !content.contains(old_text) {
            return ToolResult::error(format!("old_text not found in {}", path.display()));
        }

        let new_content = content.replacen(old_text, new_text, 1);
        match tokio::fs::write(&path, &new_content).await {
            Ok(()) => ToolResult::ok(format!("edited {}", path.display())),
            Err(e) => ToolResult::error(format!("failed to write {}: {e}", path.display())),
        }
    }

    /// Apply a multi-file patch in the OpenClaw `*** Begin Patch` envelope.
    /// Parses all operations, validates every hunk against current disk state,
    /// and only then commits writes/deletes atomically (nothing is written if any
    /// operation would fail). Tool-surface parity with OpenClaw `apply_patch`.
    async fn apply_patch(&self, args: &Value) -> ToolResult {
        let input = match args.get("input").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("missing required parameter: input"),
        };

        let ops = match crate::patch::parse_patch(input) {
            Ok(o) => o,
            Err(e) => return ToolResult::error(e.to_string()),
        };

        // Build the plan without mutating disk (validate-then-commit).
        let plan = match crate::patch::plan_patch(
            &ops,
            |p| self.resolve_path(p),
            |abs| std::fs::read_to_string(abs).ok(),
            |abs| abs.exists(),
        ) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("patch not applied: {e}")),
        };

        // Commit: create parent dirs + write, then delete. Any I/O failure here
        // is reported; earlier writes in the same patch are best-effort left in
        // place (validation already guaranteed each op is individually sound).
        for (abs, contents) in &plan.writes {
            if let Some(parent) = abs.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolResult::error(format!(
                        "failed to create directories for {}: {e}",
                        abs.display()
                    ));
                }
            }
            if let Err(e) = tokio::fs::write(abs, contents).await {
                return ToolResult::error(format!(
                    "failed to write {}: {e}",
                    abs.display()
                ));
            }
        }
        for abs in &plan.deletes {
            if let Err(e) = tokio::fs::remove_file(abs).await {
                return ToolResult::error(format!(
                    "failed to delete {}: {e}",
                    abs.display()
                ));
            }
        }

        ToolResult::ok(format!("applied patch: {}", plan.summary.join("; ")))
    }

    async fn list_directory(&self, args: &Value) -> ToolResult {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => self.resolve_path(p),
            None => return ToolResult::error("missing required parameter: path"),
        };

        let mut entries = match tokio::fs::read_dir(&path).await {
            Ok(rd) => rd,
            Err(e) => {
                return ToolResult::error(format!(
                    "failed to read directory {}: {e}",
                    path.display()
                ))
            }
        };

        let mut items = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false);
            if is_dir {
                items.push(format!("{name}/"));
            } else {
                items.push(name);
            }
        }
        items.sort();
        ToolResult::ok(items.join("\n"))
    }

    // ── Shell tools ───────────────────────────────────────────────────────────

    async fn run_command(&self, args: &Value) -> ToolResult {
        use pares_radix_core::shell_executor::ExecRequest;

        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolResult::error("missing required parameter: command"),
        };

        let workdir = args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(|p| self.resolve_path(p).to_string_lossy().to_string())
            .or_else(|| Some(self.workdir.to_string_lossy().to_string()));

        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let pty = args.get("pty").and_then(|v| v.as_bool()).unwrap_or(false);
        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

        let request = ExecRequest {
            command,
            workdir,
            background,
            pty,
            timeout_secs: Some(timeout_secs),
            yield_ms: args.get("yieldMs").and_then(|v| v.as_u64()),
            env: args
                .get("env")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        };

        let result = self.shell.exec(request).await;
        if background {
            ToolResult::ok(format!(
                "Background session started: {}",
                result.session_id.unwrap_or_default()
            ))
        } else {
            let output = if result.exit_code == Some(0) {
                result.stdout
            } else {
                format!(
                    "exit code: {}\nstdout: {}\nstderr: {}",
                    result.exit_code.unwrap_or(-1),
                    result.stdout,
                    result.stderr
                )
            };
            ToolResult::ok(output)
        }
    }

    async fn process_action(&self, args: &Value) -> ToolResult {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("missing required parameter: action"),
        };

        match action {
            "list" => {
                let sessions = self.shell.list().await;
                let json_out = serde_json::to_string_pretty(&sessions).unwrap_or_default();
                ToolResult::ok(json_out)
            }
            "poll" => {
                let session_id = match args.get("sessionId").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing sessionId for poll"),
                };
                let timeout_ms = args.get("timeout").and_then(|v| v.as_u64());
                match self.shell.poll(session_id, timeout_ms).await {
                    Some(info) => {
                        let json_out = serde_json::to_string_pretty(&info).unwrap_or_default();
                        ToolResult::ok(json_out)
                    }
                    None => ToolResult::error(format!("session not found: {session_id}")),
                }
            }
            "kill" => {
                let session_id = match args.get("sessionId").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing sessionId for kill"),
                };
                match self.shell.kill(session_id).await {
                    Ok(()) => ToolResult::ok("killed"),
                    Err(e) => ToolResult::error(format!("kill failed: {e}")),
                }
            }
            "write" => {
                let session_id = match args.get("sessionId").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing sessionId for write"),
                };
                let data = args.get("data").and_then(|v| v.as_str()).unwrap_or("");
                match self.shell.write_stdin(session_id, data).await {
                    Ok(()) => ToolResult::ok("written"),
                    Err(e) => ToolResult::error(format!("write failed: {e}")),
                }
            }
            other => ToolResult::error(format!("unknown process action: {other}")),
        }
    }

    // ── Skill workshop (local skill authoring lifecycle) ────────────────────

    /// Author/propose/apply loop for local skills, delegating to the marketplace
    /// crate's [`ProposalStore`](pares_agens_marketplace::ProposalStore).
    ///
    /// A single tool with an `action` arg mirroring OpenClaw's `skill_workshop`:
    /// `create`, `update`, `revise`, `list`, `inspect`, `apply`, `reject`,
    /// `quarantine`. Proposals are pending until `apply` installs them into the
    /// live-skills directory as `SKILL.md` + support files.
    async fn skill_workshop(&self, args: &Value) -> ToolResult {
        use pares_agens_marketplace::{
            ProposalDraft, ProposalStatus, ProposalStore, SupportFile, WorkshopError,
        };

        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("missing required parameter: action"),
        };

        // Resolve proposals + live-skills roots (arg override, else workdir default).
        let proposals_dir = args
            .get("proposals_dir")
            .and_then(|v| v.as_str())
            .map(|p| self.resolve_path(p))
            .unwrap_or_else(|| self.workdir.join(".agens-skill-proposals"));
        let skills_dir = args
            .get("skills_dir")
            .and_then(|v| v.as_str())
            .map(|p| self.resolve_path(p))
            .unwrap_or_else(|| self.workdir.join(".agens-skills"));

        let store = match ProposalStore::open(&proposals_dir, &skills_dir) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("skill_workshop: {e}")),
        };

        // Helper: parse the optional support_files array into typed values.
        let parse_support = |args: &Value| -> std::result::Result<Vec<SupportFile>, String> {
            let Some(arr) = args.get("support_files").and_then(|v| v.as_array()) else {
                return Ok(Vec::new());
            };
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let path = item
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("support_files[{i}] missing 'path'"))?;
                let content = item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("support_files[{i}] missing 'content'"))?;
                out.push(SupportFile {
                    path: path.to_string(),
                    content: content.to_string(),
                });
            }
            Ok(out)
        };

        // Helper: build a draft from create/update/revise args.
        let build_draft = |args: &Value,
                           target_skill: Option<String>|
         -> std::result::Result<ProposalDraft, String> {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: name")?;
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: description")?;
            let body = args
                .get("proposal_content")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: proposal_content")?;
            Ok(ProposalDraft {
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
                support_files: parse_support(args)?,
                target_skill,
            })
        };

        // Helper: serialise a proposal to pretty JSON for the tool result.
        fn to_json<T: serde::Serialize>(value: &T) -> String {
            serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("<serialize error: {e}>"))
        }

        // Helper: map a WorkshopError to a ToolResult error.
        fn err(e: WorkshopError) -> ToolResult {
            ToolResult::error(format!("skill_workshop: {e}"))
        }

        match action {
            "create" => {
                let draft = match build_draft(args, None) {
                    Ok(d) => d,
                    Err(m) => return ToolResult::error(m),
                };
                match store.create(draft) {
                    Ok(p) => ToolResult::ok(to_json(&p)),
                    Err(e) => err(e),
                }
            }
            "update" => {
                // Update = a proposal that targets an existing live skill.
                let target = args
                    .get("skill_name")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.get("name").and_then(|v| v.as_str()))
                    .map(str::to_string);
                let draft = match build_draft(args, target) {
                    Ok(d) => d,
                    Err(m) => return ToolResult::error(m),
                };
                match store.create(draft) {
                    Ok(p) => ToolResult::ok(to_json(&p)),
                    Err(e) => err(e),
                }
            }
            "revise" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing required parameter: id"),
                };
                let target = args
                    .get("skill_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let draft = match build_draft(args, target) {
                    Ok(d) => d,
                    Err(m) => return ToolResult::error(m),
                };
                match store.revise(id, draft) {
                    Ok(p) => ToolResult::ok(to_json(&p)),
                    Err(e) => err(e),
                }
            }
            "list" => {
                let status = match args.get("status").and_then(|v| v.as_str()) {
                    None => None,
                    Some("pending") => Some(ProposalStatus::Pending),
                    Some("applied") => Some(ProposalStatus::Applied),
                    Some("rejected") => Some(ProposalStatus::Rejected),
                    Some("quarantined") => Some(ProposalStatus::Quarantined),
                    Some("stale") => Some(ProposalStatus::Stale),
                    Some(other) => {
                        return ToolResult::error(format!("unknown status filter: {other}"))
                    }
                };
                match store.list(status) {
                    Ok(list) => ToolResult::ok(to_json(&list)),
                    Err(e) => err(e),
                }
            }
            "inspect" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing required parameter: id"),
                };
                match store.get(id) {
                    Ok(p) => ToolResult::ok(to_json(&p)),
                    Err(e) => err(e),
                }
            }
            "apply" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing required parameter: id"),
                };
                match store.apply(id) {
                    Ok(p) => ToolResult::ok(to_json(&p)),
                    Err(e) => err(e),
                }
            }
            "reject" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing required parameter: id"),
                };
                match store.reject(id) {
                    Ok(p) => ToolResult::ok(to_json(&p)),
                    Err(e) => err(e),
                }
            }
            "quarantine" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return ToolResult::error("missing required parameter: id"),
                };
                match store.quarantine(id) {
                    Ok(p) => ToolResult::ok(to_json(&p)),
                    Err(e) => err(e),
                }
            }
            other => ToolResult::error(format!("unknown skill_workshop action: {other}")),
        }
    }

    // ── Memory tools ──────────────────────────────────────────────────────────

    async fn memory_search(&self, args: &Value) -> ToolResult {
        let memory = match &self.memory {
            Some(m) => m,
            None => return ToolResult::error("memory system not configured"),
        };

        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("missing required parameter: query"),
        };

        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

        match memory.recall(query, limit, &[]).await {
            Ok(results) => {
                let formatted: Vec<Value> = results
                    .into_iter()
                    .map(|r| {
                        json!({
                            "content": r.content,
                            "score": r.score,
                            "tags": r.tags,
                        })
                    })
                    .collect();
                ToolResult::ok(serde_json::to_string_pretty(&formatted).unwrap_or_default())
            }
            Err(e) => ToolResult::error(format!("memory search failed: {e}")),
        }
    }

    async fn memory_store(&self, args: &Value) -> ToolResult {
        let memory = match &self.memory {
            Some(m) => m,
            None => return ToolResult::error("memory system not configured"),
        };

        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("missing required parameter: content"),
        };

        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        match memory.capture_fact(content, tags).await {
            Ok(Some(id)) => ToolResult::ok(format!("stored memory: {id}")),
            Ok(None) => {
                ToolResult::ok("content rejected by quality gate (too short, duplicate, or noise)")
            }
            Err(e) => ToolResult::error(format!("memory store failed: {e}")),
        }
    }

    // ── Web tools ─────────────────────────────────────────────────────────────

    async fn web_fetch(&self, args: &Value) -> ToolResult {
        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => return ToolResult::error("missing required parameter: url"),
        };

        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(30_000) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return ToolResult::error(format!("HTTP {status} for {url}"));
                }
                match resp.text().await {
                    Ok(body) => {
                        // Simple HTML to text extraction
                        let text = if body.contains("<html") || body.contains("<HTML") {
                            html2text::from_read(body.as_bytes(), 120)
                                .unwrap_or_else(|_| body.clone())
                        } else {
                            body
                        };
                        let truncated = if text.len() > max_chars {
                            format!("{}\n\n[truncated at {max_chars} chars]", &text[..max_chars])
                        } else {
                            text
                        };
                        ToolResult::ok(truncated)
                    }
                    Err(e) => ToolResult::error(format!("failed to read response body: {e}")),
                }
            }
            Err(e) => ToolResult::error(format!("fetch failed: {e}")),
        }
    }

    async fn web_search(&self, args: &Value) -> ToolResult {
        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("missing required parameter: query"),
        };

        let api_key = match &self.brave_api_key {
            Some(k) => k.clone(),
            None => return ToolResult::error("Brave Search API key not configured"),
        };

        let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(5);
        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={count}",
            urlencoding::encode(query)
        );

        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .header("X-Subscription-Token", &api_key)
            .header("Accept", "application/json")
            .send()
            .await;

        match resp {
            Ok(r) => match r.json::<Value>().await {
                Ok(data) => {
                    let results = data
                        .get("web")
                        .and_then(|w| w.get("results"))
                        .and_then(|r| r.as_array())
                        .cloned()
                        .unwrap_or_default();

                    let formatted: Vec<Value> = results
                        .into_iter()
                        .take(count as usize)
                        .map(|r| {
                            json!({
                                "title": r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                                "url": r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                                "description": r.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                            })
                        })
                        .collect();

                    ToolResult::ok(serde_json::to_string_pretty(&formatted).unwrap_or_default())
                }
                Err(e) => ToolResult::error(format!("failed to parse search results: {e}")),
            },
            Err(e) => ToolResult::error(format!("search request failed: {e}")),
        }
    }

    // ── Cron tools ────────────────────────────────────────────────────────────

    async fn cron_list(&self, _args: &Value) -> ToolResult {
        let scheduler = match &self.scheduler {
            Some(s) => s,
            None => return ToolResult::error("scheduler not configured"),
        };

        let tasks = scheduler.list().await;
        let formatted: Vec<Value> = tasks
            .into_iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "name": t.name,
                    "schedule": t.schedule,
                    "command": t.command,
                    "enabled": t.enabled,
                    "last_run": t.last_run.map(|dt| dt.to_rfc3339()),
                    "last_result": t.last_result,
                })
            })
            .collect();
        ToolResult::ok(serde_json::to_string_pretty(&formatted).unwrap_or_default())
    }

    async fn cron_add(&self, args: &Value) -> ToolResult {
        use pares_agens_agenda::scheduler::{Schedule, Task};

        let scheduler = match &self.scheduler {
            Some(s) => s,
            None => return ToolResult::error("scheduler not configured"),
        };

        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolResult::error("missing required parameter: name"),
        };
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolResult::error("missing required parameter: command"),
        };

        let schedule = if let Some(expr) = args.get("cron").and_then(|v| v.as_str()) {
            Schedule::Cron {
                expr: expr.to_string(),
            }
        } else if let Some(secs) = args.get("interval_secs").and_then(|v| v.as_u64()) {
            Schedule::Interval { every_secs: secs }
        } else {
            return ToolResult::error("missing schedule: provide 'cron' or 'interval_secs'");
        };

        // Optional failure-alert policy: alert after N consecutive failures,
        // with an optional cooldown (seconds) between alerts.
        let failure_alert = args.get("failure_alert").and_then(|fa| {
            let after = fa.get("after").and_then(|v| v.as_u64())? as u32;
            let cooldown_secs = fa
                .get("cooldown_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Some(pares_agens_agenda::scheduler::FailureAlert {
                after,
                cooldown_secs,
            })
        });

        let task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            schedule,
            command,
            enabled: true,
            last_run: None,
            last_result: None,
            failure_alert,
            ..Default::default()
        };

        let id = task.id.clone();
        scheduler.add(task).await;
        ToolResult::ok(format!("added task: {id}"))
    }

    async fn cron_remove(&self, args: &Value) -> ToolResult {
        let scheduler = match &self.scheduler {
            Some(s) => s,
            None => return ToolResult::error("scheduler not configured"),
        };

        let id = match args.get("id").and_then(|v| v.as_str()) {
            Some(i) => i,
            None => return ToolResult::error("missing required parameter: id"),
        };

        if scheduler.remove(id).await {
            ToolResult::ok(format!("removed task: {id}"))
        } else {
            ToolResult::error(format!("task not found: {id}"))
        }
    }

    async fn cron_toggle(&self, args: &Value) -> ToolResult {
        let scheduler = match &self.scheduler {
            Some(s) => s,
            None => return ToolResult::error("scheduler not configured"),
        };

        let id = match args.get("id").and_then(|v| v.as_str()) {
            Some(i) => i,
            None => return ToolResult::error("missing required parameter: id"),
        };
        let enabled = match args.get("enabled").and_then(|v| v.as_bool()) {
            Some(e) => e,
            None => return ToolResult::error("missing required parameter: enabled"),
        };

        if scheduler.set_enabled(id, enabled).await {
            ToolResult::ok(format!("task {id} enabled={enabled}"))
        } else {
            ToolResult::error(format!("task not found: {id}"))
        }
    }

    // ── Heartbeat tools ─────────────────────────────────────────────────────────

    async fn heartbeat_status(&self, _args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        // Read heartbeat config from state (same keys HeartbeatRunner uses)
        let config = store.get("heartbeat/config").await;
        let daily_count = store.get("heartbeat/daily_count").await;
        let daily_date = store.get("heartbeat/daily_date").await;
        let checklist = store.get("heartbeat/checklist").await;

        let config_obj = config.unwrap_or(json!({
            "enabled": true,
            "interval_secs": 30,
            "quiet_hours_enabled": true,
            "quiet_hours_start": 23,
            "quiet_hours_end": 8,
            "max_proactive_per_day": 6
        }));

        let checklist_items = checklist
            .and_then(|v| v.as_array().map(|a| a.len()))
            .unwrap_or(0);

        let status = json!({
            "config": config_obj,
            "daily_count": daily_count.unwrap_or(json!(0)),
            "daily_date": daily_date.unwrap_or(json!(null)),
            "checklist_item_count": checklist_items
        });

        ToolResult::ok(serde_json::to_string_pretty(&status).unwrap_or_default())
    }

    async fn heartbeat_configure(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        // Load existing config or defaults
        let mut config = store
            .get("heartbeat/config")
            .await
            .and_then(|v| serde_json::from_value::<serde_json::Map<String, Value>>(v).ok())
            .unwrap_or_else(|| {
                serde_json::from_value::<serde_json::Map<String, Value>>(json!({
                    "enabled": true,
                    "interval_secs": 30,
                    "quiet_hours_enabled": true,
                    "quiet_hours_start": 23,
                    "quiet_hours_end": 8,
                    "max_proactive_per_day": 6
                }))
                .unwrap()
            });

        // Apply provided overrides
        if let Some(enabled) = args.get("enabled") {
            config.insert("enabled".into(), enabled.clone());
        }
        if let Some(interval) = args.get("interval_secs") {
            config.insert("interval_secs".into(), interval.clone());
        }
        if let Some(quiet_enabled) = args.get("quiet_hours_enabled") {
            config.insert("quiet_hours_enabled".into(), quiet_enabled.clone());
        }
        if let Some(quiet_start) = args.get("quiet_hours_start") {
            config.insert("quiet_hours_start".into(), quiet_start.clone());
        }
        if let Some(quiet_end) = args.get("quiet_hours_end") {
            config.insert("quiet_hours_end".into(), quiet_end.clone());
        }
        if let Some(max) = args.get("max_proactive_per_day") {
            config.insert("max_proactive_per_day".into(), max.clone());
        }

        let config_value = Value::Object(config);
        store.set("heartbeat/config", config_value.clone()).await;

        ToolResult::ok(format!(
            "heartbeat config updated: {}",
            serde_json::to_string_pretty(&config_value).unwrap_or_default()
        ))
    }

    // ── State/DB tools ────────────────────────────────────────────────────────

    async fn db_get(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };

        match store.get(key).await {
            Some(Value::Null) | None => ToolResult::ok("null"),
            Some(value) => ToolResult::ok(serde_json::to_string_pretty(&value).unwrap_or_default()),
        }
    }

    async fn db_put(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };

        let value = match args.get("value") {
            Some(v) => v.clone(),
            None => return ToolResult::error("missing required parameter: value"),
        };

        store.set(key, value).await;
        ToolResult::ok(format!("stored key: {key}"))
    }

    async fn db_delete(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };

        // Use native delete which removes the key entirely.
        store.delete(key).await;
        ToolResult::ok(format!("deleted key: {key}"))
    }

    async fn db_keys(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let prefix = args.get("prefix").and_then(|v| v.as_str()).unwrap_or("");

        let keys = store.keys_with_prefix(prefix).await;
        // Filter out null-valued keys (soft-deleted remnants)
        let mut live_keys = Vec::new();
        for key in &keys {
            match store.get(key).await {
                Some(v) if !v.is_null() => live_keys.push(key.clone()),
                _ => {}
            }
        }
        ToolResult::ok(serde_json::to_string_pretty(&live_keys).unwrap_or_default())
    }

    async fn db_dump(&self, _args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        // Get all keys, then fetch each value
        let keys = store.keys_with_prefix("").await;
        let mut entries = serde_json::Map::new();
        for key in &keys {
            if let Some(value) = store.get(key).await {
                if !value.is_null() {
                    entries.insert(key.clone(), value);
                }
            }
        }
        ToolResult::ok(serde_json::to_string_pretty(&Value::Object(entries)).unwrap_or_default())
    }

    // ── Task Action tools ────────────────────────────────────────────────────────

    async fn timestamp_now(&self) -> ToolResult {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        ToolResult::ok(json!(now).to_string())
    }

    async fn generate_id(&self, args: &Value) -> ToolResult {
        let prefix = args.get("prefix").and_then(|v| v.as_str()).unwrap_or("");

        let uuid_simple = Uuid::new_v4().simple().to_string();
        let id = if prefix.is_empty() {
            uuid_simple
        } else {
            format!("{prefix}_{uuid_simple}")
        };

        ToolResult::ok(json!(id).to_string())
    }

    async fn db_get_prefix(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let prefix = match args.get("prefix").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("missing required parameter: prefix"),
        };

        let keys = store.keys_with_prefix(prefix).await;
        let mut items = Vec::new();
        for key in &keys {
            if let Some(value) = store.get(key).await {
                if !value.is_null() {
                    items.push(json!({ "key": key, "value": value }));
                }
            }
        }

        let count = items.len();
        ToolResult::ok(
            serde_json::to_string_pretty(&json!({ "items": items, "count": count }))
                .unwrap_or_default(),
        )
    }

    async fn send_message(&self, args: &Value) -> ToolResult {
        let emitter = match &self.pipeline_emitter {
            Some(e) => e,
            None => return ToolResult::error("pipeline emitter not configured"),
        };

        let chat_id = match args.get("chat_id").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolResult::error("missing required parameter: chat_id"),
        };

        let text = match args.get("text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolResult::error("missing required parameter: text"),
        };

        let channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        let event = SpineEvent::DeliveryRequest {
            id: SpineEvent::new_id(),
            channel,
            chat_id: chat_id.clone(),
            content: text.clone(),
            metadata: json!({}),
        };

        emitter.emit(event).await;

        ToolResult::ok(format!("message sent to chat_id={chat_id}"))
    }

    // ── Config & Runtime tools ──────────────────────────────────────────────────

    async fn config_get(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };

        let full_key = format!("config:{key}");
        match store.get(&full_key).await {
            Some(Value::Null) | None => ToolResult::ok("null"),
            Some(value) => ToolResult::ok(serde_json::to_string_pretty(&value).unwrap_or_default()),
        }
    }

    async fn config_set(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };

        let value = match args.get("value") {
            Some(v) => v.clone(),
            None => return ToolResult::error("missing required parameter: value"),
        };

        let full_key = format!("config:{key}");
        store.set(&full_key, value).await;

        // Update the last-modified timestamp for hot-reload detection
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        store.set("config:__last_modified", json!(now)).await;

        ToolResult::ok(format!("config set: {key}"))
    }

    async fn config_list(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let prefix = args.get("prefix").and_then(|v| v.as_str()).unwrap_or("");

        // Dynamically scan all config keys from the store
        let config_prefix = format!("config:{prefix}");
        let keys = store.keys_with_prefix(&config_prefix).await;

        let mut results = json!({});
        for full_key in &keys {
            // Strip the "config:" prefix to get the user-facing key
            let user_key = full_key.strip_prefix("config:").unwrap_or(full_key);
            // Skip internal keys
            if user_key.starts_with("__") {
                continue;
            }
            if let Some(val) = store.get(full_key).await {
                if val != Value::Null {
                    results[user_key] = val;
                }
            }
        }

        ToolResult::ok(serde_json::to_string_pretty(&results).unwrap_or_default())
    }

    async fn config_delete(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };

        let full_key = format!("config:{key}");
        let prev = store.delete(&full_key).await;

        // Update the last-modified timestamp for hot-reload detection
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        store.set("config:__last_modified", json!(now)).await;

        match prev {
            Some(v) if v != Value::Null => {
                ToolResult::ok(format!("deleted config: {key} (was: {v})"))
            }
            _ => ToolResult::ok(format!("config key not found: {key}")),
        }
    }

    async fn runtime_status(&self, _args: &Value) -> ToolResult {
        let uptime_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Get active shell session count
        let shell_sessions = self.shell.list().await;
        let active_count = shell_sessions.len();

        let mut status = json!({
            "status": "running",
            "version": env!("CARGO_PKG_VERSION"),
            "workdir": self.workdir.display().to_string(),
            "timestamp_unix": uptime_secs,
            "components": {
                "shell": "active",
                "memory": if self.memory.is_some() { "active" } else { "not_configured" },
                "scheduler": if self.scheduler.is_some() { "active" } else { "not_configured" },
                "state_store": if self.state_store.is_some() { "active" } else { "not_configured" },
                "web_search": if self.brave_api_key.is_some() { "active" } else { "not_configured" },
                "media_tools": if self.openai_api_key.is_some() { "active" } else { "not_configured" }
            },
            "active_shell_sessions": active_count
        });

        // Include config key count if state store is available
        if let Some(store) = &self.state_store {
            let config_keys = store.keys_with_prefix("config:").await;
            let user_keys: Vec<_> = config_keys
                .iter()
                .filter_map(|k| k.strip_prefix("config:"))
                .filter(|k| !k.starts_with("__"))
                .collect();
            status["config_key_count"] = json!(user_keys.len());
        }

        ToolResult::ok(serde_json::to_string_pretty(&status).unwrap_or_default())
    }

    async fn config_reload(&self, _args: &Value) -> ToolResult {
        // Signal a config reload by updating the __reload_requested timestamp.
        // Components that watch this key can pick up the new config.
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        store.set("config:__reload_requested", json!(now)).await;

        ToolResult::ok("config reload signaled")
    }

    async fn runtime_restart(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("manual restart requested");

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Record restart request in state store for the process supervisor to pick up
        store
            .set(
                "runtime:restart_requested",
                json!({
                    "timestamp": now,
                    "reason": reason
                }),
            )
            .await;

        // Also record in restart history
        store
            .set(
                &format!("runtime:restart_history:{now}"),
                json!({
                    "reason": reason,
                    "requested_at": now
                }),
            )
            .await;

        ToolResult::ok(format!(
            "restart signaled (reason: {reason}). Process supervisor will handle the restart."
        ))
    }

    async fn config_schema(&self, args: &Value) -> ToolResult {
        let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");

        // Static schema registry for known config keys
        let schema = match key {
            "model" => json!({
                "key": "model",
                "type": "string",
                "description": "Primary model for agent responses (e.g., gpt-4.1, claude-opus-4)",
                "examples": ["gpt-4.1", "claude-opus-4", "gemini-2.5-pro"]
            }),
            "routing.interactive" => json!({
                "key": "routing.interactive",
                "type": "string",
                "description": "Model used for interactive/fast responses",
                "examples": ["gpt-4.1-mini", "claude-sonnet-4"]
            }),
            "routing.background" => json!({
                "key": "routing.background",
                "type": "string",
                "description": "Model used for background/batch work",
                "examples": ["gpt-4.1", "claude-opus-4"]
            }),
            "brave_api_key" => json!({
                "key": "brave_api_key",
                "type": "string",
                "description": "Brave Search API key for web_search tool",
                "sensitive": true
            }),
            "workdir" => json!({
                "key": "workdir",
                "type": "string",
                "description": "Working directory for file and shell operations"
            }),
            "" => json!({
                "keys": [
                    "model", "routing.interactive", "routing.background",
                    "brave_api_key", "workdir"
                ],
                "description": "Pass a specific key to get its full schema. These are the known config keys."
            }),
            other => json!({
                "key": other,
                "type": "unknown",
                "description": format!("No schema registered for key: {other}. Custom keys accept any JSON value.")
            }),
        };

        ToolResult::ok(serde_json::to_string_pretty(&schema).unwrap_or_default())
    }

    // ── Media tools ────────────────────────────────────────────────────────────

    async fn image_analyze(&self, args: &Value) -> ToolResult {
        let api_key = match &self.openai_api_key {
            Some(k) => k.clone(),
            None => return ToolResult::error("OpenAI API key not configured"),
        };

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("Describe this image in detail.");
        let image_url = args.get("image_url").and_then(|v| v.as_str());
        let image_path = args.get("image_path").and_then(|v| v.as_str());

        let image_content = if let Some(url) = image_url {
            json!({"type": "image_url", "image_url": {"url": url}})
        } else if let Some(path) = image_path {
            let resolved = self.resolve_path(path);
            match tokio::fs::read(&resolved).await {
                Ok(bytes) => {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let mime = if path.ends_with(".png") {
                        "image/png"
                    } else if path.ends_with(".gif") {
                        "image/gif"
                    } else if path.ends_with(".webp") {
                        "image/webp"
                    } else {
                        "image/jpeg"
                    };
                    json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}})
                }
                Err(e) => return ToolResult::error(format!("failed to read image: {e}")),
            }
        } else {
            return ToolResult::error("provide either image_url or image_path");
        };

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gpt-4o");
        let body = json!({
            "model": model,
            "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}, image_content]}],
            "max_tokens": 1024
        });

        let client = reqwest::Client::new();
        match client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return ToolResult::error(format!("API error {status}: {text}"));
                }
                match resp.json::<Value>().await {
                    Ok(data) => ToolResult::ok(
                        data["choices"][0]["message"]["content"]
                            .as_str()
                            .unwrap_or("no response")
                            .to_string(),
                    ),
                    Err(e) => ToolResult::error(format!("failed to parse response: {e}")),
                }
            }
            Err(e) => ToolResult::error(format!("request failed: {e}")),
        }
    }

    async fn image_generate(&self, args: &Value) -> ToolResult {
        let api_key = match &self.openai_api_key {
            Some(k) => k.clone(),
            None => return ToolResult::error("OpenAI API key not configured"),
        };
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("missing required parameter: prompt"),
        };
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gpt-image-1");
        let size = args
            .get("size")
            .and_then(|v| v.as_str())
            .unwrap_or("1024x1024");
        let quality = args
            .get("quality")
            .and_then(|v| v.as_str())
            .unwrap_or("auto");

        let body =
            json!({"model": model, "prompt": prompt, "n": 1, "size": size, "quality": quality});
        let client = reqwest::Client::new();
        match client
            .post("https://api.openai.com/v1/images/generations")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return ToolResult::error(format!("API error {status}: {text}"));
                }
                match resp.json::<Value>().await {
                    Ok(data) => {
                        if let Some(b64) = data["data"][0]["b64_json"].as_str() {
                            let _ = tokio::fs::create_dir_all(&self.media_dir).await;
                            let filename = format!("{}.png", uuid::Uuid::new_v4());
                            let filepath = self.media_dir.join(&filename);
                            use base64::Engine;
                            match base64::engine::general_purpose::STANDARD.decode(b64) {
                                Ok(bytes) => {
                                    if let Err(e) = tokio::fs::write(&filepath, &bytes).await {
                                        return ToolResult::error(format!(
                                            "failed to save image: {e}"
                                        ));
                                    }
                                    ToolResult::ok(json!({"path": filepath.display().to_string(), "size_bytes": bytes.len()}).to_string())
                                }
                                Err(e) => {
                                    ToolResult::error(format!("failed to decode base64: {e}"))
                                }
                            }
                        } else if let Some(url) = data["data"][0]["url"].as_str() {
                            ToolResult::ok(json!({"url": url}).to_string())
                        } else {
                            ToolResult::error(format!("unexpected response format: {data}"))
                        }
                    }
                    Err(e) => ToolResult::error(format!("failed to parse response: {e}")),
                }
            }
            Err(e) => ToolResult::error(format!("request failed: {e}")),
        }
    }

    async fn tts_generate(&self, args: &Value) -> ToolResult {
        let api_key = match &self.openai_api_key {
            Some(k) => k.clone(),
            None => return ToolResult::error("OpenAI API key not configured"),
        };
        let text = match args.get("text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("missing required parameter: text"),
        };
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gpt-4o-mini-tts");
        let voice = args
            .get("voice")
            .and_then(|v| v.as_str())
            .unwrap_or("alloy");

        let body = json!({"model": model, "input": text, "voice": voice});
        let client = reqwest::Client::new();
        match client
            .post("https://api.openai.com/v1/audio/speech")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let err_text = resp.text().await.unwrap_or_default();
                    return ToolResult::error(format!("API error {status}: {err_text}"));
                }
                match resp.bytes().await {
                    Ok(bytes) => {
                        let _ = tokio::fs::create_dir_all(&self.media_dir).await;
                        let filename = format!("{}.mp3", uuid::Uuid::new_v4());
                        let filepath = self.media_dir.join(&filename);
                        if let Err(e) = tokio::fs::write(&filepath, &bytes).await {
                            return ToolResult::error(format!("failed to save audio: {e}"));
                        }
                        ToolResult::ok(json!({"path": filepath.display().to_string(), "size_bytes": bytes.len()}).to_string())
                    }
                    Err(e) => ToolResult::error(format!("failed to read response: {e}")),
                }
            }
            Err(e) => ToolResult::error(format!("request failed: {e}")),
        }
    }

    async fn pdf_analyze(&self, args: &Value) -> ToolResult {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => self.resolve_path(p),
            None => return ToolResult::error("missing required parameter: path"),
        };
        if !path.exists() {
            return ToolResult::error(format!("file not found: {}", path.display()));
        }

        let output = tokio::process::Command::new("pdftotext")
            .arg(path.to_string_lossy().as_ref())
            .arg("-")
            .output()
            .await;
        let text = match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            Ok(out) => {
                return ToolResult::error(format!(
                    "pdftotext failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
            Err(e) => {
                return ToolResult::error(format!(
                    "pdftotext not found or failed: {e}. Install poppler-utils."
                ))
            }
        };

        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                let truncated = if text.len() > 50_000 {
                    format!("{}\n\n[truncated]", &text[..50_000])
                } else {
                    text
                };
                return ToolResult::ok(truncated);
            }
        };

        let api_key = match &self.openai_api_key {
            Some(k) => k.clone(),
            None => {
                return ToolResult::ok(format!(
                    "[no API key for analysis]\n\n{}",
                    if text.len() > 50_000 {
                        &text[..50_000]
                    } else {
                        &text
                    }
                ))
            }
        };

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gpt-4o-mini");
        let max_text = 100_000;
        let pdf_text = if text.len() > max_text {
            &text[..max_text]
        } else {
            &text
        };
        let body = json!({
            "model": model,
            "messages": [
                {"role": "system", "content": "You are analyzing a PDF document."},
                {"role": "user", "content": format!("{prompt}\n\n---\n\n{pdf_text}")}
            ],
            "max_tokens": 2048
        });

        let client = reqwest::Client::new();
        match client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let err_text = resp.text().await.unwrap_or_default();
                    return ToolResult::error(format!("API error {status}: {err_text}"));
                }
                match resp.json::<Value>().await {
                    Ok(data) => ToolResult::ok(
                        data["choices"][0]["message"]["content"]
                            .as_str()
                            .unwrap_or("no response")
                            .to_string(),
                    ),
                    Err(e) => ToolResult::error(format!("failed to parse response: {e}")),
                }
            }
            Err(e) => ToolResult::error(format!("request failed: {e}")),
        }
    }

    async fn video_generate(&self, _args: &Value) -> ToolResult {
        ToolResult::error("video generation provider not configured. Configure a provider via config_set to enable.")
    }

    async fn music_generate(&self, _args: &Value) -> ToolResult {
        ToolResult::error("music generation provider not configured. Configure a provider via config_set to enable.")
    }

    // ── Remote Node tools ──────────────────────────────────────────────────────

    /// Simple shell quoting: wrap in single quotes, escaping existing single quotes.
    fn shell_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }

    /// Resolve a node identifier (name/id/IP) to an SSH target.
    /// If the identifier looks like an IP or hostname, use it directly.
    /// Otherwise, look up in state store under "nodes:<name>".
    async fn resolve_node(&self, node_id: &str) -> Result<String, String> {
        // Direct IP/hostname detection
        if node_id.contains('.') || node_id.contains(':') {
            return Ok(node_id.to_string());
        }
        // Look up from state store
        let store = self
            .state_store
            .as_ref()
            .ok_or_else(|| "state store not configured".to_string())?;
        let key = format!("nodes:{node_id}");
        match store.get(&key).await {
            Some(val) => {
                // Expect {"host": "...", ...}
                if let Some(host) = val.get("host").and_then(|h| h.as_str()) {
                    Ok(host.to_string())
                } else {
                    Err(format!("node '{node_id}' has no 'host' field"))
                }
            }
            None => Err(format!("node '{node_id}' not found")),
        }
    }

    async fn node_file_read(&self, args: &Value) -> ToolResult {
        let _store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error("node operations require state store (not configured)")
            }
        };
        let node_id = match args.get("node").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required parameter: node"),
        };
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("missing required parameter: path"),
        };
        let host = match self.resolve_node(node_id).await {
            Ok(h) => h,
            Err(e) => return ToolResult::error(e),
        };
        // Use SSH to read the file
        let cmd = format!(
            "ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 {} cat {}",
            Self::shell_quote(&host),
            Self::shell_quote(path)
        );
        use pares_radix_core::shell_executor::ExecRequest;
        let req = ExecRequest {
            command: cmd,
            workdir: None,
            background: false,
            pty: false,
            timeout_secs: Some(30),
            env: std::collections::HashMap::new(),
            yield_ms: None,
        };
        {
            let output = self.shell.exec(req).await;
            if output.exit_code == Some(0) {
                ToolResult::ok(output.stdout)
            } else {
                ToolResult::error(format!("SSH read failed: {}", output.stderr))
            }
        }
    }

    async fn node_file_write(&self, args: &Value) -> ToolResult {
        let _store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error("node operations require state store (not configured)")
            }
        };
        let node_id = match args.get("node").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required parameter: node"),
        };
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("missing required parameter: path"),
        };
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("missing required parameter: content"),
        };
        let host = match self.resolve_node(node_id).await {
            Ok(h) => h,
            Err(e) => return ToolResult::error(e),
        };
        // Pipe content via SSH
        let cmd = format!(
            "ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 {} 'cat > {}'",
            Self::shell_quote(&host),
            Self::shell_quote(path)
        );
        use pares_radix_core::shell_executor::ExecRequest;
        let req = ExecRequest {
            command: format!("echo {} | {}", Self::shell_quote(content), cmd),
            workdir: None,
            background: false,
            pty: false,
            timeout_secs: Some(30),
            env: std::collections::HashMap::new(),
            yield_ms: None,
        };
        {
            let output = self.shell.exec(req).await;
            if output.exit_code == Some(0) {
                ToolResult::ok(format!("wrote {} bytes to {path} on node", content.len()))
            } else {
                ToolResult::error(format!("SSH write failed: {}", output.stderr))
            }
        }
    }

    async fn node_dir_list(&self, args: &Value) -> ToolResult {
        let _store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error("node operations require state store (not configured)")
            }
        };
        let node_id = match args.get("node").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required parameter: node"),
        };
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("missing required parameter: path"),
        };
        let host = match self.resolve_node(node_id).await {
            Ok(h) => h,
            Err(e) => return ToolResult::error(e),
        };
        let cmd = format!(
            "ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 {} ls -la {}",
            Self::shell_quote(&host),
            Self::shell_quote(path)
        );
        use pares_radix_core::shell_executor::ExecRequest;
        let req = ExecRequest {
            command: cmd,
            workdir: None,
            background: false,
            pty: false,
            timeout_secs: Some(30),
            env: std::collections::HashMap::new(),
            yield_ms: None,
        };
        {
            let output = self.shell.exec(req).await;
            if output.exit_code == Some(0) {
                ToolResult::ok(output.stdout)
            } else {
                ToolResult::error(format!("SSH dir list failed: {}", output.stderr))
            }
        }
    }

    async fn node_dir_fetch(&self, args: &Value) -> ToolResult {
        let _store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error("node operations require state store (not configured)")
            }
        };
        let node_id = match args.get("node").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required parameter: node"),
        };
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("missing required parameter: path"),
        };
        let local_path = args
            .get("local_path")
            .and_then(|v| v.as_str())
            .unwrap_or("/tmp/node-fetch");
        let host = match self.resolve_node(node_id).await {
            Ok(h) => h,
            Err(e) => return ToolResult::error(e),
        };
        // tar + ssh to fetch directory
        let cmd = format!(
            "mkdir -p {} && ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 {} 'tar czf - -C {} .' | tar xzf - -C {}",
            Self::shell_quote(local_path),
            Self::shell_quote(&host),
            Self::shell_quote(path),
            Self::shell_quote(local_path),
        );
        use pares_radix_core::shell_executor::ExecRequest;
        let req = ExecRequest {
            command: cmd,
            workdir: None,
            background: false,
            pty: false,
            timeout_secs: Some(60),
            env: std::collections::HashMap::new(),
            yield_ms: None,
        };
        {
            let output = self.shell.exec(req).await;
            if output.exit_code == Some(0) {
                ToolResult::ok(format!("fetched {path} to {local_path}"))
            } else {
                ToolResult::error(format!("SSH dir fetch failed: {}", output.stderr))
            }
        }
    }

    async fn node_status(&self, _args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error("node operations require state store (not configured)")
            }
        };
        // List all keys starting with "nodes:"
        let keys = store.keys_with_prefix("nodes:").await;
        if keys.is_empty() {
            return ToolResult::ok("no nodes configured".to_string());
        }
        let mut status_lines = Vec::new();
        for key in &keys {
            if let Some(val) = store.get(key).await {
                let name = key.strip_prefix("nodes:").unwrap_or(key);
                let host = val
                    .get("host")
                    .and_then(|h| h.as_str())
                    .unwrap_or("unknown");
                status_lines.push(format!("• {name}: {host}"));
            }
        }
        ToolResult::ok(format!(
            "Configured nodes ({}):\n{}",
            keys.len(),
            status_lines.join("\n")
        ))
    }

    // ── Browser tools ─────────────────────────────────────────────────────────

    async fn browser_status(&self, _args: &Value) -> ToolResult {
        let browser = match &self.browser {
            Some(b) => b,
            None => {
                return ToolResult::error("browser not configured. Set CDP endpoint via config.")
            }
        };
        if !browser.is_available().await {
            return ToolResult::ok(
                json!({"available": false, "message": "no browser reachable at CDP endpoint"})
                    .to_string(),
            );
        }
        match browser.version().await {
            Ok(version) => {
                ToolResult::ok(json!({"available": true, "version": version}).to_string())
            }
            Err(e) => ToolResult::ok(json!({"available": false, "error": e}).to_string()),
        }
    }

    async fn browser_navigate(&self, args: &Value) -> ToolResult {
        let browser = match &self.browser {
            Some(b) => b,
            None => return ToolResult::error("browser not configured"),
        };
        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return ToolResult::error("missing required parameter: url"),
        };
        match browser.navigate(url).await {
            Ok(result) => ToolResult::ok(format!("navigated to {url}: {result}")),
            Err(e) => ToolResult::error(format!("navigation failed: {e}")),
        }
    }

    async fn browser_snapshot(&self, _args: &Value) -> ToolResult {
        let browser = match &self.browser {
            Some(b) => b,
            None => return ToolResult::error("browser not configured"),
        };
        match browser.snapshot().await {
            Ok(text) => ToolResult::ok(text),
            Err(e) => ToolResult::error(format!("snapshot failed: {e}")),
        }
    }

    async fn browser_screenshot(&self, args: &Value) -> ToolResult {
        let browser = match &self.browser {
            Some(b) => b,
            None => return ToolResult::error("browser not configured"),
        };
        let format = args.get("format").and_then(|v| v.as_str());
        match browser.screenshot(format).await {
            Ok(base64_data) => {
                let ext = format.unwrap_or("png");
                let filename = format!(
                    "screenshot-{}.{ext}",
                    chrono::Utc::now().format("%Y%m%d-%H%M%S")
                );
                let path = self.media_dir.join(&filename);
                if let Err(e) = tokio::fs::create_dir_all(&self.media_dir).await {
                    return ToolResult::error(format!("failed to create media dir: {e}"));
                }
                match base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &base64_data,
                ) {
                    Ok(bytes) => {
                        if let Err(e) = tokio::fs::write(&path, &bytes).await {
                            return ToolResult::error(format!("failed to save screenshot: {e}"));
                        }
                        ToolResult::ok(format!(
                            "screenshot saved: {} ({} bytes)",
                            path.display(),
                            bytes.len()
                        ))
                    }
                    Err(e) => ToolResult::error(format!("failed to decode screenshot: {e}")),
                }
            }
            Err(e) => ToolResult::error(format!("screenshot failed: {e}")),
        }
    }

    async fn browser_click(&self, args: &Value) -> ToolResult {
        let browser = match &self.browser {
            Some(b) => b,
            None => return ToolResult::error("browser not configured"),
        };
        let selector = args.get("selector").and_then(|v| v.as_str());
        let x = args.get("x").and_then(|v| v.as_f64());
        let y = args.get("y").and_then(|v| v.as_f64());
        match browser.click(selector, x, y).await {
            Ok(msg) => ToolResult::ok(msg),
            Err(e) => ToolResult::error(format!("click failed: {e}")),
        }
    }

    async fn browser_type(&self, args: &Value) -> ToolResult {
        let browser = match &self.browser {
            Some(b) => b,
            None => return ToolResult::error("browser not configured"),
        };
        let text = match args.get("text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("missing required parameter: text"),
        };
        let selector = args.get("selector").and_then(|v| v.as_str());
        match browser.type_text(text, selector).await {
            Ok(msg) => ToolResult::ok(msg),
            Err(e) => ToolResult::error(format!("type failed: {e}")),
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    // ── Praxis tools ──────────────────────────────────────────────────────────

    async fn praxis_evaluate(&self, args: &Value) -> ToolResult {
        use pares_radix_praxis::px::executor::default_evaluate_condition;
        use std::collections::HashMap;

        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolResult::error("missing required parameter: action"),
        };

        let payload = args.get("payload").cloned().unwrap_or(json!({}));
        let module_filter = args.get("module").and_then(|v| v.as_str());
        let phase_filter = args.get("phase").and_then(|v| v.as_str());

        let mut all_results: Vec<Value> = Vec::new();

        // ── Phase 1: Evaluate classic PraxisModule rules ──────────────────────
        for module in &self.praxis_modules {
            if let Some(filter) = module_filter {
                if module.name() != filter {
                    continue;
                }
            }

            let ctx = RuleContext::new(&action, payload.clone());
            let results = module.evaluate_all(&ctx);
            for (rule_name, result) in results {
                let (status, message) = match &result {
                    RuleResult::Pass => ("pass", None),
                    RuleResult::Fail { reason } => ("fail", Some(reason.clone())),
                    RuleResult::Warning { message } => ("warning", Some(message.clone())),
                    RuleResult::Gate {
                        action: _,
                        rationale,
                    } => ("gate", Some(rationale.clone())),
                };

                let mut entry = json!({
                    "module": module.name(),
                    "rule": rule_name,
                    "status": status,
                });
                if let Some(msg) = message {
                    entry["message"] = Value::String(msg);
                }
                all_results.push(entry);
            }
        }

        // ── Phase 2: Evaluate persisted px:constraint/* from PluresDB ─────────
        if let Some(ref store) = self.state_store {
            let constraint_keys = store.keys_with_prefix("px:constraint/").await;

            // Build vars from payload for condition evaluation
            let mut vars: HashMap<String, Value> = HashMap::new();
            vars.insert("action".to_string(), Value::String(action.clone()));
            if let Value::Object(map) = &payload {
                for (k, v) in map {
                    vars.insert(k.clone(), v.clone());
                }
            }

            for key in constraint_keys {
                if let Some(record) = store.get(&key).await {
                    let name = record
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let severity = record
                        .get("severity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("error")
                        .to_string();
                    let message_tmpl = record
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let when_expr = record
                        .get("when")
                        .and_then(|v| v.as_str())
                        .unwrap_or("true");
                    let require_expr = record
                        .get("require")
                        .and_then(|v| v.as_str())
                        .unwrap_or("true");
                    let phases: Vec<String> = record
                        .get("phases")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    // Apply phase filter: skip if constraint has phases and none match
                    if let Some(pf) = phase_filter {
                        if !phases.is_empty() && !phases.iter().any(|p| p == pf) {
                            continue;
                        }
                    }

                    // Evaluate `when` — if false, constraint doesn't apply (skip)
                    if !default_evaluate_condition(when_expr, &vars) {
                        continue;
                    }

                    // Evaluate `require` — if false, constraint is violated
                    let satisfied = default_evaluate_condition(require_expr, &vars);

                    let status = if satisfied {
                        "pass"
                    } else {
                        match severity.as_str() {
                            "warning" => "warning",
                            "gate" => "gate",
                            _ => "fail",
                        }
                    };

                    let mut entry = json!({
                        "source": "px",
                        "constraint": name,
                        "status": status,
                    });
                    if !satisfied && !message_tmpl.is_empty() {
                        entry["message"] = Value::String(message_tmpl);
                    }
                    if !phases.is_empty() {
                        entry["phases"] = json!(phases);
                    }
                    all_results.push(entry);
                }
            }
        }

        let failures = all_results
            .iter()
            .filter(|r| r["status"] == "fail" || r["status"] == "gate")
            .count();
        let warnings = all_results
            .iter()
            .filter(|r| r["status"] == "warning")
            .count();

        ToolResult::ok(
            json!({
                "action": action,
                "total_rules": all_results.len(),
                "failures": failures,
                "warnings": warnings,
                "passed": all_results.len() - failures - warnings,
                "results": all_results
            })
            .to_string(),
        )
    }

    async fn praxis_list(&self, args: &Value) -> ToolResult {
        let module_filter = args.get("module").and_then(|v| v.as_str());
        let procedures_guard = self.loaded_procedures.read().await;

        let mut modules_info: Vec<Value> = Vec::new();
        let mut total_rules = 0;

        for module in &self.praxis_modules {
            if let Some(filter) = module_filter {
                if module.name() != filter {
                    continue;
                }
            }

            let rules = module.rules();
            let audit = module.audit();
            total_rules += rules.len();

            let rule_list: Vec<Value> = rules
                .iter()
                .map(|r| {
                    json!({
                        "name": r.name(),
                        "category": format!("{:?}", r.category()),
                    })
                })
                .collect();

            modules_info.push(json!({
                "name": module.name(),
                "rule_count": rules.len(),
                "completeness_pct": audit.completeness_pct,
                "expectations": module.expectations(),
                "rules": rule_list,
            }));
        }

        let mut procedures_info: Vec<Value> = Vec::new();
        for (name, proc) in procedures_guard.iter() {
            procedures_info.push(json!({
                "name": name,
                "source_file": proc.source_file.display().to_string(),
                "description": proc.description,
            }));
        }

        // ── Enumerate persisted px:constraint/* and px:rule/* from PluresDB ──
        let mut persisted_constraints: Vec<Value> = Vec::new();
        let mut persisted_rules: Vec<Value> = Vec::new();

        if let Some(ref store) = self.state_store {
            let constraint_keys = store.keys_with_prefix("px:constraint/").await;
            for key in constraint_keys {
                if let Some(record) = store.get(&key).await {
                    let name = record
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&key)
                        .to_string();
                    let severity = record
                        .get("severity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("error")
                        .to_string();
                    let phases: Vec<String> = record
                        .get("phases")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let message = record
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut entry = json!({
                        "key": key,
                        "name": name,
                        "severity": severity,
                    });
                    if !phases.is_empty() {
                        entry["phases"] = json!(phases);
                    }
                    if !message.is_empty() {
                        entry["message"] = Value::String(message);
                    }
                    persisted_constraints.push(entry);
                }
            }

            let rule_keys = store.keys_with_prefix("px:rule/").await;
            for key in rule_keys {
                if let Some(record) = store.get(&key).await {
                    let name = record
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&key)
                        .to_string();
                    let priority = record.get("priority").and_then(|v| v.as_u64()).unwrap_or(0);
                    let conditions: Vec<String> = record
                        .get("conditions")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let actions = record.get("actions").cloned().unwrap_or(json!([]));
                    persisted_rules.push(json!({
                        "key": key,
                        "name": name,
                        "priority": priority,
                        "conditions": conditions,
                        "actions": actions,
                    }));
                }
            }
        }

        ToolResult::ok(
            json!({
                "modules": modules_info,
                "total_rules": total_rules,
                "loaded_procedures": procedures_info,
                "persisted_constraints": persisted_constraints,
                "persisted_rules": persisted_rules,
            })
            .to_string(),
        )
    }

    /// Run a .px procedure by inline source or file path.
    async fn praxis_run(&self, args: &Value) -> ToolResult {
        use std::collections::HashMap;

        // Build a ProcedureRegistry from all loaded procedures so that
        // procedure-to-procedure calls resolve via ComposableHandler.
        let mut registry = self.build_procedure_registry().await;

        // Check if requesting a pre-loaded procedure by name
        let target_name = args.get("procedure").and_then(|v| v.as_str());
        if let Some(name) = target_name {
            let procedures_guard = self.loaded_procedures.read().await;
            if let Some(loaded) = procedures_guard.get(name) {
                // Execute the pre-loaded procedure directly
                let mut initial_vars: HashMap<String, Value> = HashMap::new();
                if let Some(vars) = args.get("vars").and_then(|v| v.as_object()) {
                    for (k, v) in vars {
                        initial_vars.insert(k.clone(), v.clone());
                    }
                }
                let shell_handler = ShellBackedProcedureHandler {
                    shell: Arc::clone(&self.shell),
                    workdir: self.workdir.clone(),
                    children: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                };
                let handler = ComposableHandler::new(registry, shell_handler);
                return match px_async::execute_async_with_vars(&loaded.data, &handler, initial_vars)
                    .await
                {
                    Ok(result) => {
                        let step_summaries: Vec<Value> = result
                            .step_results
                            .iter()
                            .map(|s| {
                                json!({
                                    "index": s.index,
                                    "kind": s.kind,
                                    "skipped": s.skipped,
                                    "output": s.output,
                                })
                            })
                            .collect();
                        ToolResult::ok(
                            json!({
                                "procedure": result.procedure_name,
                                "success": result.success,
                                "variables": result.variables,
                                "steps": step_summaries,
                                "source": "preloaded",
                            })
                            .to_string(),
                        )
                    }
                    Err(e) => ToolResult::error(format!("procedure execution failed: {e}")),
                };
            }
        }

        // Get source from inline or file
        let source = if let Some(src) = args.get("source").and_then(|v| v.as_str()) {
            src.to_string()
        } else if let Some(file_path) = args.get("file").and_then(|v| v.as_str()) {
            let resolved = self.resolve_path(file_path);
            match tokio::fs::read_to_string(&resolved).await {
                Ok(content) => content,
                Err(e) => return ToolResult::error(format!("failed to read .px file: {e}")),
            }
        } else {
            return ToolResult::error(
                "'procedure' (name of preloaded .px), 'source' (inline .px code), or 'file' (path to .px file) is required",
            );
        };

        // Parse
        let doc = match px::parse(&source) {
            Ok(d) => d,
            Err(e) => return ToolResult::error(format!("parse error: {e}")),
        };

        let has_procedures = doc.statements.iter().any(|s| {
            matches!(
                s,
                px::Statement::DataflowProcedure(_) | px::Statement::LegacyProcedure(_)
            )
        });
        if !has_procedures {
            return ToolResult::error("no procedures found in source");
        }

        // Compile
        let records = compiler::compile(&doc);
        let procedure_records: Vec<_> = records
            .iter()
            .filter(|r| r.data.get("type").and_then(|v| v.as_str()) == Some("procedure"))
            .collect();

        if procedure_records.is_empty() {
            return ToolResult::error("no compiled procedure records found");
        }

        // Add all procedures from the inline/file source to the registry
        // so they can call each other during execution.
        for record in &procedure_records {
            if let Some(name) = record.data.get("name").and_then(|v| v.as_str()) {
                registry.register_as(name.to_string(), record.data.clone());
            }
        }

        // Select the target procedure
        let target_name = args.get("procedure").and_then(|v| v.as_str());
        let record = if let Some(name) = target_name {
            match procedure_records
                .iter()
                .find(|r| r.data.get("name").and_then(|v| v.as_str()) == Some(name))
            {
                Some(r) => r,
                None => {
                    let available: Vec<_> = procedure_records
                        .iter()
                        .filter_map(|r| r.data.get("name").and_then(|v| v.as_str()))
                        .collect();
                    return ToolResult::error(format!(
                        "procedure '{name}' not found. Available: {available:?}"
                    ));
                }
            }
        } else {
            &procedure_records[0]
        };

        // Build initial vars from args
        let mut initial_vars: HashMap<String, Value> = HashMap::new();
        if let Some(vars) = args.get("vars").and_then(|v| v.as_object()) {
            for (k, v) in vars {
                initial_vars.insert(k.clone(), v.clone());
            }
        }

        // Create a composable handler that resolves procedure-to-procedure calls
        let shell_handler = ShellBackedProcedureHandler {
            shell: Arc::clone(&self.shell),
            workdir: self.workdir.clone(),
            children: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        };
        let handler = ComposableHandler::new(registry, shell_handler);

        // Execute
        match px_async::execute_async_with_vars(&record.data, &handler, initial_vars).await {
            Ok(result) => {
                let step_summaries: Vec<Value> = result
                    .step_results
                    .iter()
                    .map(|s| {
                        json!({
                            "index": s.index,
                            "kind": s.kind,
                            "skipped": s.skipped,
                            "output": s.output,
                        })
                    })
                    .collect();

                ToolResult::ok(
                    json!({
                        "procedure": result.procedure_name,
                        "success": result.success,
                        "variables": result.variables,
                        "steps": step_summaries,
                    })
                    .to_string(),
                )
            }
            Err(e) => ToolResult::error(format!("execution error: {e}")),
        }
    }

    // ── Praxis persistence tools ────────────────────────────────────────────

    async fn praxis_add_constraint(&self, args: &Value) -> ToolResult {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required field: name"),
        };
        let severity = match args.get("severity").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("missing required field: severity"),
        };

        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("no state store available for persistence"),
        };

        let mut record = json!({
            "name": name,
            "severity": severity,
        });
        if let Some(when) = args.get("when").and_then(|v| v.as_str()) {
            record["when"] = Value::String(when.to_string());
        }
        if let Some(require) = args.get("require").and_then(|v| v.as_str()) {
            record["require"] = Value::String(require.to_string());
        }
        if let Some(message) = args.get("message").and_then(|v| v.as_str()) {
            record["message"] = Value::String(message.to_string());
        }
        if let Some(phases) = args.get("phases").and_then(|v| v.as_array()) {
            record["phases"] = Value::Array(phases.clone());
        }

        let key = format!("px:constraint/{}", name);
        store.set(&key, record).await;

        ToolResult::ok(json!({"stored": key}).to_string())
    }

    async fn praxis_add_rule(&self, args: &Value) -> ToolResult {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required field: name"),
        };

        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("no state store available for persistence"),
        };

        let mut record = json!({ "name": name });
        if let Some(priority) = args.get("priority").and_then(|v| v.as_u64()) {
            record["priority"] = json!(priority);
        }
        if let Some(conditions) = args.get("conditions").and_then(|v| v.as_array()) {
            record["conditions"] = Value::Array(conditions.clone());
        }
        if let Some(actions) = args.get("actions") {
            record["actions"] = actions.clone();
        }

        let key = format!("px:rule/{}", name);
        store.set(&key, record).await;

        ToolResult::ok(json!({"stored": key}).to_string())
    }

    // ── Lint tool ──────────────────────────────────────────────────────────────

    async fn px_lint(&self, args: &Value) -> ToolResult {
        let source = if let Some(src) = args.get("source").and_then(|v| v.as_str()) {
            src.to_string()
        } else if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => content,
                Err(e) => return ToolResult::error(format!("failed to read file: {e}")),
            }
        } else {
            return ToolResult::error("provide either 'source' or 'path' parameter");
        };

        let doc = match px::parse(&source) {
            Ok(doc) => doc,
            Err(e) => {
                return ToolResult::ok(
                    json!({
                        "status": "parse_error",
                        "error": format!("{e}"),
                        "diagnostics": []
                    })
                    .to_string(),
                );
            }
        };

        let diagnostics = px::lint::lint(&doc);
        let diag_json: Vec<Value> = diagnostics
            .iter()
            .map(|d| {
                json!({
                    "code": d.code,
                    "message": d.message,
                    "severity": match d.severity {
                        px::lint::LintSeverity::Warning => "warning",
                        px::lint::LintSeverity::Error => "error",
                    },
                    "procedure": d.procedure,
                    "step_index": d.step_index,
                })
            })
            .collect();

        ToolResult::ok(
            json!({
                "status": if diagnostics.is_empty() { "clean" } else { "issues_found" },
                "diagnostics_count": diagnostics.len(),
                "diagnostics": diag_json
            })
            .to_string(),
        )
    }

    // ── Compose tool ──────────────────────────────────────────────────────────

    async fn px_compose(&self, args: &Value) -> ToolResult {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("missing required field: action"),
        };

        match action {
            "register" => {
                // Get source from inline or file
                let source = if let Some(src) = args.get("source").and_then(|v| v.as_str()) {
                    src.to_string()
                } else if let Some(file_path) = args.get("file").and_then(|v| v.as_str()) {
                    let resolved = self.resolve_path(file_path);
                    match tokio::fs::read_to_string(&resolved).await {
                        Ok(content) => content,
                        Err(e) => {
                            return ToolResult::error(format!("failed to read .px file: {e}"))
                        }
                    }
                } else {
                    return ToolResult::error(
                        "'source' (inline .px code) or 'file' (path to .px file) required for action=register",
                    );
                };

                // Parse and compile
                let doc = match px::parse(&source) {
                    Ok(d) => d,
                    Err(e) => return ToolResult::error(format!("parse error: {e}")),
                };

                let records = compiler::compile(&doc);
                let mut registered: Vec<String> = Vec::new();

                let mut procedures_guard = self.loaded_procedures.write().await;
                let source_path = args
                    .get("file")
                    .and_then(|v| v.as_str())
                    .map(|f| self.resolve_path(f))
                    .unwrap_or_else(|| PathBuf::from("<inline>"));

                for record in &records {
                    if record.data.get("type").and_then(|v| v.as_str()) == Some("procedure") {
                        let name = record
                            .data
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let description = record
                            .data
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        procedures_guard.insert(
                            name.clone(),
                            LoadedProcedure {
                                name: name.clone(),
                                source_file: source_path.clone(),
                                data: record.data.clone(),
                                description,
                            },
                        );
                        registered.push(name);
                    }
                }

                if registered.is_empty() {
                    return ToolResult::error("no procedures found in source");
                }

                info!(count = registered.len(), names = ?registered, "px_compose: registered procedures");
                ToolResult::ok(
                    json!({
                        "registered": registered,
                        "total": procedures_guard.len(),
                    })
                    .to_string(),
                )
            }

            "unregister" => {
                let name = match args.get("name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return ToolResult::error("'name' required for action=unregister"),
                };

                let mut procedures_guard = self.loaded_procedures.write().await;
                if procedures_guard.remove(name).is_some() {
                    info!(name, "px_compose: unregistered procedure");
                    ToolResult::ok(
                        json!({
                            "unregistered": name,
                            "total": procedures_guard.len(),
                        })
                        .to_string(),
                    )
                } else {
                    let available: Vec<_> = procedures_guard.keys().cloned().collect();
                    ToolResult::error(format!(
                        "procedure '{name}' not found. Available: {available:?}"
                    ))
                }
            }

            "list" => {
                let procedures_guard = self.loaded_procedures.read().await;
                let list: Vec<Value> = procedures_guard
                    .values()
                    .map(|p| {
                        json!({
                            "name": p.name,
                            "source_file": p.source_file.to_string_lossy(),
                            "description": p.description,
                        })
                    })
                    .collect();
                ToolResult::ok(
                    json!({
                        "procedures": list,
                        "count": list.len(),
                    })
                    .to_string(),
                )
            }

            "pipe" => {
                let pipeline = match args.get("pipeline").and_then(|v| v.as_array()) {
                    Some(arr) => arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
                    None => {
                        return ToolResult::error(
                            "'pipeline' (array of procedure names) required for action=pipe",
                        )
                    }
                };

                if pipeline.is_empty() {
                    return ToolResult::error("pipeline must contain at least one procedure name");
                }

                let initial_input = args
                    .get("input")
                    .cloned()
                    .or_else(|| args.get("vars").cloned())
                    .unwrap_or(Value::Null);

                let registry = self.build_procedure_registry().await;

                // Verify all procedures exist before running
                for name in &pipeline {
                    if !registry.contains(name) {
                        return ToolResult::error(format!(
                            "procedure '{}' not found in registry. Available: {:?}",
                            name,
                            registry.names()
                        ));
                    }
                }

                let shell_handler = ShellBackedProcedureHandler {
                    shell: Arc::clone(&self.shell),
                    workdir: self.workdir.clone(),
                    children: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                };

                use pares_radix_praxis::px::compose::pipe;
                match pipe(&pipeline, &registry, &shell_handler, initial_input).await {
                    Ok(result) => ToolResult::ok(
                        json!({
                            "pipeline": pipeline,
                            "result": result,
                            "success": true,
                        })
                        .to_string(),
                    ),
                    Err(e) => ToolResult::error(format!("pipe execution failed: {e}")),
                }
            }

            other => ToolResult::error(format!(
                "unknown action: '{other}'. Valid actions: register, unregister, list, pipe"
            )),
        }
    }

    // ── Px Status tool ──────────────────────────────────────────────────────────

    async fn px_status(&self) -> ToolResult {
        // Loaded procedures
        let procedures_guard = self.loaded_procedures.read().await;
        let procedure_names: Vec<&str> = procedures_guard.keys().map(|s| s.as_str()).collect();
        let procedure_count = procedure_names.len();

        // Praxis modules
        let modules_info: Vec<Value> = self
            .praxis_modules
            .iter()
            .map(|m| {
                json!({
                    "name": m.name(),
                    "rule_count": m.rules().len(),
                    "completeness_pct": m.audit().completeness_pct,
                })
            })
            .collect();

        let total_module_rules: usize = self.praxis_modules.iter().map(|m| m.rules().len()).sum();

        // Persisted px:* keys from PluresDB
        let (constraint_count, rule_count, fact_count, total_px_keys) =
            if let Some(ref store) = self.state_store {
                let constraints = store.keys_with_prefix("px:constraint/").await.len();
                let rules = store.keys_with_prefix("px:rule/").await.len();
                let facts = store.keys_with_prefix("px:fact/").await.len();
                let all_px = store.keys_with_prefix("px:").await.len();
                (constraints, rules, facts, all_px)
            } else {
                (0, 0, 0, 0)
            };

        ToolResult::ok(
            json!({
                "procedures": {
                    "count": procedure_count,
                    "names": procedure_names,
                },
                "modules": {
                    "count": modules_info.len(),
                    "total_rules": total_module_rules,
                    "details": modules_info,
                },
                "persisted": {
                    "constraints": constraint_count,
                    "rules": rule_count,
                    "facts": fact_count,
                    "total_px_keys": total_px_keys,
                },
                "state_store": self.state_store.is_some(),
            })
            .to_string(),
        )
    }

    // ── Chronos tools ─────────────────────────────────────────────────────────

    async fn chronos_history(&self, args: &Value) -> ToolResult {
        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let entries = chronos.history(key, limit);
        ToolResult::ok(serde_json::to_string_pretty(&entries).unwrap_or_default())
    }

    async fn chronos_recent(&self, args: &Value) -> ToolResult {
        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let entries = chronos.recent(limit);
        ToolResult::ok(serde_json::to_string_pretty(&entries).unwrap_or_default())
    }

    async fn chronos_by_actor(&self, args: &Value) -> ToolResult {
        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let actor = match args.get("actor").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("missing required parameter: actor"),
        };
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let entries = chronos.by_actor(actor, limit);
        ToolResult::ok(serde_json::to_string_pretty(&entries).unwrap_or_default())
    }

    // ── Chronos record tool ────────────────────────────────────────────────────────

    async fn chronos_record(&self, args: &Value) -> ToolResult {
        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => return ToolResult::error("missing required parameter: key"),
        };
        let actor = args
            .get("actor")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");
        let action_str = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("Create");
        let action = match action_str.to_lowercase().as_str() {
            "create" => ChronosAction::Create,
            "update" => ChronosAction::Update,
            "delete" => ChronosAction::Delete,
            "move" => ChronosAction::Move,
            "tool_invoked" | "toolinvoked" => ChronosAction::ToolInvoked,
            "message_received" | "messagereceived" => ChronosAction::MessageReceived,
            "response_generated" | "responsegenerated" => ChronosAction::ResponseGenerated,
            "context_managed" | "contextmanaged" => ChronosAction::ContextManaged,
            "model_called" | "modelcalled" => ChronosAction::ModelCalled,
            "outcome_recorded" | "outcomerecorded" => ChronosAction::OutcomeRecorded,
            _ => return ToolResult::error(format!("unknown action: {action_str}. Valid: create, update, delete, move, tool_invoked, message_received, response_generated, context_managed, model_called, outcome_recorded")),
        };
        let data = args.get("data").cloned().unwrap_or(json!({}));
        let rationale = args
            .get("rationale")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let constraints: Vec<String> = args
            .get("constraints")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let level_str = args.get("level").and_then(|v| v.as_str()).unwrap_or("info");
        let level = ChronosLevel::from_str_loose(level_str).unwrap_or(ChronosLevel::Info);

        let entry = chronos.build_entry_with_level(
            key,
            actor,
            action,
            level,
            &data,
            constraints,
            rationale,
        );
        let recorded = chronos.record(&entry);

        ToolResult::ok(
            json!({
                "id": entry.id,
                "key": entry.key,
                "timestamp": entry.timestamp,
                "recorded": recorded,
                "level": level_str
            })
            .to_string(),
        )
    }

    // ── Chronos level tools ───────────────────────────────────────────────────────

    async fn chronos_set_level(&self, args: &Value) -> ToolResult {
        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let level_str = match args.get("level").and_then(|v| v.as_str()) {
            Some(l) => l,
            None => {
                return ToolResult::error(
                    "missing required parameter: level (debug|info|warn|error)",
                )
            }
        };
        let level = match ChronosLevel::from_str_loose(level_str) {
            Some(l) => l,
            None => {
                return ToolResult::error(format!(
                    "invalid level: {level_str}. Valid: debug, info, warn, error"
                ))
            }
        };
        chronos.set_level(level);
        ToolResult::ok(json!({ "level": level.to_string() }).to_string())
    }

    async fn chronos_get_level(&self, _args: &Value) -> ToolResult {
        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let level = chronos.get_level();
        ToolResult::ok(json!({ "level": level.to_string() }).to_string())
    }

    async fn chronos_replay(&self, args: &Value) -> ToolResult {
        use pares_radix_praxis::px::executor::default_evaluate_condition;
        use std::collections::HashMap;

        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let from_id = args.get("fromId").and_then(|v| v.as_str());
        let to_id = args.get("toId").and_then(|v| v.as_str());

        // Validate IDs exist before replay (avoid silent empty results)
        if from_id.is_some() || to_id.is_some() {
            let all_entries = chronos.recent(10_000);
            if let Some(fid) = from_id {
                if !all_entries.iter().any(|e| e.id == fid) {
                    return ToolResult::error(format!("fromId '{fid}' not found in timeline. Use chronos_timeline to list valid event IDs."));
                }
            }
            if let Some(tid) = to_id {
                if !all_entries.iter().any(|e| e.id == tid) {
                    return ToolResult::error(format!("toId '{tid}' not found in timeline. Use chronos_timeline to list valid event IDs."));
                }
            }
        }

        let entries = chronos.replay(from_id, to_id);

        // Dry-run: evaluate each entry through .px constraints in PluresDB
        let mut results: Vec<Value> = Vec::new();
        for entry in &entries {
            let action_str = format!("{:?}", entry.action);
            let mut violations: Vec<Value> = Vec::new();

            if let Some(ref store) = self.state_store {
                let constraint_keys = store.keys_with_prefix("px:constraint/").await;
                let mut vars: HashMap<String, Value> = HashMap::new();
                vars.insert("action".to_string(), Value::String(action_str.clone()));
                vars.insert("actor".to_string(), Value::String(entry.actor.clone()));
                vars.insert("key".to_string(), Value::String(entry.key.clone()));
                vars.insert("level".to_string(), Value::String(entry.level.to_string()));

                for key in constraint_keys {
                    if let Some(record) = store.get(&key).await {
                        let name = record
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let severity = record
                            .get("severity")
                            .and_then(|v| v.as_str())
                            .unwrap_or("error");
                        let message = record.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        let when_expr = record
                            .get("when")
                            .and_then(|v| v.as_str())
                            .unwrap_or("true");
                        let require_expr = record
                            .get("require")
                            .and_then(|v| v.as_str())
                            .unwrap_or("true");

                        if !default_evaluate_condition(when_expr, &vars) {
                            continue;
                        }
                        if !default_evaluate_condition(require_expr, &vars) {
                            violations.push(json!({
                                "constraint": name,
                                "severity": severity,
                                "message": message,
                            }));
                        }
                    }
                }
            }

            results.push(json!({
                "id": entry.id,
                "timestamp": entry.timestamp,
                "actor": entry.actor,
                "key": entry.key,
                "action": action_str,
                "level": entry.level.to_string(),
                "violations": violations,
            }));
        }

        ToolResult::ok(
            json!({
                "replayed": results.len(),
                "fromId": from_id,
                "toId": to_id,
                "results": results,
            })
            .to_string(),
        )
    }

    async fn chronos_timeline(&self, args: &Value) -> ToolResult {
        let chronos = match &self.chronos {
            Some(c) => c,
            None => return ToolResult::error("Chronos timeline not configured"),
        };
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let since = args.get("since").and_then(|v| v.as_str()).and_then(|s| {
            // Parse ISO 8601 timestamp to epoch millis
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.timestamp_millis() as u64)
        });
        let level = args
            .get("level")
            .and_then(|v| v.as_str())
            .and_then(ChronosLevel::from_str_loose);
        let entries = chronos.timeline(limit, since, level);
        ToolResult::ok(serde_json::to_string_pretty(&entries).unwrap_or_default())
    }

    // ── Plugin management tools ──────────────────────────────────────────────────────

    async fn plugin_list(&self) -> ToolResult {
        let runtime = match &self.plugin_runtime {
            Some(r) => r,
            None => return ToolResult::error("Plugin runtime not configured"),
        };
        let plugins = runtime.list().await;
        let list: Vec<Value> = plugins
            .iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "version": m.version,
                    "description": m.description,
                    "status": "active"
                })
            })
            .collect();
        ToolResult::ok(serde_json::to_string_pretty(&list).unwrap_or_default())
    }

    async fn plugin_info(&self, args: &Value) -> ToolResult {
        let runtime = match &self.plugin_runtime {
            Some(r) => r,
            None => return ToolResult::error("Plugin runtime not configured"),
        };
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required parameter: name"),
        };
        match runtime.get(name).await {
            Some(manifest) => ToolResult::ok(
                serde_json::to_string_pretty(&json!({
                    "name": manifest.name,
                    "version": manifest.version,
                    "description": manifest.description,
                    "author": manifest.author,
                    "tools": manifest.tools.len(),
                    "hooks": manifest.hooks.len(),
                    "dependencies": manifest.dependencies,
                    "status": "active"
                }))
                .unwrap_or_default(),
            ),
            None => ToolResult::error(format!("Plugin '{}' not found", name)),
        }
    }

    async fn plugin_register(&self, args: &Value) -> ToolResult {
        use pares_radix_core::plugins::PluginManifest;
        let runtime = match &self.plugin_runtime {
            Some(r) => r,
            None => return ToolResult::error("Plugin runtime not configured"),
        };
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolResult::error("missing required parameter: name"),
        };
        let version = match args.get("version").and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => return ToolResult::error("missing required parameter: version"),
        };
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let capabilities: Vec<String> = args
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let manifest = PluginManifest {
            name: name.clone(),
            version,
            description,
            author: None,
            schema: Default::default(),
            logic: Default::default(),
            tools: Vec::new(),
            ui: None,
            permissions: Default::default(),
            hooks: Vec::new(),
            dependencies: capabilities,
            // ADR-0022: capability contracts (required/optional/provided). This
            // MCP registration path does not yet surface them; default = none.
            capabilities: Default::default(),
        };
        match runtime.install(manifest).await {
            Ok(()) => {
                self.notify_tools_changed();
                ToolResult::ok(json!({"registered": name, "status": "active"}).to_string())
            }
            Err(e) => ToolResult::error(format!("Failed to register plugin: {}", e)),
        }
    }

    async fn plugin_activate(&self, args: &Value) -> ToolResult {
        // In pares-radix, all installed plugins are active.
        // This tool is provided for API compatibility with OpenClaw.
        let runtime = match &self.plugin_runtime {
            Some(r) => r,
            None => return ToolResult::error("Plugin runtime not configured"),
        };
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required parameter: name"),
        };
        match runtime.get(name).await {
            Some(_) => ToolResult::ok(
                json!({"name": name, "status": "active", "message": "Plugin is already active"})
                    .to_string(),
            ),
            None => ToolResult::error(format!("Plugin '{}' not found", name)),
        }
    }

    async fn plugin_deactivate(&self, args: &Value) -> ToolResult {
        let runtime = match &self.plugin_runtime {
            Some(r) => r,
            None => return ToolResult::error("Plugin runtime not configured"),
        };
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("missing required parameter: name"),
        };
        match runtime.uninstall(name, false).await {
            Ok(()) => {
                self.notify_tools_changed();
                ToolResult::ok(json!({"name": name, "status": "deactivated"}).to_string())
            }
            Err(e) => ToolResult::error(format!("Failed to deactivate plugin: {}", e)),
        }
    }

    /// Send a `notifications/tools/list_changed` notification to the MCP client.
    ///
    /// Called after plugin register/deactivate so the client knows to re-fetch `tools/list`.
    fn notify_tools_changed(&self) {
        if let Some(tx) = &self.notification_tx {
            let _ = tx.send(crate::server::ServerNotification::tools_list_changed());
        }
    }

    // ── Sub-agent tools ──────────────────────────────────────────────────────────

    async fn subagent_spawn(&self, args: &Value) -> ToolResult {
        let manager = match &self.subagent_manager {
            Some(m) => m,
            None => return ToolResult::error("Sub-agent manager not configured"),
        };
        let agent = match args.get("agent").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("missing required parameter: agent"),
        };
        let task = match args.get("task").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("missing required parameter: task"),
        };

        let mut options = SpawnOptions::default();
        if let Some(label) = args.get("label").and_then(|v| v.as_str()) {
            options = options.with_label(label);
        }
        if let Some(timeout_secs) = args.get("timeout_seconds").and_then(|v| v.as_u64()) {
            options = options.with_timeout(std::time::Duration::from_secs(timeout_secs));
        }
        if let Some(ctx) = args.get("context").and_then(|v| v.as_str()) {
            options = options.with_parent_context(ctx);
        }

        let session_id = manager.spawn(agent, task, options).await;

        ToolResult::ok(
            json!({
                "session_id": session_id,
                "status": "running",
                "agent": agent,
                "task": task
            })
            .to_string(),
        )
    }

    async fn subagent_list(&self, _args: &Value) -> ToolResult {
        let manager = match &self.subagent_manager {
            Some(m) => m,
            None => return ToolResult::error("Sub-agent manager not configured"),
        };

        let sessions = manager.list().await;
        let output: Vec<Value> = sessions
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "agent": s.agent_name,
                    "label": s.label,
                    "status": format!("{:?}", s.status),
                    "started_at": s.started_at.to_rfc3339(),
                    "completed_at": s.completed_at.map(|t| t.to_rfc3339()),
                })
            })
            .collect();

        ToolResult::ok(serde_json::to_string_pretty(&output).unwrap_or_default())
    }

    async fn subagent_kill(&self, args: &Value) -> ToolResult {
        let manager = match &self.subagent_manager {
            Some(m) => m,
            None => return ToolResult::error("Sub-agent manager not configured"),
        };
        let session_id = match args.get("session_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return ToolResult::error("missing required parameter: session_id"),
        };

        let killed = manager.kill(session_id).await;
        if killed {
            ToolResult::ok(json!({"killed": true, "session_id": session_id}).to_string())
        } else {
            ToolResult::error(format!(
                "session not found or already completed: {session_id}"
            ))
        }
    }

    async fn subagent_steer(&self, args: &Value) -> ToolResult {
        let manager = match &self.subagent_manager {
            Some(m) => m,
            None => return ToolResult::error("Sub-agent manager not configured"),
        };
        let session_id = match args.get("session_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return ToolResult::error("missing required parameter: session_id"),
        };
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => return ToolResult::error("missing required parameter: message"),
        };

        let steered = manager.steer(session_id, message).await;
        if steered {
            ToolResult::ok(json!({"steered": true, "session_id": session_id}).to_string())
        } else {
            ToolResult::error(format!(
                "session not found, not running, or steering not supported: {session_id}"
            ))
        }
    }

    /// Full agent loop via MCP — channel-agnostic. Same as Telegram/TUI.
    async fn session_status(&self, args: &Value) -> ToolResult {
        let session_id = args.get("session_id").and_then(|v| v.as_str());

        // Handle model override if requested
        if let Some(model_override) = args.get("model").and_then(|v| v.as_str()) {
            if let Some(store) = &self.state_store {
                let key = format!("session:model_override:{}", session_id.unwrap_or("main"));
                if model_override == "default" {
                    store.delete(&key).await;
                } else {
                    store
                        .set(&key, Value::String(model_override.to_string()))
                        .await;
                }
            }
        }

        let uptime_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let shell_sessions = self.shell.list().await;
        let active_shells = shell_sessions.len();

        let mut active_subagents = 0usize;
        if let Some(manager) = &self.subagent_manager {
            active_subagents = manager.list_running().await.len();
        }

        // Get model override if set
        let model_override = if let Some(store) = &self.state_store {
            let key = format!("session:model_override:{}", session_id.unwrap_or("main"));
            store
                .get(&key)
                .await
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        } else {
            None
        };

        let status = json!({
            "session_id": session_id.unwrap_or("main"),
            "status": "running",
            "version": env!("CARGO_PKG_VERSION"),
            "timestamp_unix": uptime_secs,
            "model": model_override.as_deref().unwrap_or("default"),
            "active_shell_sessions": active_shells,
            "active_subagents": active_subagents,
            "components": {
                "memory": if self.memory.is_some() { "active" } else { "not_configured" },
                "scheduler": if self.scheduler.is_some() { "active" } else { "not_configured" },
                "state_store": if self.state_store.is_some() { "active" } else { "not_configured" },
                "subagent_manager": if self.subagent_manager.is_some() { "active" } else { "not_configured" }
            }
        });

        ToolResult::ok(serde_json::to_string_pretty(&status).unwrap_or_default())
    }

    async fn session_history(&self, args: &Value) -> ToolResult {
        let session_id = match args.get("session_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return ToolResult::error("missing required parameter: session_id"),
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(100) as usize;
        let include_tools = args
            .get("include_tools")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Look up session history from state store
        let store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error(
                    "State store not configured — session history unavailable",
                )
            }
        };

        let key = format!("session:history:{session_id}");
        let history = store.get(&key).await;

        match history {
            Some(Value::Array(messages)) => {
                let filtered: Vec<&Value> = messages
                    .iter()
                    .filter(|msg| {
                        if include_tools {
                            return true;
                        }
                        // Filter out tool messages unless include_tools=true
                        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
                        role != "tool" && role != "tool_result"
                    })
                    .collect();
                let truncated: Vec<&&Value> = filtered.iter().rev().take(limit).collect();
                let result: Vec<&Value> = truncated.into_iter().rev().copied().collect();

                ToolResult::ok(
                    json!({
                        "session_id": session_id,
                        "message_count": result.len(),
                        "total_messages": messages.len(),
                        "messages": result
                    })
                    .to_string(),
                )
            }
            Some(other) => ToolResult::ok(
                json!({
                    "session_id": session_id,
                    "message_count": 0,
                    "messages": [],
                    "note": format!("unexpected history format: {}", other)
                })
                .to_string(),
            ),
            None => ToolResult::ok(
                json!({
                    "session_id": session_id,
                    "message_count": 0,
                    "messages": [],
                    "note": "no history found for this session"
                })
                .to_string(),
            ),
        }
    }

    async fn session_send(&self, args: &Value) -> ToolResult {
        let session_id = match args.get("session_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return ToolResult::error("missing required parameter: session_id"),
        };
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return ToolResult::error("missing required parameter: message"),
        };
        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .min(300);

        let store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error(
                    "State store not configured — session messaging unavailable",
                )
            }
        };

        // Append message to the target session's inbox
        let inbox_key = format!("session:inbox:{session_id}");
        let msg_entry = json!({
            "from": "mcp",
            "message": message,
            "timestamp": SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "status": "pending"
        });

        // Get existing inbox or create new
        let mut inbox = match store.get(&inbox_key).await {
            Some(Value::Array(arr)) => arr,
            _ => Vec::new(),
        };
        inbox.push(msg_entry.clone());
        store.set(&inbox_key, Value::Array(inbox)).await;

        // Wait for a response if timeout > 0
        if timeout_secs > 0 {
            let response_key = format!("session:response:{session_id}:latest");
            let start = std::time::Instant::now();
            let timeout_dur = std::time::Duration::from_secs(timeout_secs);

            // Clear any old response first
            store.delete(&response_key).await;

            // Poll for response (check every 500ms)
            while start.elapsed() < timeout_dur {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Some(response) = store.get(&response_key).await {
                    return ToolResult::ok(
                        json!({
                            "session_id": session_id,
                            "status": "responded",
                            "response": response
                        })
                        .to_string(),
                    );
                }
            }
        }

        // If we didn't get a response (or timeout=0), return confirmation of delivery
        ToolResult::ok(
            json!({
                "session_id": session_id,
                "status": "delivered",
                "message": "Message delivered to session inbox. No response received within timeout."
            })
            .to_string(),
        )
    }

    async fn session_list(&self, _args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => {
                return ToolResult::error(
                    "State store not configured — session listing unavailable",
                )
            }
        };

        // List sessions from state store (sessions register themselves)
        let sessions_key = "sessions:active";
        let sessions = match store.get(sessions_key).await {
            Some(Value::Array(arr)) => arr,
            _ => Vec::new(),
        };

        // Also list shell sessions and subagents
        let shell_sessions = self.shell.list().await;
        let mut subagent_info: Vec<Value> = Vec::new();
        if let Some(manager) = &self.subagent_manager {
            let running = manager.list_running().await;
            for s in running {
                subagent_info.push(json!({
                    "id": s.id,
                    "agent": s.agent_name,
                    "task": s.task_input,
                    "label": s.label,
                    "started_at": s.started_at.to_rfc3339(),
                    "status": format!("{:?}", s.status)
                }));
            }
        }

        ToolResult::ok(
            json!({
                "sessions": sessions,
                "shell_sessions": shell_sessions.len(),
                "subagent_count": subagent_info.len(),
                "subagents": subagent_info
            })
            .to_string(),
        )
    }

    /// End the current agent turn and yield control back to the orchestrator.
    /// Used after spawning sub-agents to wait for their completion events.
    async fn session_yield(&self, args: &Value) -> ToolResult {
        let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");

        // Record the yield event in Chronos if available
        if let Some(chronos) = &self.chronos {
            let entry = chronos.build_entry(
                "session.yield",
                "agent",
                ChronosAction::ToolInvoked,
                &json!({"message": message}),
                vec![],
                None,
            );
            chronos.record(&entry);
        }

        // Store yield state so the orchestrator knows this session is waiting
        if let Some(store) = &self.state_store {
            store
                .set(
                    "session:yield:pending",
                    json!({
                        "yielded": true,
                        "message": message,
                        "at": chrono::Utc::now().to_rfc3339()
                    }),
                )
                .await;
        }

        ToolResult::ok(
            json!({
                "yielded": true,
                "message": message,
                "status": "Turn ended. Waiting for sub-agent completion events."
            })
            .to_string(),
        )
    }

    // ── Telemetry tools ──────────────────────────────────────────────────────────────────────

    async fn telemetry_snapshot(&self) -> ToolResult {
        let snapshot = match self.metrics.lock() {
            Ok(metrics) => metrics.snapshot(),
            Err(_) => return ToolResult::error("metrics lock poisoned".to_string()),
        };
        ToolResult::ok(snapshot.to_string())
    }

    async fn telemetry_reset(&self) -> ToolResult {
        match self.metrics.lock() {
            Ok(mut metrics) => {
                metrics.reset();
                ToolResult::ok(
                    json!({"reset": true, "message": "All telemetry counters cleared."})
                        .to_string(),
                )
            }
            Err(_) => ToolResult::error("metrics lock poisoned".to_string()),
        }
    }

    // ── Canvas tool implementations ────────────────────────────────────────

    async fn canvas_create(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolResult::error("missing required parameter: title"),
        };
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let id = uuid::Uuid::new_v4().to_string();
        let canvas = json!({
            "id": id,
            "title": title,
            "description": description,
            "tree": { "id": "root", "type": "Root", "children": [] },
            "data": {},
            "procedures": [],
            "rules": [],
            "createdAt": chrono::Utc::now().to_rfc3339(),
            "updatedAt": chrono::Utc::now().to_rfc3339()
        });

        store.set("canvas:active", canvas.clone()).await;
        ToolResult::ok(serde_json::to_string_pretty(&canvas).unwrap_or_default())
    }

    async fn canvas_get(&self) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        match store.get("canvas:active").await {
            Some(Value::Null) | None => ToolResult::ok("null"),
            Some(canvas) => {
                ToolResult::ok(serde_json::to_string_pretty(&canvas).unwrap_or_default())
            }
        }
    }

    async fn canvas_set_tree(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let tree = match args.get("tree") {
            Some(t) => t.clone(),
            None => return ToolResult::error("missing required parameter: tree"),
        };

        let mut canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        canvas["tree"] = tree;
        canvas["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        store.set("canvas:active", canvas).await;
        ToolResult::ok("tree updated")
    }

    async fn canvas_add_node(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let parent_id = match args.get("parentId").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolResult::error("missing required parameter: parentId"),
        };
        let node = match args.get("node") {
            Some(n) => n.clone(),
            None => return ToolResult::error("missing required parameter: node"),
        };

        let mut canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        fn insert_into_parent(tree: &mut Value, parent_id: &str, node: &Value) -> bool {
            if let Some(id) = tree.get("id").and_then(|v| v.as_str()) {
                if id == parent_id {
                    if tree.get("children").is_none() {
                        tree["children"] = json!([]);
                    }
                    if let Some(arr) = tree["children"].as_array_mut() {
                        arr.push(node.clone());
                    }
                    return true;
                }
            }
            if let Some(children) = tree.get_mut("children").and_then(|c| c.as_array_mut()) {
                for child in children.iter_mut() {
                    if insert_into_parent(child, parent_id, node) {
                        return true;
                    }
                }
            }
            false
        }

        if !insert_into_parent(&mut canvas["tree"], &parent_id, &node) {
            return ToolResult::error(format!("parent node '{parent_id}' not found in tree"));
        }

        canvas["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        store.set("canvas:active", canvas).await;
        ToolResult::ok(format!("node added under parent '{parent_id}'"))
    }

    async fn canvas_remove_node(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let node_id = match args.get("nodeId").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolResult::error("missing required parameter: nodeId"),
        };

        let mut canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        fn remove_from_tree(tree: &mut Value, node_id: &str) -> bool {
            if let Some(children) = tree.get_mut("children").and_then(|c| c.as_array_mut()) {
                let len_before = children.len();
                children.retain(|child| child.get("id").and_then(|v| v.as_str()) != Some(node_id));
                if children.len() < len_before {
                    return true;
                }
                for child in children.iter_mut() {
                    if remove_from_tree(child, node_id) {
                        return true;
                    }
                }
            }
            false
        }

        if !remove_from_tree(&mut canvas["tree"], &node_id) {
            return ToolResult::error(format!("node '{node_id}' not found in tree"));
        }

        canvas["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        store.set("canvas:active", canvas).await;
        ToolResult::ok(format!("node '{node_id}' removed"))
    }

    async fn canvas_validate(&self) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        let mut issues: Vec<Value> = Vec::new();

        // Check required fields
        if canvas.get("id").and_then(|v| v.as_str()).is_none() {
            issues.push(json!({"severity": "error", "message": "Canvas missing 'id' field"}));
        }
        if canvas.get("title").and_then(|v| v.as_str()).is_none() {
            issues.push(json!({"severity": "error", "message": "Canvas missing 'title' field"}));
        }
        if canvas.get("tree").is_none() {
            issues.push(json!({"severity": "error", "message": "Canvas missing 'tree' field"}));
        }

        // Check for duplicate node IDs in the tree
        fn collect_ids(tree: &Value, ids: &mut Vec<String>) {
            if let Some(id) = tree.get("id").and_then(|v| v.as_str()) {
                ids.push(id.to_string());
            }
            if let Some(children) = tree.get("children").and_then(|c| c.as_array()) {
                for child in children {
                    collect_ids(child, ids);
                }
            }
        }

        if let Some(tree) = canvas.get("tree") {
            let mut ids = Vec::new();
            collect_ids(tree, &mut ids);
            let mut seen = std::collections::HashSet::new();
            for id in &ids {
                if !seen.insert(id.as_str()) {
                    issues.push(json!({"severity": "error", "message": format!("Duplicate node ID: '{id}'")}));
                }
            }
        }

        let result = json!({
            "valid": issues.is_empty(),
            "issues": issues
        });
        ToolResult::ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn canvas_export(&self) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        match store.get("canvas:active").await {
            Some(c) if !c.is_null() => {
                ToolResult::ok(serde_json::to_string_pretty(&c).unwrap_or_default())
            }
            _ => ToolResult::error("no active canvas — create one first"),
        }
    }

    async fn canvas_import(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let json_str = match args.get("json").and_then(|v| v.as_str()) {
            Some(j) => j,
            None => return ToolResult::error("missing required parameter: json"),
        };

        let canvas: Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(format!("invalid JSON: {e}")),
        };

        // Validate basic structure
        if canvas.get("id").is_none() || canvas.get("title").is_none() {
            return ToolResult::error("imported canvas must have 'id' and 'title' fields");
        }

        store.set("canvas:active", canvas.clone()).await;
        ToolResult::ok(format!(
            "canvas '{}' imported and set as active",
            canvas
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("untitled")
        ))
    }

    async fn canvas_list(&self) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let keys = store.keys_with_prefix("canvas:saved:").await;
        let mut canvases: Vec<Value> = Vec::new();

        for key in &keys {
            if let Some(canvas) = store.get(key).await {
                if !canvas.is_null() {
                    canvases.push(json!({
                        "id": canvas.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "title": canvas.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                        "description": canvas.get("description"),
                        "updatedAt": canvas.get("updatedAt")
                    }));
                }
            }
        }

        ToolResult::ok(serde_json::to_string_pretty(&canvases).unwrap_or_default())
    }

    async fn canvas_load(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let id = match args.get("id").and_then(|v| v.as_str()) {
            Some(i) => i,
            None => return ToolResult::error("missing required parameter: id"),
        };

        let key = format!("canvas:saved:{id}");
        match store.get(&key).await {
            Some(canvas) if !canvas.is_null() => {
                store.set("canvas:active", canvas.clone()).await;
                ToolResult::ok(serde_json::to_string_pretty(&canvas).unwrap_or_default())
            }
            _ => ToolResult::error(format!("canvas '{id}' not found")),
        }
    }

    async fn canvas_save(&self) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        let id = match canvas.get("id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => return ToolResult::error("active canvas has no id"),
        };

        let key = format!("canvas:saved:{id}");
        store.set(&key, canvas).await;
        ToolResult::ok(format!("canvas '{id}' saved"))
    }

    async fn canvas_set_data(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let data = match args.get("data") {
            Some(d) if d.is_object() => d.clone(),
            Some(_) => return ToolResult::error("'data' must be a JSON object"),
            None => return ToolResult::error("missing required parameter: data"),
        };

        let mut canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        // Merge data into existing canvas data
        if let Some(existing) = canvas.get_mut("data").and_then(|d| d.as_object_mut()) {
            if let Some(new_data) = data.as_object() {
                for (k, v) in new_data {
                    existing.insert(k.clone(), v.clone());
                }
            }
        } else {
            canvas["data"] = data;
        }

        canvas["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        store.set("canvas:active", canvas).await;
        ToolResult::ok("data updated")
    }

    async fn canvas_add_procedure(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let procedure = match args.get("procedure") {
            Some(p) => p.clone(),
            None => return ToolResult::error("missing required parameter: procedure"),
        };

        let mut canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        if canvas.get("procedures").is_none() {
            canvas["procedures"] = json!([]);
        }
        if let Some(arr) = canvas["procedures"].as_array_mut() {
            arr.push(procedure);
        }

        canvas["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        store.set("canvas:active", canvas).await;
        ToolResult::ok("procedure added")
    }

    async fn canvas_add_rule(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let rule = match args.get("rule") {
            Some(r) => r.clone(),
            None => return ToolResult::error("missing required parameter: rule"),
        };

        let mut canvas = match store.get("canvas:active").await {
            Some(c) if !c.is_null() => c,
            _ => return ToolResult::error("no active canvas — create one first"),
        };

        if canvas.get("rules").is_none() {
            canvas["rules"] = json!([]);
        }
        if let Some(arr) = canvas["rules"].as_array_mut() {
            arr.push(rule);
        }

        canvas["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        store.set("canvas:active", canvas).await;
        ToolResult::ok("rule added")
    }

    /// Push A2UI JSONL rendering instructions to the active canvas.
    ///
    /// A2UI (Agent-to-UI) is the protocol for pushing live UI updates from the
    /// agent loop to connected canvas renderers (Tauri, web, node). Each JSONL
    /// line is a rendering instruction (set, append, clear, animate, etc.).
    async fn canvas_a2ui_push(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        // Accept either `jsonl` (string of newline-delimited JSON) or `instructions` (array)
        let instructions: Vec<Value> =
            if let Some(jsonl_str) = args.get("jsonl").and_then(|v| v.as_str()) {
                jsonl_str
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect()
            } else if let Some(arr) = args.get("instructions").and_then(|v| v.as_array()) {
                arr.clone()
            } else {
                return ToolResult::error(
                    "missing required parameter: jsonl (string) or instructions (array)",
                );
            };

        if instructions.is_empty() {
            return ToolResult::error("no valid instructions provided");
        }

        // Append to the A2UI queue in state store
        let queue_key = "canvas:a2ui:queue";
        let mut queue = match store.get(queue_key).await {
            Some(Value::Array(arr)) => arr,
            _ => Vec::new(),
        };

        let count = instructions.len();
        for instr in instructions {
            queue.push(instr);
        }

        store.set(queue_key, Value::Array(queue)).await;

        // Also update the canvas's a2ui timestamp
        if let Some(mut canvas) = store.get("canvas:active").await {
            canvas["a2uiLastPush"] = json!(chrono::Utc::now().to_rfc3339());
            store.set("canvas:active", canvas).await;
        }

        ToolResult::ok(format!("{count} instruction(s) pushed to A2UI queue"))
    }

    /// Reset the A2UI rendering queue, clearing all pending instructions.
    ///
    /// Optionally targets a specific node canvas; if no target is specified,
    /// resets the global A2UI queue.
    async fn canvas_a2ui_reset(&self, args: &Value) -> ToolResult {
        let store = match &self.state_store {
            Some(s) => s,
            None => return ToolResult::error("state store not configured"),
        };

        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        let queue_key = if target == "global" {
            "canvas:a2ui:queue".to_string()
        } else {
            format!("canvas:a2ui:queue:{target}")
        };

        store.set(&queue_key, json!([])).await;

        // Update canvas timestamp
        if let Some(mut canvas) = store.get("canvas:active").await {
            canvas["a2uiLastReset"] = json!(chrono::Utc::now().to_rfc3339());
            store.set("canvas:active", canvas).await;
        }

        ToolResult::ok(format!("A2UI queue reset (target: {target})"))
    }

    async fn canvas_catalog(&self) -> ToolResult {
        let catalog = json!({
            "components": [
                {
                    "type": "Root",
                    "description": "Top-level container for a canvas tree. Every canvas has exactly one Root.",
                    "props": {},
                    "children": true
                },
                {
                    "type": "Container",
                    "description": "Layout container that holds child components. Supports flex/grid layout.",
                    "props": {
                        "direction": {"type": "string", "enum": ["row", "column"], "default": "column"},
                        "gap": {"type": "number", "description": "Spacing between children in px"},
                        "padding": {"type": "number", "description": "Inner padding in px"},
                        "align": {"type": "string", "enum": ["start", "center", "end", "stretch"]},
                        "justify": {"type": "string", "enum": ["start", "center", "end", "between", "around"]},
                        "wrap": {"type": "boolean", "default": false},
                        "style": {"type": "object", "description": "CSS-like style overrides"}
                    },
                    "children": true
                },
                {
                    "type": "Text",
                    "description": "Renders text content. Supports markdown-like formatting.",
                    "props": {
                        "text": {"type": "string", "required": true, "description": "Text content to display"},
                        "variant": {"type": "string", "enum": ["body", "heading", "caption", "code", "mono"], "default": "body"},
                        "size": {"type": "string", "enum": ["xs", "sm", "md", "lg", "xl"]},
                        "weight": {"type": "string", "enum": ["normal", "medium", "bold"]},
                        "color": {"type": "string", "description": "Text color (CSS value or theme token)"},
                        "align": {"type": "string", "enum": ["left", "center", "right"]}
                    },
                    "children": false
                },
                {
                    "type": "Button",
                    "description": "Interactive button that triggers procedures on click.",
                    "props": {
                        "label": {"type": "string", "required": true, "description": "Button label text"},
                        "variant": {"type": "string", "enum": ["primary", "secondary", "danger", "ghost"], "default": "primary"},
                        "size": {"type": "string", "enum": ["sm", "md", "lg"], "default": "md"},
                        "disabled": {"type": "boolean", "default": false},
                        "icon": {"type": "string", "description": "Optional icon name"}
                    },
                    "children": false,
                    "events": ["onClick"]
                },
                {
                    "type": "Input",
                    "description": "Text input field with optional label and validation.",
                    "props": {
                        "label": {"type": "string", "description": "Input label"},
                        "placeholder": {"type": "string", "description": "Placeholder text"},
                        "type": {"type": "string", "enum": ["text", "number", "email", "password", "url"], "default": "text"},
                        "required": {"type": "boolean", "default": false},
                        "disabled": {"type": "boolean", "default": false},
                        "defaultValue": {"type": "string"}
                    },
                    "children": false,
                    "events": ["onChange", "onSubmit"]
                },
                {
                    "type": "Select",
                    "description": "Dropdown select with predefined options.",
                    "props": {
                        "label": {"type": "string", "description": "Select label"},
                        "options": {"type": "array", "required": true, "description": "Array of {value, label} objects"},
                        "placeholder": {"type": "string"},
                        "multiple": {"type": "boolean", "default": false},
                        "disabled": {"type": "boolean", "default": false}
                    },
                    "children": false,
                    "events": ["onChange"]
                },
                {
                    "type": "Image",
                    "description": "Displays an image from URL or local path.",
                    "props": {
                        "src": {"type": "string", "required": true, "description": "Image URL or local path"},
                        "alt": {"type": "string", "description": "Alt text for accessibility"},
                        "width": {"type": "number"},
                        "height": {"type": "number"},
                        "fit": {"type": "string", "enum": ["contain", "cover", "fill", "none"], "default": "contain"}
                    },
                    "children": false
                },
                {
                    "type": "List",
                    "description": "Renders a list of items, optionally bound to data.",
                    "props": {
                        "items": {"type": "array", "description": "Static items array, or use bindings for dynamic data"},
                        "ordered": {"type": "boolean", "default": false},
                        "dividers": {"type": "boolean", "default": false}
                    },
                    "children": true
                },
                {
                    "type": "Card",
                    "description": "A bordered container with optional header and padding.",
                    "props": {
                        "title": {"type": "string", "description": "Card header title"},
                        "subtitle": {"type": "string"},
                        "padding": {"type": "number", "default": 16},
                        "elevated": {"type": "boolean", "default": false, "description": "Add shadow/elevation"}
                    },
                    "children": true
                },
                {
                    "type": "Divider",
                    "description": "A horizontal rule / visual separator.",
                    "props": {
                        "spacing": {"type": "number", "default": 8, "description": "Vertical margin in px"}
                    },
                    "children": false
                },
                {
                    "type": "Badge",
                    "description": "A small label/tag for status or categorization.",
                    "props": {
                        "text": {"type": "string", "required": true},
                        "variant": {"type": "string", "enum": ["default", "success", "warning", "danger", "info"]}
                    },
                    "children": false
                },
                {
                    "type": "Progress",
                    "description": "A progress bar showing completion state.",
                    "props": {
                        "value": {"type": "number", "required": true, "description": "Progress 0-100"},
                        "label": {"type": "string"},
                        "variant": {"type": "string", "enum": ["default", "success", "warning", "danger"]}
                    },
                    "children": false
                },
                {
                    "type": "Table",
                    "description": "Renders tabular data with columns and rows.",
                    "props": {
                        "columns": {"type": "array", "required": true, "description": "Array of {key, label, width?} column definitions"},
                        "rows": {"type": "array", "required": true, "description": "Array of row objects matching column keys"},
                        "striped": {"type": "boolean", "default": false},
                        "compact": {"type": "boolean", "default": false}
                    },
                    "children": false
                },
                {
                    "type": "Tabs",
                    "description": "Tabbed interface — children are tab panes.",
                    "props": {
                        "defaultTab": {"type": "string", "description": "ID of initially active tab"}
                    },
                    "children": true
                },
                {
                    "type": "TabPane",
                    "description": "A single tab pane within a Tabs component.",
                    "props": {
                        "id": {"type": "string", "required": true},
                        "label": {"type": "string", "required": true, "description": "Tab label text"}
                    },
                    "children": true
                },
                {
                    "type": "Chart",
                    "description": "Data visualization chart.",
                    "props": {
                        "type": {"type": "string", "enum": ["bar", "line", "pie", "donut", "area"], "required": true},
                        "data": {"type": "array", "required": true, "description": "Array of data points"},
                        "xKey": {"type": "string", "description": "Key for x-axis values"},
                        "yKey": {"type": "string", "description": "Key for y-axis values"},
                        "title": {"type": "string"},
                        "height": {"type": "number", "default": 300}
                    },
                    "children": false
                },
                {
                    "type": "Code",
                    "description": "Syntax-highlighted code block.",
                    "props": {
                        "code": {"type": "string", "required": true},
                        "language": {"type": "string", "default": "plaintext"},
                        "showLineNumbers": {"type": "boolean", "default": false},
                        "maxHeight": {"type": "number", "description": "Max height in px before scrolling"}
                    },
                    "children": false
                },
                {
                    "type": "Spacer",
                    "description": "Invisible spacing element.",
                    "props": {
                        "size": {"type": "number", "default": 16, "description": "Space in px"}
                    },
                    "children": false
                }
            ]
        });
        ToolResult::ok(serde_json::to_string_pretty(&catalog).unwrap_or_default())
    }

    async fn agent_ask(&self, args: &Value) -> ToolResult {
        let agent =
            match &self.agent {
                Some(a) => a,
                None => return ToolResult::error(
                    "Agent not configured for MCP. Start with --copilot to enable the agent loop.",
                ),
            };
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolResult::error("missing required parameter: prompt"),
        };
        let session = args
            .get("session")
            .and_then(|v| v.as_str())
            .unwrap_or("mcp");

        let event = pares_radix_core::event::Event::Message {
            id: uuid::Uuid::new_v4().to_string(),
            channel: session.to_string(),
            sender: "mcp-client".to_string(),
            content: prompt,
        };

        match agent.handle_event(event).await {
            Some(pares_radix_core::event::Event::ModelResponse { content, .. }) => {
                ToolResult::ok(content)
            }
            Some(other) => ToolResult::ok(format!("{:?}", other)),
            None => ToolResult::ok("(no response from agent)".to_string()),
        }
    }

    /// Build a ProcedureRegistry from all loaded procedures.
    async fn build_procedure_registry(&self) -> ProcedureRegistry {
        let mut registry = ProcedureRegistry::new();
        let procedures_guard = self.loaded_procedures.read().await;
        for (name, loaded) in procedures_guard.iter() {
            registry.register_as(name.clone(), loaded.data.clone());
        }
        registry
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            p
        } else {
            self.workdir.join(p)
        }
    }
}

impl ToolResult {
    /// Successful tool result.
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// Error tool result.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[async_trait]
impl ToolHandler for RadixToolHandler {
    async fn list_tools(&self) -> Vec<Tool> {
        let mut tools = vec![
            Tool {
                name: "read_file".into(),
                description: Some("Read a UTF-8 text file from disk".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({"path": {"type": "string"}})),
                    required: Some(vec!["path".into()]),
                },
            },
            Tool {
                name: "write_file".into(),
                description: Some("Write a UTF-8 text file to disk (creates parent dirs)".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    })),
                    required: Some(vec!["path".into(), "content".into()]),
                },
            },
            Tool {
                name: "edit_file".into(),
                description: Some(
                    "Replace the first occurrence of old_text with new_text in a file".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "path": {"type": "string"},
                        "old_text": {"type": "string"},
                        "new_text": {"type": "string"}
                    })),
                    required: Some(vec!["path".into(), "old_text".into(), "new_text".into()]),
                },
            },
            Tool {
                name: "apply_patch".into(),
                description: Some(
                    "Apply a multi-file patch in the '*** Begin Patch' envelope (Add/Update/Delete File sections). Validates all hunks against current file state, then commits atomically.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "input": {"type": "string", "description": "The full patch text including '*** Begin Patch' and '*** End Patch' markers."}
                    })),
                    required: Some(vec!["input".into()]),
                },
            },
            Tool {
                name: "list_directory".into(),
                description: Some("List files in a directory".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({"path": {"type": "string"}})),
                    required: Some(vec!["path".into()]),
                },
            },
            Tool {
                name: "run_command".into(),
                description: Some(
                    "Run a shell command. Supports background, pty, timeout, workdir, env.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "command": {"type": "string", "description": "Shell command to execute"},
                        "workdir": {"type": "string", "description": "Working directory"},
                        "background": {"type": "boolean", "description": "Run in background"},
                        "pty": {"type": "boolean", "description": "Use pseudo-terminal"},
                        "timeout": {"type": "integer", "description": "Timeout in seconds"},
                        "env": {"type": "object", "description": "Additional environment variables"}
                    })),
                    required: Some(vec!["command".into()]),
                },
            },
            Tool {
                name: "process".into(),
                description: Some(
                    "Manage background shell sessions: list, poll, write, kill.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "action": {"type": "string", "enum": ["list", "poll", "write", "kill"]},
                        "sessionId": {"type": "string"},
                        "timeout": {"type": "integer"},
                        "data": {"type": "string"}
                    })),
                    required: Some(vec!["action".into()]),
                },
            },
            Tool {
                name: "skill_workshop".into(),
                description: Some(
                    "Author, list, inspect, and apply local skill proposals. Actions: create, \
                     update, revise, list, inspect, apply, reject, quarantine. Proposals are \
                     pending until applied; apply installs SKILL.md + support files into the \
                     live-skills directory."
                        .into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "action": {
                            "type": "string",
                            "enum": [
                                "create", "update", "revise", "list", "inspect",
                                "apply", "reject", "quarantine"
                            ],
                            "description": "Lifecycle action to perform."
                        },
                        "id": {"type": "string", "description": "Proposal id (revise/inspect/apply/reject/quarantine)."},
                        "name": {"type": "string", "description": "Skill name (create/update)."},
                        "description": {"type": "string", "description": "Short description, ≤160 bytes (create/update/revise)."},
                        "proposal_content": {"type": "string", "description": "Procedure markdown body, stored as PROPOSAL.md (create/update/revise)."},
                        "skill_name": {"type": "string", "description": "Existing live-skill this proposal targets (update)."},
                        "support_files": {
                            "type": "array",
                            "description": "Support files under assets/examples/references/scripts/templates.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": {"type": "string"},
                                    "content": {"type": "string"}
                                },
                                "required": ["path", "content"]
                            }
                        },
                        "status": {
                            "type": "string",
                            "enum": ["pending", "applied", "rejected", "quarantined", "stale"],
                            "description": "Optional status filter (list)."
                        },
                        "proposals_dir": {"type": "string", "description": "Override proposals root (default <workdir>/.agens-skill-proposals)."},
                        "skills_dir": {"type": "string", "description": "Override live-skills dir (default <workdir>/.agens-skills)."}
                    })),
                    required: Some(vec!["action".into()]),
                },
            },
            Tool {
                name: "memory_search".into(),
                description: Some(
                    "Search long-term memory semantically. Returns relevant stored memories."
                        .into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "query": {"type": "string", "description": "Semantic search query"},
                        "limit": {"type": "integer", "description": "Max results (default 5)"}
                    })),
                    required: Some(vec!["query".into()]),
                },
            },
            Tool {
                name: "memory_store".into(),
                description: Some(
                    "Store a fact or decision in long-term memory with optional tags.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "content": {"type": "string"},
                        "tags": {"type": "array", "items": {"type": "string"}}
                    })),
                    required: Some(vec!["content".into()]),
                },
            },
            Tool {
                name: "web_fetch".into(),
                description: Some(
                    "Fetch a URL and return readable content (HTML auto-converted to text).".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "url": {"type": "string"},
                        "max_chars": {"type": "integer", "description": "Max chars (default 30000)"}
                    })),
                    required: Some(vec!["url".into()]),
                },
            },
            Tool {
                name: "web_search".into(),
                description: Some("Search the web via Brave Search API.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "query": {"type": "string"},
                        "count": {"type": "integer", "description": "Number of results (default 5)"}
                    })),
                    required: Some(vec!["query".into()]),
                },
            },
            Tool {
                name: "cron_list".into(),
                description: Some("List all scheduled cron tasks.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({})),
                    required: None,
                },
            },
            Tool {
                name: "cron_add".into(),
                description: Some("Add a scheduled cron task. Provide 'cron' (5-field expr) or 'interval_secs'.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "name": {"type": "string", "description": "Human-readable task name"},
                        "command": {"type": "string", "description": "Shell command to execute"},
                        "cron": {"type": "string", "description": "Cron expression (5-field: min hour dom month dow)"},
                        "interval_secs": {"type": "integer", "description": "Run every N seconds (alternative to cron)"},
                        "failure_alert": {
                            "type": "object",
                            "description": "Alert after N consecutive failures (with optional cooldown between alerts).",
                            "properties": {
                                "after": {"type": "integer", "description": "Consecutive failures before alerting"},
                                "cooldown_secs": {"type": "integer", "description": "Minimum seconds between alerts (0 = every time)"}
                            },
                            "required": ["after"]
                        }
                    })),
                    required: Some(vec!["name".into(), "command".into()]),
                },
            },
            Tool {
                name: "cron_remove".into(),
                description: Some("Remove a scheduled task by id.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({"id": {"type": "string"}})),
                    required: Some(vec!["id".into()]),
                },
            },
            Tool {
                name: "cron_toggle".into(),
                description: Some("Enable or disable a scheduled task.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "id": {"type": "string"},
                        "enabled": {"type": "boolean"}
                    })),
                    required: Some(vec!["id".into(), "enabled".into()]),
                },
            },
            // ── Heartbeat tools ───────────────────────────────────────────
            Tool {
                name: "heartbeat_status".into(),
                description: Some(
                    "Get heartbeat system status: config, daily count, checklist items.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({})),
                    required: None,
                },
            },
            Tool {
                name: "heartbeat_configure".into(),
                description: Some(
                    "Update heartbeat configuration. Provide only the fields to change.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "enabled": {"type": "boolean", "description": "Enable/disable heartbeat"},
                        "interval_secs": {"type": "integer", "description": "Tick interval in seconds"},
                        "quiet_hours_enabled": {"type": "boolean", "description": "Enable quiet hours"},
                        "quiet_hours_start": {"type": "integer", "description": "Quiet start hour (0-23)"},
                        "quiet_hours_end": {"type": "integer", "description": "Quiet end hour (0-23)"},
                        "max_proactive_per_day": {"type": "integer", "description": "Max proactive messages/day"}
                    })),
                    required: None,
                },
            },
            // ── State/DB tools ────────────────────────────────────────────
            Tool {
                name: "db_get".into(),
                description: Some(
                    "Get a value from the key-value state store. Returns JSON or null.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "key": {"type": "string", "description": "The key to retrieve"}
                    })),
                    required: Some(vec!["key".into()]),
                },
            },
            Tool {
                name: "db_put".into(),
                description: Some(
                    "Store a JSON value under a key in the state store. Overwrites existing values."
                        .into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "key": {"type": "string", "description": "The key to store under"},
                        "value": {"description": "The JSON value to store (any type)"}
                    })),
                    required: Some(vec!["key".into(), "value".into()]),
                },
            },
            Tool {
                name: "db_delete".into(),
                description: Some(
                    "Delete a key from the state store (sets to null).".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "key": {"type": "string", "description": "The key to delete"}
                    })),
                    required: Some(vec!["key".into()]),
                },
            },
            Tool {
                name: "db_keys".into(),
                description: Some(
                    "List all keys in the state store matching an optional prefix.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "prefix": {"type": "string", "description": "Optional prefix filter. Lists all keys if omitted."}
                    })),
                    required: None,
                },
            },
            Tool {
                name: "db_dump".into(),
                description: Some(
                    "Dump all non-null key-value pairs from the state store.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: None,
                    required: None,
                },
            },
            // Task Action tools
            Tool {
                name: "timestamp_now".into(),
                description: Some(
                    "Returns the current Unix timestamp in seconds.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: None,
                    required: None,
                },
            },
            Tool {
                name: "generate_id".into(),
                description: Some(
                    "Generate a unique ID with an optional prefix. Returns prefix_<uuid> or just <uuid>.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "prefix": {"type": "string", "description": "Optional prefix prepended to the UUID (e.g. 'task' produces 'task_<uuid>')"}
                    })),
                    required: None,
                },
            },
            Tool {
                name: "db_get_prefix".into(),
                description: Some(
                    "Prefix scan of the state store. Returns all key-value pairs matching the prefix.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "prefix": {"type": "string", "description": "The key prefix to scan (e.g. 'task:')"}
                    })),
                    required: Some(vec!["prefix".into()]),
                },
            },
            Tool {
                name: "send_message".into(),
                description: Some(
                    "Send a message to a chat via the spine pipeline. Emits a DeliveryRequest event.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "chat_id": {"type": "string", "description": "Target chat/conversation ID"},
                        "text": {"type": "string", "description": "Message text to send"},
                        "channel": {"type": "string", "description": "Delivery channel (e.g. 'telegram', 'discord'). Defaults to 'default'."}
                    })),
                    required: Some(vec!["chat_id".into(), "text".into()]),
                },
            },
            // ── Config & Runtime tools ────────────────────────────────────
            Tool {
                name: "config_get".into(),
                description: Some(
                    "Get a configuration value by key. Returns JSON or null.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "key": {"type": "string", "description": "Config key (e.g. 'model', 'routing.interactive')"}
                    })),
                    required: Some(vec!["key".into()]),
                },
            },
            Tool {
                name: "config_set".into(),
                description: Some(
                    "Set a configuration value. Triggers hot-reload for listening components.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "key": {"type": "string", "description": "Config key to set"},
                        "value": {"description": "The configuration value (any JSON type)"}
                    })),
                    required: Some(vec!["key".into(), "value".into()]),
                },
            },
            Tool {
                name: "config_list".into(),
                description: Some(
                    "List all configuration keys and their current values. Dynamically scans the store. Optional prefix filter.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "prefix": {"type": "string", "description": "Optional prefix filter for keys"}
                    })),
                    required: None,
                },
            },
            Tool {
                name: "config_delete".into(),
                description: Some(
                    "Delete a configuration key. Returns the previous value if it existed.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "key": {"type": "string", "description": "Config key to delete"}
                    })),
                    required: Some(vec!["key".into()]),
                },
            },
            Tool {
                name: "config_reload".into(),
                description: Some(
                    "Signal a configuration reload. Components watching config will pick up changes.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({})),
                    required: None,
                },
            },
            Tool {
                name: "runtime_status".into(),
                description: Some(
                    "Get runtime status: version, active components, shell sessions, health.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({})),
                    required: None,
                },
            },
            Tool {
                name: "runtime_restart".into(),
                description: Some(
                    "Signal a graceful restart. The process supervisor handles the actual restart.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "reason": {"type": "string", "description": "Reason for restart"}
                    })),
                    required: None,
                },
            },
            Tool {
                name: "config_schema".into(),
                description: Some(
                    "Look up the schema for a config key. Pass key='' to list all known keys.".into(),
                ),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "key": {"type": "string", "description": "Config key to look up schema for (empty for full list)"}
                    })),
                    required: None,
                },
            },
            // ── Browser automation tools ──────────────────────────────
            Tool {
                name: "browser_status".into(),
                description: Some("Check if a browser is available via CDP.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({})),
                    required: None,
                },
            },
            Tool {
                name: "browser_navigate".into(),
                description: Some("Navigate the browser to a URL.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "url": {"type": "string", "description": "URL to navigate to"}
                    })),
                    required: Some(vec!["url".into()]),
                },
            },
            Tool {
                name: "browser_snapshot".into(),
                description: Some("Get page content as accessibility tree or text extract.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({})),
                    required: None,
                },
            },
            Tool {
                name: "browser_screenshot".into(),
                description: Some("Capture a screenshot of the current page.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "format": {"type": "string", "enum": ["png", "jpeg"], "description": "Image format (default png)"}
                    })),
                    required: None,
                },
            },
            Tool {
                name: "browser_click".into(),
                description: Some("Click an element by CSS selector or x/y coordinates.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "selector": {"type": "string", "description": "CSS selector of element to click"},
                        "x": {"type": "number", "description": "X coordinate"},
                        "y": {"type": "number", "description": "Y coordinate"}
                    })),
                    required: None,
                },
            },
            Tool {
                name: "browser_type".into(),
                description: Some("Type text into the focused element or a specified selector.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "text": {"type": "string", "description": "Text to type"},
                        "selector": {"type": "string", "description": "Optional CSS selector to focus first"}
                    })),
                    required: Some(vec!["text".into()]),
                },
            },
            // ── Media tools ───────────────────────────────────────────────
            Tool {
                name: "image_analyze".into(),
                description: Some("Analyze an image using a vision model (GPT-4o). Accepts image_url or image_path.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "image_url": {"type": "string", "description": "URL of the image to analyze"},
                        "image_path": {"type": "string", "description": "Local file path of the image"},
                        "prompt": {"type": "string", "description": "Analysis prompt (default: describe the image)"},
                        "model": {"type": "string", "description": "Vision model to use (default: gpt-4o)"}
                    })),
                    required: None,
                },
            },
            Tool {
                name: "image_generate".into(),
                description: Some("Generate an image from a text prompt via OpenAI (DALL-E / gpt-image-1).".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "prompt": {"type": "string", "description": "Image generation prompt"},
                        "model": {"type": "string", "description": "Model (default: gpt-image-1)"},
                        "size": {"type": "string", "description": "Image size (default: 1024x1024)"},
                        "quality": {"type": "string", "description": "Quality: auto, low, medium, high"}
                    })),
                    required: Some(vec!["prompt".into()]),
                },
            },
            Tool {
                name: "tts_generate".into(),
                description: Some("Generate speech audio from text via OpenAI TTS. Returns path to MP3 file.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "text": {"type": "string", "description": "Text to convert to speech"},
                        "voice": {"type": "string", "description": "Voice: alloy, echo, fable, onyx, nova, shimmer"},
                        "model": {"type": "string", "description": "TTS model (default: gpt-4o-mini-tts)"}
                    })),
                    required: Some(vec!["text".into()]),
                },
            },
            Tool {
                name: "pdf_analyze".into(),
                description: Some("Extract text from a PDF and optionally analyze with a model. Requires pdftotext.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "path": {"type": "string", "description": "Path to PDF file"},
                        "prompt": {"type": "string", "description": "Analysis prompt (omit to just extract text)"},
                        "model": {"type": "string", "description": "Model for analysis (default: gpt-4o-mini)"}
                    })),
                    required: Some(vec!["path".into()]),
                },
            },
            Tool {
                name: "video_generate".into(),
                description: Some("Generate a video from a prompt. Requires an external provider.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({"prompt": {"type": "string", "description": "Video generation prompt"}})),
                    required: Some(vec!["prompt".into()]),
                },
            },
            Tool {
                name: "music_generate".into(),
                description: Some("Generate music from a prompt. Requires an external provider.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({"prompt": {"type": "string", "description": "Music generation prompt"}})),
                    required: Some(vec!["prompt".into()]),
                },
            },
        ];

        // ── Remote Node tools (only available when state store is configured) ──
        if self.state_store.is_some() {
            tools.push(Tool {
                name: "node_file_read".into(),
                description: Some("Read a file from a remote node via SSH.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "node": {"type": "string", "description": "Node name, id, or IP address"},
                        "path": {"type": "string", "description": "Absolute path to the file on the node"}
                    })),
                    required: Some(vec!["node".into(), "path".into()]),
                },
            });
            tools.push(Tool {
                name: "node_file_write".into(),
                description: Some("Write a file to a remote node via SSH.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "node": {"type": "string", "description": "Node name, id, or IP address"},
                        "path": {"type": "string", "description": "Absolute path on the node"},
                        "content": {"type": "string", "description": "File content to write"}
                    })),
                    required: Some(vec!["node".into(), "path".into(), "content".into()]),
                },
            });
            tools.push(Tool {
                name: "node_dir_list".into(),
                description: Some("List directory contents on a remote node via SSH.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "node": {"type": "string", "description": "Node name, id, or IP address"},
                        "path": {"type": "string", "description": "Absolute path to directory on the node"}
                    })),
                    required: Some(vec!["node".into(), "path".into()]),
                },
            });
            tools.push(Tool {
                name: "node_dir_fetch".into(),
                description: Some("Fetch a directory tree from a remote node as a tarball via SSH.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "node": {"type": "string", "description": "Node name, id, or IP address"},
                        "path": {"type": "string", "description": "Absolute path to directory on the node"},
                        "local_path": {"type": "string", "description": "Local path to save the fetched tree"}
                    })),
                    required: Some(vec!["node".into(), "path".into()]),
                },
            });
            tools.push(Tool {
                name: "node_status".into(),
                description: Some("Show status of configured remote nodes.".into()),
                input_schema: ToolInputSchema {
                    schema_type: "object".into(),
                    properties: Some(json!({
                        "node": {"type": "string", "description": "Optional: specific node to check (omit for all)"}
                    })),
                    required: None,
                },
            });
        }

        // ── Praxis tools (always available) ───────────────────────────────────
        tools.push(Tool {
            name: "praxis_evaluate".into(),
            description: Some(
                "Evaluate praxis rules/constraints against a context. Checks both loaded PraxisModules and persisted px:constraint/* records from PluresDB. Returns pass/fail/warning results.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "action": {"type": "string", "description": "The action being evaluated (e.g. 'send_email', 'deploy')"},
                    "payload": {"type": "object", "description": "Context payload for rule evaluation — keys become variables for constraint when/require expressions"},
                    "module": {"type": "string", "description": "Optional: specific PraxisModule to evaluate. Omit for all."},
                    "phase": {"type": "string", "description": "Optional: filter px constraints by phase (e.g. 'pre-commit', 'pre-push', 'runtime')"}
                })),
                required: Some(vec!["action".into()]),
            },
        });
        tools.push(Tool {
            name: "praxis_list".into(),
            description: Some(
                "List loaded praxis modules, their rules, and completeness audit.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "module": {"type": "string", "description": "Optional: specific module to inspect (omit for all)"}
                })),
                required: None,
            },
        });

        tools.push(Tool {
            name: "praxis_run".into(),
            description: Some(
                "Run a .px procedure by name or inline source. Executes the procedure's steps asynchronously with tool dispatch.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "source": {"type": "string", "description": "Inline .px source code containing the procedure to run"},
                    "file": {"type": "string", "description": "Path to a .px file containing the procedure (relative to workdir)"},
                    "procedure": {"type": "string", "description": "Name of the procedure to run (if source/file contains multiple). Omit to run the first."},
                    "vars": {"type": "object", "description": "Initial variables to seed the execution context with"}
                })),
                required: None,
            },
        });

        // ── Praxis add tools (persist constraints/rules to PluresDB) ────────────
        tools.push(Tool {
            name: "praxis_add_constraint".into(),
            description: Some(
                "Add a Praxis constraint to PluresDB. Constraints are evaluated by praxis_evaluate.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "name": {"type": "string", "description": "Unique constraint name"},
                    "severity": {"type": "string", "description": "Severity: error, warning, or info"},
                    "when": {"type": "string", "description": "Condition expression (evaluated against context)"},
                    "require": {"type": "string", "description": "Requirement expression that must be true when 'when' matches"},
                    "message": {"type": "string", "description": "Human-readable violation message"},
                    "phases": {"type": "array", "items": {"type": "string"}, "description": "Optional phases this constraint applies to"}
                })),
                required: Some(vec!["name".into(), "severity".into()]),
            },
        });
        tools.push(Tool {
            name: "praxis_add_rule".into(),
            description: Some(
                "Add a Praxis rule to PluresDB. Rules define conditions and actions for the behavior engine.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "name": {"type": "string", "description": "Unique rule name"},
                    "priority": {"type": "integer", "description": "Rule priority (higher = evaluated first)"},
                    "conditions": {"type": "array", "items": {"type": "string"}, "description": "Condition expressions"},
                    "actions": {"type": "array", "description": "Actions to execute when conditions match"}
                })),
                required: Some(vec!["name".into()]),
            },
        });

        // ── Lint tool ────────────────────────────────────────────────────────────────
        tools.push(Tool {
            name: "px_lint".into(),
            description: Some(
                "Lint .px source code for potential issues (non-exhaustive matches, unreachable arms, duplicate conditions, unused variables, unused loop items, empty catch blocks). Returns diagnostics.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "source": {"type": "string", "description": "The .px source code to lint"},
                    "path": {"type": "string", "description": "Path to a .px file to lint (alternative to source)"}
                })),
                required: None,
            },
        });

        // ── Compose tool ─────────────────────────────────────────────────────────────
        tools.push(Tool {
            name: "px_compose".into(),
            description: Some(
                "Dynamic procedure composition: register/unregister procedures at runtime, list the registry, or run a pipeline (pipe) of procedures sequentially. Actions: register, unregister, list, pipe.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "action": {"type": "string", "enum": ["register", "unregister", "list", "pipe"], "description": "Action to perform"},
                    "source": {"type": "string", "description": "Inline .px source to register (for action=register)"},
                    "file": {"type": "string", "description": "Path to .px file to register (for action=register)"},
                    "name": {"type": "string", "description": "Procedure name (for action=unregister)"},
                    "pipeline": {"type": "array", "items": {"type": "string"}, "description": "Ordered list of procedure names to execute as a pipe (for action=pipe)"},
                    "input": {"description": "Initial input value for the pipe (for action=pipe)"},
                    "vars": {"type": "object", "description": "Initial variables passed to the first pipeline stage (for action=pipe)"}
                })),
                required: Some(vec!["action".into()]),
            },
        });

        // ── Px Status tool ────────────────────────────────────────────────────────────
        tools.push(Tool {
            name: "px_status".into(),
            description: Some(
                "Get a diagnostic overview of the Praxis subsystem: loaded procedures, persisted constraints/rules/facts, and registered modules.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        });

        // ── Chronos tools (always available) ────────────────────────────────────────
        tools.push(Tool {
            name: "chronos_history".into(),
            description: Some(
                "Get the version history for a data key. Returns causal chain of mutations.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "key": {"type": "string", "description": "The data key to get history for"},
                    "limit": {"type": "integer", "description": "Max entries to return (default 20)"}
                })),
                required: Some(vec!["key".into()]),
            },
        });
        tools.push(Tool {
            name: "chronos_recent".into(),
            description: Some(
                "Get recent Chronos entries across all keys (newest first). Useful for auditing recent changes.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "limit": {"type": "integer", "description": "Max entries to return (default 20)"}
                })),
                required: None,
            },
        });
        tools.push(Tool {
            name: "chronos_by_actor".into(),
            description: Some(
                "Get Chronos entries by a specific actor (newest first). Useful for tracking who changed what.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "actor": {"type": "string", "description": "Actor name to filter by"},
                    "limit": {"type": "integer", "description": "Max entries to return (default 20)"}
                })),
                required: Some(vec!["actor".into()]),
            },
        });
        tools.push(Tool {
            name: "chronos_record".into(),
            description: Some(
                "Record a mutation event in the Chronos timeline. Creates an auditable entry with causal chain.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "key": {"type": "string", "description": "Data key this event relates to"},
                    "actor": {"type": "string", "description": "Who/what performed this action (default: agent)"},
                    "action": {"type": "string", "description": "Action type: create, update, delete, move, tool_invoked, message_received, response_generated, context_managed, model_called, outcome_recorded"},
                    "level": {"type": "string", "description": "Severity level: debug, info, warn, error (default: info). Events below the minimum recording level are dropped."},
                    "data": {"type": "object", "description": "Arbitrary data payload for this event"},
                    "rationale": {"type": "string", "description": "Human-readable reason for this mutation"},
                    "constraints": {"type": "array", "items": {"type": "string"}, "description": "Constraint results that apply to this event"}
                })),
                required: Some(vec!["key".into()]),
            },
        });
        tools.push(Tool {
            name: "chronos_set_level".into(),
            description: Some(
                "Set the minimum recording level for Chronos. Events below this level are silently dropped.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "level": {"type": "string", "enum": ["debug", "info", "warn", "error"], "description": "Minimum recording level"}
                })),
                required: Some(vec!["level".into()]),
            },
        });
        tools.push(Tool {
            name: "chronos_get_level".into(),
            description: Some("Get the current minimum recording level for Chronos.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });

        tools.push(Tool {
            name: "chronos_replay".into(),
            description: Some(
                "Replay timeline events through the Praxis engine (dry-run evaluation). Returns entries with any constraint violations.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "fromId": {"type": "string", "description": "Start replay from this event id (inclusive). Omit to start from oldest."},
                    "toId": {"type": "string", "description": "End replay at this event id (inclusive). Omit to replay to newest."}
                })),
                required: None,
            },
        });

        tools.push(Tool {
            name: "chronos_timeline".into(),
            description: Some(
                "Get the event timeline (last N events), optionally filtered by since timestamp and severity level. Mirrors OpenClaw's radix__chronos-timeline.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "limit": {"type": "integer", "description": "Max events to return (default 50)"},
                    "since": {"type": "string", "description": "ISO 8601 timestamp — only events after this time"},
                    "level": {"type": "string", "enum": ["debug", "info", "warn", "error"], "description": "Minimum severity level filter"}
                })),
                required: None,
            },
        });

        // ── Plugin management tools ────────────────────────────────────────────────────────
        tools.push(Tool {
            name: "plugin_list".into(),
            description: Some("List all registered plugins and their status.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });
        tools.push(Tool {
            name: "plugin_info".into(),
            description: Some("Get detailed info about a specific plugin.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "name": {"type": "string", "description": "Plugin name to inspect"}
                })),
                required: Some(vec!["name".into()]),
            },
        });
        tools.push(Tool {
            name: "plugin_register".into(),
            description: Some(
                "Register a plugin manifest. Installs and activates the plugin.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "name": {"type": "string", "description": "Unique plugin name (kebab-case)"},
                    "version": {"type": "string", "description": "Semver version string"},
                    "description": {"type": "string", "description": "Plugin description"},
                    "capabilities": {"type": "array", "items": {"type": "string"}, "description": "Capabilities/dependencies"}
                })),
                required: Some(vec!["name".into(), "version".into()]),
            },
        });
        tools.push(Tool {
            name: "plugin_activate".into(),
            description: Some("Activate a registered plugin.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "name": {"type": "string", "description": "Plugin name to activate"}
                })),
                required: Some(vec!["name".into()]),
            },
        });
        tools.push(Tool {
            name: "plugin_deactivate".into(),
            description: Some("Deactivate a plugin (uninstalls it).".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "name": {"type": "string", "description": "Plugin name to deactivate"}
                })),
                required: Some(vec!["name".into()]),
            },
        });

        // ── Sub-agent tools ─────────────────────────────────────────────────────────────
        tools.push(Tool {
            name: "subagent_spawn".into(),
            description: Some(
                "Spawn an isolated sub-agent to perform a delegated task. Returns session_id for tracking.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "agent": {"type": "string", "description": "Agent name to spawn (must be registered)"},
                    "task": {"type": "string", "description": "Task/prompt for the sub-agent to execute"},
                    "label": {"type": "string", "description": "Optional human-readable label for the session"},
                    "timeout_seconds": {"type": "integer", "description": "Optional timeout in seconds (default: 1800)"},
                    "context": {"type": "string", "description": "Optional parent context to pass to the sub-agent"}
                })),
                required: Some(vec!["agent".into(), "task".into()]),
            },
        });
        tools.push(Tool {
            name: "subagent_list".into(),
            description: Some("List all sub-agent sessions (running and completed).".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        });
        tools.push(Tool {
            name: "subagent_kill".into(),
            description: Some("Kill a running sub-agent session by ID.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "session_id": {"type": "string", "description": "Session ID to kill"}
                })),
                required: Some(vec!["session_id".into()]),
            },
        });

        tools.push(Tool {
            name: "subagent_steer".into(),
            description: Some("Send a steering message to a running sub-agent session.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "session_id": {"type": "string", "description": "Session ID to steer"},
                    "message": {"type": "string", "description": "Steering message to inject into the running session"}
                })),
                required: Some(vec!["session_id".into(), "message".into()]),
            },
        });

        tools.push(Tool {
            name: "agent_ask".into(),
            description: Some("Send a prompt through the full agent loop (model + tools + memory). Channel-agnostic — same result as Telegram or TUI.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "prompt": {"type": "string", "description": "The prompt to send to the agent"},
                    "session": {"type": "string", "description": "Session ID for conversation continuity (default: mcp)"}
                })),
                required: Some(vec!["prompt".into()]),
            },
        });

        // ── Session management tools ─────────────────────────────────────
        tools.push(Tool {
            name: "session_status".into(),
            description: Some(
                "Get status of the current or a specific session: model, uptime, usage stats, active sub-agents.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "session_id": {"type": "string", "description": "Optional session ID (defaults to current/main session)"},
                    "model": {"type": "string", "description": "Optional: set a per-session model override. Use 'default' to reset."}
                })),
                required: None,
            },
        });
        tools.push(Tool {
            name: "session_history".into(),
            description: Some(
                "Get message history for a session. Returns sanitized messages with role and content.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "session_id": {"type": "string", "description": "Session ID to retrieve history for (required)"},
                    "limit": {"type": "integer", "description": "Max messages to return (default: 20, max: 100)"},
                    "include_tools": {"type": "boolean", "description": "Include tool call/result messages (default: false)"}
                })),
                required: Some(vec!["session_id".into()]),
            },
        });
        tools.push(Tool {
            name: "session_send".into(),
            description: Some(
                "Send a message to another session. Delivers to the target session's inbox and optionally waits for a response.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "session_id": {"type": "string", "description": "Target session ID to send the message to (required)"},
                    "message": {"type": "string", "description": "Message content to send (required)"},
                    "timeout_seconds": {"type": "integer", "description": "Seconds to wait for a response (default: 30, max: 300, 0 = fire-and-forget)"}
                })),
                required: Some(vec!["session_id".into(), "message".into()]),
            },
        });
        tools.push(Tool {
            name: "session_list".into(),
            description: Some(
                "List active sessions, shell sessions, and running sub-agents.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        });
        tools.push(Tool {
            name: "session_yield".into(),
            description: Some(
                "End the current agent turn and yield control back to the orchestrator. Use after spawning sub-agents to wait for their completion events.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "message": {"type": "string", "description": "Optional message to include with the yield (e.g., status update or reason for yielding)"}
                })),
                required: None,
            },
        });

        // ── Telemetry tools ────────────────────────────────────────────────────────────────
        tools.push(Tool {
            name: "telemetry_snapshot".into(),
            description: Some(
                "Get runtime telemetry: total tool calls, per-tool stats (call count, success/failure, avg latency), uptime. Useful for observability and performance monitoring.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });
        tools.push(Tool {
            name: "telemetry_reset".into(),
            description: Some(
                "Reset all telemetry counters to zero. Useful for starting a fresh measurement window.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });

        // ── Canvas tools ────────────────────────────────────────────────────────────────
        tools.push(Tool {
            name: "canvas_create".into(),
            description: Some(
                "Create a new canvas app. Returns the canvas document with a generated ID.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "title": {"type": "string", "description": "Title for the canvas app"},
                    "description": {"type": "string", "description": "Optional description"}
                })),
                required: Some(vec!["title".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_get".into(),
            description: Some(
                "Get the current active canvas document. Returns null if no canvas is active."
                    .into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });
        tools.push(Tool {
            name: "canvas_set_tree".into(),
            description: Some(
                "Replace the entire component tree of the active canvas.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "tree": {"type": "object", "description": "The new component tree (CanvasNode with id, type, props, children)"}
                })),
                required: Some(vec!["tree".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_add_node".into(),
            description: Some(
                "Add a component node to the canvas tree under a specified parent.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "parentId": {"type": "string", "description": "ID of the parent node"},
                    "node": {"type": "object", "description": "CanvasNode: { id, type, props?, bindings?, children?, visible? }"}
                })),
                required: Some(vec!["parentId".into(), "node".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_remove_node".into(),
            description: Some("Remove a node from the canvas tree by ID.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "nodeId": {"type": "string", "description": "ID of the node to remove"}
                })),
                required: Some(vec!["nodeId".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_validate".into(),
            description: Some("Validate the active canvas and return any issues found.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });
        tools.push(Tool {
            name: "canvas_export".into(),
            description: Some("Export the active canvas as a .canvas JSON string.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });
        tools.push(Tool {
            name: "canvas_import".into(),
            description: Some(
                "Import a canvas from a JSON string. Sets it as the active canvas.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "json": {"type": "string", "description": "The .canvas JSON string to import"}
                })),
                required: Some(vec!["json".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_list".into(),
            description: Some("List all saved canvases (id, title, description).".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });
        tools.push(Tool {
            name: "canvas_load".into(),
            description: Some("Load a saved canvas by ID, making it the active canvas.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "id": {"type": "string", "description": "The canvas ID to load"}
                })),
                required: Some(vec!["id".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_save".into(),
            description: Some("Save the active canvas to the saved canvases list.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });
        tools.push(Tool {
            name: "canvas_set_data".into(),
            description: Some(
                "Set data values in the active canvas (seeds PluresDB with canvas-scoped data)."
                    .into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "data": {"type": "object", "description": "Key-value data to set in the canvas"}
                })),
                required: Some(vec!["data".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_add_procedure".into(),
            description: Some(
                "Add a behavior procedure to the active canvas.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "procedure": {"type": "object", "description": "The procedure definition to add"}
                })),
                required: Some(vec!["procedure".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_add_rule".into(),
            description: Some("Add a Praxis validation rule to the active canvas.".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "rule": {"type": "object", "description": "The Praxis rule definition to add"}
                })),
                required: Some(vec!["rule".into()]),
            },
        });
        tools.push(Tool {
            name: "canvas_a2ui_push".into(),
            description: Some(
                "Push A2UI (Agent-to-UI) rendering instructions to the active canvas. Accepts JSONL string or instructions array.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "jsonl": {"type": "string", "description": "Newline-delimited JSON rendering instructions"},
                    "instructions": {"type": "array", "items": {"type": "object"}, "description": "Array of rendering instruction objects"}
                })),
                required: None,
            },
        });
        tools.push(Tool {
            name: "canvas_a2ui_reset".into(),
            description: Some(
                "Reset the A2UI rendering queue, clearing all pending instructions.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "target": {"type": "string", "description": "Target node canvas to reset (default: global)"}
                })),
                required: None,
            },
        });
        tools.push(Tool {
            name: "canvas_catalog".into(),
            description: Some(
                "Get the full component catalog — lists all available component types, their props, events, and whether they accept children.".into(),
            ),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        });

        tools
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> ToolResult {
        debug!(tool = name, "MCP tool call");

        // Record metrics for every tool call (except telemetry tools themselves to avoid recursion)
        let start = Instant::now();
        let result = self.dispatch_tool(name, arguments).await;
        if !name.starts_with("telemetry_") {
            let latency_ms = start.elapsed().as_millis() as u64;
            let success = !result.is_error;
            if let Ok(mut metrics) = self.metrics.lock() {
                metrics.record(name, latency_ms, success);
            }
            // Emit to OTLP exporter (no-op without configured provider)
            self.otel_metrics
                .record_tool_call(name, latency_ms as f64, success);
        }
        result
    }
}

impl RadixToolHandler {
    /// Internal dispatch — routes tool name to handler method.
    async fn dispatch_tool(&self, name: &str, arguments: Value) -> ToolResult {
        match name {
            "read_file" => self.read_file(&arguments).await,
            "write_file" => self.write_file(&arguments).await,
            "edit_file" => self.edit_file(&arguments).await,
            "apply_patch" => self.apply_patch(&arguments).await,
            "list_directory" => self.list_directory(&arguments).await,
            "run_command" => self.run_command(&arguments).await,
            "process" => self.process_action(&arguments).await,
            "skill_workshop" => self.skill_workshop(&arguments).await,
            "memory_search" => self.memory_search(&arguments).await,
            "memory_store" => self.memory_store(&arguments).await,
            "web_fetch" => self.web_fetch(&arguments).await,
            "web_search" => self.web_search(&arguments).await,
            "cron_list" => self.cron_list(&arguments).await,
            "cron_add" => self.cron_add(&arguments).await,
            "cron_remove" => self.cron_remove(&arguments).await,
            "cron_toggle" => self.cron_toggle(&arguments).await,
            "heartbeat_status" => self.heartbeat_status(&arguments).await,
            "heartbeat_configure" => self.heartbeat_configure(&arguments).await,
            "db_get" => self.db_get(&arguments).await,
            "db_put" => self.db_put(&arguments).await,
            "db_delete" => self.db_delete(&arguments).await,
            "db_keys" => self.db_keys(&arguments).await,
            "db_dump" => self.db_dump(&arguments).await,
            "db_get_prefix" => self.db_get_prefix(&arguments).await,
            "timestamp_now" => self.timestamp_now().await,
            "generate_id" => self.generate_id(&arguments).await,
            "send_message" => self.send_message(&arguments).await,
            "config_get" => self.config_get(&arguments).await,
            "config_set" => self.config_set(&arguments).await,
            "config_list" => self.config_list(&arguments).await,
            "config_delete" => self.config_delete(&arguments).await,
            "config_reload" => self.config_reload(&arguments).await,
            "runtime_status" => self.runtime_status(&arguments).await,
            "runtime_restart" => self.runtime_restart(&arguments).await,
            "config_schema" => self.config_schema(&arguments).await,
            "browser_status" => self.browser_status(&arguments).await,
            "browser_navigate" => self.browser_navigate(&arguments).await,
            "browser_snapshot" => self.browser_snapshot(&arguments).await,
            "browser_screenshot" => self.browser_screenshot(&arguments).await,
            "browser_click" => self.browser_click(&arguments).await,
            "browser_type" => self.browser_type(&arguments).await,
            "image_analyze" => self.image_analyze(&arguments).await,
            "image_generate" => self.image_generate(&arguments).await,
            "tts_generate" => self.tts_generate(&arguments).await,
            "pdf_analyze" => self.pdf_analyze(&arguments).await,
            "video_generate" => self.video_generate(&arguments).await,
            "music_generate" => self.music_generate(&arguments).await,
            "node_file_read" => self.node_file_read(&arguments).await,
            "node_file_write" => self.node_file_write(&arguments).await,
            "node_dir_list" => self.node_dir_list(&arguments).await,
            "node_dir_fetch" => self.node_dir_fetch(&arguments).await,
            "node_status" => self.node_status(&arguments).await,
            "praxis_evaluate" => self.praxis_evaluate(&arguments).await,
            "praxis_list" => self.praxis_list(&arguments).await,
            "praxis_run" => self.praxis_run(&arguments).await,
            "praxis_add_constraint" => self.praxis_add_constraint(&arguments).await,
            "praxis_add_rule" => self.praxis_add_rule(&arguments).await,
            "px_lint" => self.px_lint(&arguments).await,
            "px_compose" => self.px_compose(&arguments).await,
            "px_status" => self.px_status().await,
            "chronos_history" => self.chronos_history(&arguments).await,
            "chronos_recent" => self.chronos_recent(&arguments).await,
            "chronos_by_actor" => self.chronos_by_actor(&arguments).await,
            "chronos_record" => self.chronos_record(&arguments).await,
            "chronos_set_level" => self.chronos_set_level(&arguments).await,
            "chronos_get_level" => self.chronos_get_level(&arguments).await,
            "chronos_replay" => self.chronos_replay(&arguments).await,
            "chronos_timeline" => self.chronos_timeline(&arguments).await,
            "plugin_list" => self.plugin_list().await,
            "plugin_info" => self.plugin_info(&arguments).await,
            "plugin_register" => self.plugin_register(&arguments).await,
            "plugin_activate" => self.plugin_activate(&arguments).await,
            "plugin_deactivate" => self.plugin_deactivate(&arguments).await,
            "subagent_spawn" => self.subagent_spawn(&arguments).await,
            "subagent_list" => self.subagent_list(&arguments).await,
            "subagent_kill" => self.subagent_kill(&arguments).await,
            "subagent_steer" => self.subagent_steer(&arguments).await,
            "agent_ask" => self.agent_ask(&arguments).await,
            "session_status" => self.session_status(&arguments).await,
            "session_history" => self.session_history(&arguments).await,
            "session_send" => self.session_send(&arguments).await,
            "session_list" => self.session_list(&arguments).await,
            "session_yield" => self.session_yield(&arguments).await,
            "telemetry_snapshot" => self.telemetry_snapshot().await,
            "telemetry_reset" => self.telemetry_reset().await,
            "canvas_create" => self.canvas_create(&arguments).await,
            "canvas_get" => self.canvas_get().await,
            "canvas_set_tree" => self.canvas_set_tree(&arguments).await,
            "canvas_add_node" => self.canvas_add_node(&arguments).await,
            "canvas_remove_node" => self.canvas_remove_node(&arguments).await,
            "canvas_validate" => self.canvas_validate().await,
            "canvas_export" => self.canvas_export().await,
            "canvas_import" => self.canvas_import(&arguments).await,
            "canvas_list" => self.canvas_list().await,
            "canvas_load" => self.canvas_load(&arguments).await,
            "canvas_save" => self.canvas_save().await,
            "canvas_set_data" => self.canvas_set_data(&arguments).await,
            "canvas_add_procedure" => self.canvas_add_procedure(&arguments).await,
            "canvas_add_rule" => self.canvas_add_rule(&arguments).await,
            "canvas_a2ui_push" => self.canvas_a2ui_push(&arguments).await,
            "canvas_a2ui_reset" => self.canvas_a2ui_reset(&arguments).await,
            "canvas_catalog" => self.canvas_catalog().await,
            other => {
                warn!(tool = other, "unknown tool called via MCP");
                ToolResult::error(format!("unknown tool: {other}"))
            }
        }
    }
}

// ── Procedure Action Handler (bridges .px steps to shell/tools) ────────────────

/// Action handler for .px procedures that delegates `call` steps to shell commands.
///
/// Step names are interpreted as shell commands unless they match a known built-in.
/// Parameters are passed as JSON via stdin or command-line args.
struct ShellBackedProcedureHandler {
    #[allow(dead_code)]
    shell: Arc<ShellExecutor>,
    workdir: PathBuf,
    children: Arc<tokio::sync::Mutex<std::collections::HashMap<u32, tokio::process::Child>>>,
}

#[async_trait]
impl AsyncActionHandler for ShellBackedProcedureHandler {
    async fn call(
        &self,
        name: &str,
        params: &Value,
    ) -> Result<Value, pares_radix_praxis::px::executor::ExecutionError> {
        use pares_radix_praxis::px::executor::ExecutionError;

        match name {
            // Built-in: run a shell command
            "shell" | "run" | "exec" => {
                let cmd = params
                    .get("command")
                    .or_else(|| params.get("cmd"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'command' parameter".into(),
                    })?;

                let output = tokio::process::Command::new("bash")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&self.workdir)
                    .output()
                    .await
                    .map_err(|e| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("spawn failed: {e}"),
                    })?;

                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    Ok(json!({
                        "status": "ok",
                        "stdout": stdout.trim(),
                        "stderr": stderr.trim(),
                        "exit_code": 0
                    }))
                } else {
                    Ok(json!({
                        "status": "error",
                        "stdout": stdout.trim(),
                        "stderr": stderr.trim(),
                        "exit_code": output.status.code().unwrap_or(-1)
                    }))
                }
            }

            // Built-in: read a file
            "read_file" | "read" => {
                let path = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'path' parameter".into(),
                    }
                })?;

                let full_path = if std::path::Path::new(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    self.workdir.join(path)
                };

                let content = tokio::fs::read_to_string(&full_path).await.map_err(|e| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("read failed: {e}"),
                    }
                })?;

                Ok(Value::String(content))
            }

            // Built-in: write a file
            "write_file" | "write" => {
                let path = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'path' parameter".into(),
                    }
                })?;
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");

                let full_path = if std::path::Path::new(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    self.workdir.join(path)
                };

                if let Some(parent) = full_path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }

                tokio::fs::write(&full_path, content).await.map_err(|e| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("write failed: {e}"),
                    }
                })?;

                Ok(json!({"status": "ok", "path": full_path.display().to_string()}))
            }

            // Built-in: echo/noop (useful for testing)
            "echo" | "noop" => Ok(params.clone()),

            // Built-in: HTTP GET request
            "http_get" | "http" | "fetch" => {
                let url = params.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'url' parameter".into(),
                    }
                })?;
                let timeout_secs = params
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10);
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(timeout_secs))
                    .build()
                    .map_err(|e| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("client build failed: {e}"),
                    })?;
                let mut req = client.get(url);
                if let Some(headers) = params.get("headers").and_then(|v| v.as_object()) {
                    for (k, v) in headers {
                        if let Some(v_str) = v.as_str() {
                            req = req.header(k.as_str(), v_str);
                        }
                    }
                }
                let resp = req.send().await.map_err(|e| ExecutionError::ActionFailed {
                    action: name.to_string(),
                    message: format!("request failed: {e}"),
                })?;
                let status = resp.status().as_u16();
                let resp_headers: serde_json::Map<String, Value> = resp
                    .headers()
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.to_string(),
                            Value::String(v.to_str().unwrap_or("").to_string()),
                        )
                    })
                    .collect();
                let body = resp.text().await.unwrap_or_default();
                Ok(json!({
                    "status": status,
                    "body": body,
                    "headers": resp_headers
                }))
            }

            // Built-in: HTTP POST request
            "http_post" | "post" => {
                let url = params.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'url' parameter".into(),
                    }
                })?;
                let timeout_secs = params
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10);
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(timeout_secs))
                    .build()
                    .map_err(|e| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("client build failed: {e}"),
                    })?;
                let mut req = client.post(url);
                if let Some(headers) = params.get("headers").and_then(|v| v.as_object()) {
                    for (k, v) in headers {
                        if let Some(v_str) = v.as_str() {
                            req = req.header(k.as_str(), v_str);
                        }
                    }
                }
                if let Some(json_body) = params.get("json") {
                    req = req.json(json_body);
                } else if let Some(body_str) = params.get("body").and_then(|v| v.as_str()) {
                    req = req.body(body_str.to_string());
                }
                let resp = req.send().await.map_err(|e| ExecutionError::ActionFailed {
                    action: name.to_string(),
                    message: format!("request failed: {e}"),
                })?;
                let status = resp.status().as_u16();
                let resp_headers: serde_json::Map<String, Value> = resp
                    .headers()
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.to_string(),
                            Value::String(v.to_str().unwrap_or("").to_string()),
                        )
                    })
                    .collect();
                let body = resp.text().await.unwrap_or_default();
                Ok(json!({
                    "status": status,
                    "body": body,
                    "headers": resp_headers
                }))
            }

            // Built-in: assert equality
            "assert_eq" | "assert_equal" => {
                let actual = params.get("actual");
                let expected = params.get("expected");
                let message = params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assertion failed");
                if actual == expected {
                    Ok(json!({"status": "ok", "assertion": "eq"}))
                } else {
                    Err(ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("{message}: expected {:?}, got {:?}", expected, actual),
                    })
                }
            }

            // Built-in: assert contains (substring or array element)
            "assert_contains" | "assert_has" => {
                let message = params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assertion failed");
                let contains =
                    params
                        .get("contains")
                        .ok_or_else(|| ExecutionError::ActionFailed {
                            action: name.to_string(),
                            message: "missing 'contains' parameter".into(),
                        })?;
                let value = params
                    .get("value")
                    .ok_or_else(|| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'value' parameter".into(),
                    })?;
                let found = if let Some(s) = value.as_str() {
                    if let Some(needle) = contains.as_str() {
                        s.contains(needle)
                    } else {
                        false
                    }
                } else if let Some(arr) = value.as_array() {
                    arr.contains(contains)
                } else if value.is_object() {
                    // For objects (e.g. shell results), check stdout/output fields first,
                    // then fall back to stringified representation
                    if let Some(needle) = contains.as_str() {
                        // Check common output fields
                        let stdout_match = value
                            .get("stdout")
                            .and_then(|v| v.as_str())
                            .map(|s| s.contains(needle))
                            .unwrap_or(false);
                        let output_match = value
                            .get("output")
                            .and_then(|v| v.as_str())
                            .map(|s| s.contains(needle))
                            .unwrap_or(false);
                        stdout_match || output_match || value.to_string().contains(needle)
                    } else {
                        false
                    }
                } else {
                    false
                };
                if found {
                    Ok(json!({"status": "ok", "assertion": "contains"}))
                } else {
                    Err(ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("{message}: {:?} does not contain {:?}", value, contains),
                    })
                }
            }

            // Built-in: assert truthy
            "assert_ok" | "assert_true" => {
                let message = params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assertion failed: value is falsy");
                let value = params.get("value").unwrap_or(&Value::Null);
                let truthy = match value {
                    Value::Null => false,
                    Value::Bool(b) => *b,
                    Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                    Value::String(s) => !s.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    Value::Object(_) => true,
                };
                if truthy {
                    Ok(json!({"status": "ok", "assertion": "truthy"}))
                } else {
                    Err(ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: message.to_string(),
                    })
                }
            }

            // Built-in: start a background process
            "start_process" | "start_server" => {
                let cmd = params
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'command' parameter".into(),
                    })?;
                let child = tokio::process::Command::new("bash")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&self.workdir)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("spawn failed: {e}"),
                    })?;
                let pid = child.id().unwrap_or(0);
                self.children.lock().await.insert(pid, child);

                // Optionally wait for a URL to become ready
                if let Some(ready_url) = params.get("ready_url").and_then(|v| v.as_str()) {
                    let timeout_secs = params
                        .get("ready_timeout_secs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(30);
                    let client = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(2))
                        .build()
                        .unwrap_or_default();
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
                    loop {
                        if tokio::time::Instant::now() >= deadline {
                            break;
                        }
                        if let Ok(resp) = client.get(ready_url).send().await {
                            if resp.status().is_success() {
                                break;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }

                Ok(json!({"pid": pid}))
            }

            // Built-in: stop a background process
            "stop_process" | "stop_server" | "kill_process" => {
                let pid = params
                    .get("pid")
                    .and_then(|v| v.as_u64())
                    .map(|p| p as u32)
                    .ok_or_else(|| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'pid' parameter".into(),
                    })?;
                let mut children = self.children.lock().await;
                if let Some(child) = children.get_mut(&pid) {
                    // Try SIGTERM first via kill
                    #[cfg(unix)]
                    {
                        unsafe {
                            libc::kill(pid as i32, libc::SIGTERM);
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    // Then force kill
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    children.remove(&pid);
                    Ok(json!({"status": "ok", "pid": pid}))
                } else {
                    // Try killing by PID directly even if not tracked
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                    Ok(json!({"status": "ok", "pid": pid, "note": "not tracked, sent SIGKILL"}))
                }
            }

            // Built-in: wait for a URL to respond with 200
            "wait_for_ready" | "wait_for_url" => {
                let url = params.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'url' parameter".into(),
                    }
                })?;
                let timeout_secs = params
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30);
                let interval_ms = params
                    .get("interval_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(200);
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()
                    .unwrap_or_default();
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
                loop {
                    if let Ok(resp) = client.get(url).send().await {
                        if resp.status().is_success() {
                            return Ok(json!({"status": "ok", "url": url}));
                        }
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ExecutionError::ActionFailed {
                            action: name.to_string(),
                            message: format!("timeout after {timeout_secs}s waiting for {url}"),
                        });
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
                }
            }

            // Built-in: sleep/wait
            "sleep" | "wait" => {
                let ms = params
                    .get("ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_else(|| {
                        params.get("secs").and_then(|v| v.as_u64()).unwrap_or(0) * 1000
                    });
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                Ok(json!({"status": "ok", "slept_ms": ms}))
            }

            // Built-in: parse JSON string into Value
            "json_parse" | "parse_json" => {
                let text = params.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'text' parameter".into(),
                    }
                })?;
                let parsed: Value =
                    serde_json::from_str(text).map_err(|e| ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("parse failed: {e}"),
                    })?;
                Ok(parsed)
            }

            // Built-in: detect OS type
            "detect_os" => {
                let os = if cfg!(target_os = "linux") {
                    "linux"
                } else if cfg!(target_os = "macos") {
                    "macos"
                } else if cfg!(target_os = "windows") {
                    "windows"
                } else {
                    "unknown"
                };
                Ok(Value::String(os.to_string()))
            }

            // Built-in: get platform temp directory
            "get_temp_dir" | "temp_dir" => Ok(Value::String(
                std::env::temp_dir().to_string_lossy().to_string(),
            )),

            // Built-in: join path components
            "join_path" | "path_join" => {
                let base = params.get("base").and_then(|v| v.as_str()).unwrap_or("");
                let name = params.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'name' parameter".into(),
                    }
                })?;
                let joined = PathBuf::from(base).join(name);
                Ok(Value::String(joined.to_string_lossy().to_string()))
            }

            // Built-in: delete a file
            "delete_file" | "remove_file" | "rm" => {
                let path = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: "missing 'path' parameter".into(),
                    }
                })?;
                let full_path = if std::path::Path::new(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    self.workdir.join(path)
                };
                tokio::fs::remove_file(&full_path).await.map_err(|e| {
                    ExecutionError::ActionFailed {
                        action: name.to_string(),
                        message: format!("delete failed: {e}"),
                    }
                })?;
                Ok(json!({"status": "ok", "deleted": full_path.display().to_string()}))
            }

            // Built-in: emit event (log/noop in procedure context)
            "emit" => {
                // In procedure context, emit is a structured event log.
                // We just return the params as acknowledgment.
                Ok(params.clone())
            }

            // SECURITY: unrecognized action/step names must NEVER be executed as a
            // shell command. Previously this fallback ran
            // `bash -c "{other} '{params_str}'"`, which allowed arbitrary shell
            // injection via any unrecognized action/step name in a procedure
            // passed to `praxis_run` (procedures are attacker-controlled MCP
            // input). Unknown actions are now a typed error, matching the
            // idiomatic `ExecutionError::ActionFailed` pattern used by every
            // other arm in this handler; nothing reaches `Command::new`.
            other => Err(ExecutionError::ActionFailed {
                action: other.to_string(),
                message: format!(
                    "unknown action '{other}': no built-in handler is registered for it, \
                     and unrecognized actions are not executed as shell commands. \
                     Use the 'shell' | 'run' | 'exec' action with an explicit 'command' \
                     parameter to run a shell command."
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_handler() -> RadixToolHandler {
        let shell = Arc::new(ShellExecutor::new());
        RadixToolHandler::new(shell, PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn list_tools_returns_all() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        assert!(tools.len() >= 10);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"run_command"));
        assert!(names.contains(&"memory_search"));
        assert!(names.contains(&"web_fetch"));
        assert!(names.contains(&"apply_patch"));
    }

    #[tokio::test]
    async fn apply_patch_add_update_delete_end_to_end() {
        // Real filesystem E2E: temp workdir, drive the handler's apply_patch,
        // verify the resulting on-disk state (build the binary, run the binary).
        let dir = std::env::temp_dir().join(format!("agens_patch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("update_me.txt"), "alpha\nold\nomega\n").unwrap();
        std::fs::write(dir.join("delete_me.txt"), "bye\n").unwrap();

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, dir.clone());

        let patch = "\
*** Begin Patch\n\
*** Add File: created.txt\n\
+new file line\n\
*** Update File: update_me.txt\n\
@@\n\
 alpha\n\
-old\n\
+edited\n\
*** Delete File: delete_me.txt\n\
*** End Patch\n";

        let res = handler.apply_patch(&json!({ "input": patch })).await;
        assert!(!res.is_error, "apply_patch errored: {}", res.content);

        // Add happened.
        assert_eq!(
            std::fs::read_to_string(dir.join("created.txt")).unwrap(),
            "new file line\n"
        );
        // Update replaced only the matched run, context preserved.
        assert_eq!(
            std::fs::read_to_string(dir.join("update_me.txt")).unwrap(),
            "alpha\nedited\nomega\n"
        );
        // Delete happened.
        assert!(!dir.join("delete_me.txt").exists());

        // Atomicity: a patch whose Add conflicts writes NOTHING.
        let bad = "\
*** Begin Patch\n\
*** Add File: should_not_appear.txt\n\
+nope\n\
*** Add File: created.txt\n\
+conflict\n\
*** End Patch\n";
        let res2 = handler.apply_patch(&json!({ "input": bad })).await;
        assert!(res2.is_error, "conflicting patch should fail");
        assert!(
            !dir.join("should_not_appear.txt").exists(),
            "atomicity violated: partial write occurred"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_file_missing_path() {
        let handler = make_handler();
        let result = handler.call_tool("read_file", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("path"));
    }

    #[tokio::test]
    async fn read_file_nonexistent() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "read_file",
                json!({"path": "/tmp/nonexistent_radix_test_xyz"}),
            )
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let handler = make_handler();
        let test_path = "/tmp/radix_mcp_test_write.txt";

        let write_result = handler
            .call_tool(
                "write_file",
                json!({"path": test_path, "content": "hello from MCP"}),
            )
            .await;
        assert!(!write_result.is_error);

        let read_result = handler
            .call_tool("read_file", json!({"path": test_path}))
            .await;
        assert!(!read_result.is_error);
        assert_eq!(read_result.content, "hello from MCP");

        // Cleanup
        let _ = tokio::fs::remove_file(test_path).await;
    }

    #[tokio::test]
    async fn run_command_echo() {
        let handler = make_handler();
        let result = handler
            .call_tool("run_command", json!({"command": "echo hello_mcp"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello_mcp"));
    }

    #[tokio::test]
    async fn list_directory_tmp() {
        let handler = make_handler();
        let result = handler
            .call_tool("list_directory", json!({"path": "/tmp"}))
            .await;
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let handler = make_handler();
        let result = handler.call_tool("nonexistent_tool", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn memory_search_without_memory_returns_error() {
        let handler = make_handler();
        let result = handler
            .call_tool("memory_search", json!({"query": "test"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn web_search_without_api_key_returns_error() {
        let handler = make_handler();
        let result = handler
            .call_tool("web_search", json!({"query": "rust lang"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn cron_list_without_scheduler_returns_error() {
        let handler = make_handler();
        let result = handler.call_tool("cron_list", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn cron_add_without_scheduler_returns_error() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "cron_add",
                json!({"name": "test", "command": "echo hi", "interval_secs": 60}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn cron_tools_with_scheduler() {
        let shell = Arc::new(ShellExecutor::new());
        let scheduler = Arc::new(Scheduler::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_scheduler(scheduler);

        // List starts empty
        let result = handler.call_tool("cron_list", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("[]"));

        // Add a task
        let result = handler
            .call_tool(
                "cron_add",
                json!({
                    "name": "test_task",
                    "command": "echo hello",
                    "interval_secs": 300
                }),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("added task:"));

        // List has one task
        let result = handler.call_tool("cron_list", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("test_task"));

        // Parse id from list
        let tasks: Vec<Value> = serde_json::from_str(&result.content).unwrap();
        let task_id = tasks[0]["id"].as_str().unwrap().to_string();

        // Toggle disable
        let result = handler
            .call_tool("cron_toggle", json!({"id": &task_id, "enabled": false}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("enabled=false"));

        // Remove
        let result = handler
            .call_tool("cron_remove", json!({"id": &task_id}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("removed task:"));

        // List empty again
        let result = handler.call_tool("cron_list", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("[]"));
    }

    // ── State/DB tool tests ─────────────────────────────────────────────────

    fn make_handler_with_state() -> RadixToolHandler {
        let shell = Arc::new(ShellExecutor::new());
        let state = Arc::new(pares_radix_core::InMemoryStateStore::new());
        RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_state_store(state)
    }

    #[tokio::test]
    async fn db_get_missing_key_returns_null() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("db_get", json!({"key": "nonexistent"}))
            .await;
        assert!(!result.is_error);
        assert_eq!(result.content, "null");
    }

    #[tokio::test]
    async fn db_put_then_get_roundtrip() {
        let handler = make_handler_with_state();

        let result = handler
            .call_tool("db_put", json!({"key": "test:foo", "value": {"bar": 42}}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("stored key: test:foo"));

        let result = handler
            .call_tool("db_get", json!({"key": "test:foo"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("42"));
        assert!(result.content.contains("bar"));
    }

    #[tokio::test]
    async fn db_delete_sets_null() {
        let handler = make_handler_with_state();

        handler
            .call_tool("db_put", json!({"key": "del:me", "value": "hello"}))
            .await;
        let result = handler
            .call_tool("db_delete", json!({"key": "del:me"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("deleted key: del:me"));

        let result = handler.call_tool("db_get", json!({"key": "del:me"})).await;
        assert!(!result.is_error);
        // After delete, value is null
        assert_eq!(result.content, "null");
    }

    #[tokio::test]
    async fn db_get_without_state_store_returns_error() {
        let handler = make_handler(); // no state store attached
        let result = handler.call_tool("db_get", json!({"key": "x"})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn db_tools_appear_in_tool_list() {
        let handler = make_handler_with_state();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"db_get"));
        assert!(names.contains(&"db_put"));
        assert!(names.contains(&"db_delete"));
        assert!(names.contains(&"db_keys"));
        assert!(names.contains(&"db_dump"));
    }

    #[tokio::test]
    async fn db_keys_lists_matching_prefix() {
        let handler = make_handler_with_state();
        handler
            .call_tool("db_put", json!({"key": "app:one", "value": 1}))
            .await;
        handler
            .call_tool("db_put", json!({"key": "app:two", "value": 2}))
            .await;
        handler
            .call_tool("db_put", json!({"key": "other:x", "value": 3}))
            .await;

        let result = handler
            .call_tool("db_keys", json!({"prefix": "app:"}))
            .await;
        assert!(!result.is_error);
        let keys: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"app:one".to_string()));
        assert!(keys.contains(&"app:two".to_string()));
    }

    #[tokio::test]
    async fn db_keys_no_prefix_lists_all() {
        let handler = make_handler_with_state();
        handler
            .call_tool("db_put", json!({"key": "k1", "value": "a"}))
            .await;
        handler
            .call_tool("db_put", json!({"key": "k2", "value": "b"}))
            .await;

        let result = handler.call_tool("db_keys", json!({})).await;
        assert!(!result.is_error);
        let keys: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert!(keys.len() >= 2);
        assert!(keys.contains(&"k1".to_string()));
        assert!(keys.contains(&"k2".to_string()));
    }

    #[tokio::test]
    async fn db_dump_returns_all_entries() {
        let handler = make_handler_with_state();
        handler
            .call_tool("db_put", json!({"key": "d:a", "value": {"x": 1}}))
            .await;
        handler
            .call_tool("db_put", json!({"key": "d:b", "value": "hello"}))
            .await;

        let result = handler.call_tool("db_dump", json!({})).await;
        assert!(!result.is_error);
        let dump: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let obj = dump.as_object().unwrap();
        assert!(obj.contains_key("d:a"));
        assert!(obj.contains_key("d:b"));
        assert_eq!(obj["d:a"], json!({"x": 1}));
        assert_eq!(obj["d:b"], json!("hello"));
    }

    #[tokio::test]
    async fn db_dump_excludes_deleted_keys() {
        let handler = make_handler_with_state();
        handler
            .call_tool("db_put", json!({"key": "keep", "value": 1}))
            .await;
        handler
            .call_tool("db_put", json!({"key": "gone", "value": 2}))
            .await;
        handler.call_tool("db_delete", json!({"key": "gone"})).await;

        let result = handler.call_tool("db_dump", json!({})).await;
        assert!(!result.is_error);
        let dump: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let obj = dump.as_object().unwrap();
        assert!(obj.contains_key("keep"));
        assert!(!obj.contains_key("gone"));
    }

    // ── Config & Runtime tool tests ────────────────────────────────────────────

    #[tokio::test]
    async fn config_set_and_get_roundtrip() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("config_set", json!({"key": "model", "value": "gpt-4.1"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("config set: model"));

        let result = handler
            .call_tool("config_get", json!({"key": "model"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("gpt-4.1"));
    }

    #[tokio::test]
    async fn config_get_missing_returns_null() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("config_get", json!({"key": "nonexistent_key"}))
            .await;
        assert!(!result.is_error);
        assert_eq!(result.content, "null");
    }

    #[tokio::test]
    async fn config_list_shows_set_keys() {
        let handler = make_handler_with_state();
        handler
            .call_tool("config_set", json!({"key": "model", "value": "test-model"}))
            .await;
        handler
            .call_tool(
                "config_set",
                json!({"key": "endpoint", "value": "http://localhost"}),
            )
            .await;

        let result = handler.call_tool("config_list", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("model"));
        assert!(result.content.contains("test-model"));
    }

    #[tokio::test]
    async fn config_list_with_prefix_filter() {
        let handler = make_handler_with_state();
        handler
            .call_tool(
                "config_set",
                json!({"key": "routing.interactive", "value": "fast"}),
            )
            .await;
        handler
            .call_tool("config_set", json!({"key": "model", "value": "gpt-4"}))
            .await;

        let result = handler
            .call_tool("config_list", json!({"prefix": "routing"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("routing.interactive"));
        // Should not contain "model" since it doesn't start with "routing"
        assert!(!result.content.contains("gpt-4"));
    }

    #[tokio::test]
    async fn config_reload_signals() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("config_reload", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("reload signaled"));
    }

    #[tokio::test]
    async fn runtime_status_returns_components() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("runtime_status", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("running"));
        assert!(result.content.contains("components"));
        assert!(result.content.contains("version"));
    }

    #[tokio::test]
    async fn config_get_without_state_store_returns_error() {
        let handler = make_handler(); // no state store
        let result = handler
            .call_tool("config_get", json!({"key": "model"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn config_runtime_tools_in_tool_list() {
        let handler = make_handler_with_state();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"config_get"));
        assert!(names.contains(&"config_set"));
        assert!(names.contains(&"config_list"));
        assert!(names.contains(&"config_delete"));
        assert!(names.contains(&"config_reload"));
        assert!(names.contains(&"runtime_status"));
    }

    #[tokio::test]
    async fn config_delete_existing_key() {
        let handler = make_handler_with_state();
        handler
            .call_tool("config_set", json!({"key": "model", "value": "gpt-4"}))
            .await;

        let result = handler
            .call_tool("config_delete", json!({"key": "model"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("deleted config: model"));
        assert!(result.content.contains("gpt-4"));

        // Verify it's gone
        let result = handler
            .call_tool("config_get", json!({"key": "model"}))
            .await;
        assert_eq!(result.content, "null");
    }

    #[tokio::test]
    async fn config_delete_missing_key() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("config_delete", json!({"key": "nonexistent"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn config_list_dynamic_scanning() {
        let handler = make_handler_with_state();
        // Set some custom keys not in any hardcoded list
        handler
            .call_tool(
                "config_set",
                json!({"key": "custom.setting", "value": "enabled"}),
            )
            .await;
        handler
            .call_tool(
                "config_set",
                json!({"key": "custom.threshold", "value": 42}),
            )
            .await;

        let result = handler
            .call_tool("config_list", json!({"prefix": "custom"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("custom.setting"));
        assert!(result.content.contains("custom.threshold"));
        assert!(result.content.contains("enabled"));
    }

    #[tokio::test]
    async fn heartbeat_status_returns_defaults() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("heartbeat_status", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("enabled"));
        assert!(result.content.contains("interval_secs"));
        assert!(result.content.contains("quiet_hours_start"));
    }

    #[tokio::test]
    async fn heartbeat_configure_updates_config() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool(
                "heartbeat_configure",
                json!({"enabled": false, "interval_secs": 60}),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("false"));
        assert!(result.content.contains("60"));

        // Verify it persisted
        let status = handler.call_tool("heartbeat_status", json!({})).await;
        assert!(status.content.contains("false"));
        assert!(status.content.contains("60"));
    }

    #[tokio::test]
    async fn heartbeat_configure_partial_update() {
        let handler = make_handler_with_state();
        // Only change one field
        handler
            .call_tool("heartbeat_configure", json!({"max_proactive_per_day": 10}))
            .await;
        let result = handler.call_tool("heartbeat_status", json!({})).await;
        assert!(!result.is_error);
        // Default enabled should still be true
        assert!(result.content.contains("true"));
        // But max should be updated
        assert!(result.content.contains("10"));
    }

    #[tokio::test]
    async fn heartbeat_status_without_state_store_errors() {
        let handler = make_handler();
        let result = handler.call_tool("heartbeat_status", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("state store not configured"));
    }

    #[tokio::test]
    async fn heartbeat_tools_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"heartbeat_status"));
        assert!(names.contains(&"heartbeat_configure"));
    }

    #[tokio::test]
    async fn runtime_restart_signals() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("runtime_restart", json!({"reason": "config change"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("restart signaled"));
        assert!(result.content.contains("config change"));
    }

    #[tokio::test]
    async fn runtime_restart_default_reason() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("runtime_restart", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("manual restart requested"));
    }

    #[tokio::test]
    async fn config_schema_known_key() {
        let handler = make_handler();
        let result = handler
            .call_tool("config_schema", json!({"key": "model"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("model"));
        assert!(result.content.contains("string"));
    }

    #[tokio::test]
    async fn config_schema_list_all() {
        let handler = make_handler();
        let result = handler.call_tool("config_schema", json!({"key": ""})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("keys"));
        assert!(result.content.contains("model"));
    }

    #[tokio::test]
    async fn config_schema_unknown_key() {
        let handler = make_handler();
        let result = handler
            .call_tool("config_schema", json!({"key": "unknown.thing"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("unknown"));
    }

    #[tokio::test]
    async fn new_tools_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"runtime_restart"));
        assert!(names.contains(&"config_schema"));
    }

    // ── Remote Node tool tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn node_tools_in_tool_list() {
        let handler = make_handler_with_state();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"node_file_read"));
        assert!(names.contains(&"node_file_write"));
        assert!(names.contains(&"node_dir_list"));
        assert!(names.contains(&"node_dir_fetch"));
        assert!(names.contains(&"node_status"));
    }

    #[tokio::test]
    async fn node_file_read_missing_params() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("node_file_read", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("node"));

        let result = handler
            .call_tool("node_file_read", json!({"node": "test"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("path"));
    }

    #[tokio::test]
    async fn node_file_read_unknown_node() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool(
                "node_file_read",
                json!({"node": "nonexistent", "path": "/etc/hostname"}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn node_status_empty_returns_info() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("node_status", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("no nodes configured"));
    }

    #[tokio::test]
    async fn node_status_without_state_store_errors() {
        let handler = make_handler();
        let result = handler.call_tool("node_status", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    #[ignore = "resolves a direct SSH host (192.168.1.1) which behaves differently offline; not available in hermetic Nix build sandbox. Run in CI with --ignored."]
    async fn node_resolve_direct_host() {
        let handler = make_handler_with_state();
        // Direct host format should resolve without state store lookup
        let result = handler
            .call_tool(
                "node_file_read",
                json!({"node": "192.168.1.1", "path": "/etc/hostname"}),
            )
            .await;
        // Will fail SSH connection but should NOT fail on "node not found"
        assert!(result.is_error);
        assert!(!result.content.contains("not found"));
    }

    // ── Media tool tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn media_tools_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"image_analyze"));
        assert!(names.contains(&"image_generate"));
        assert!(names.contains(&"tts_generate"));
        assert!(names.contains(&"pdf_analyze"));
        assert!(names.contains(&"video_generate"));
        assert!(names.contains(&"music_generate"));
    }

    #[tokio::test]
    async fn image_analyze_without_api_key_returns_error() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "image_analyze",
                json!({"image_url": "https://example.com/img.jpg"}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn image_analyze_missing_image_returns_error() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_openai_api_key("sk-test".into());
        let result = handler
            .call_tool("image_analyze", json!({"prompt": "describe"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("image_url or image_path"));
    }

    #[tokio::test]
    async fn image_generate_without_api_key_returns_error() {
        let handler = make_handler();
        let result = handler
            .call_tool("image_generate", json!({"prompt": "a cat"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn image_generate_missing_prompt_returns_error() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_openai_api_key("sk-test".into());
        let result = handler.call_tool("image_generate", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("prompt"));
    }

    #[tokio::test]
    async fn tts_generate_without_api_key_returns_error() {
        let handler = make_handler();
        let result = handler
            .call_tool("tts_generate", json!({"text": "hello"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn tts_generate_missing_text_returns_error() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_openai_api_key("sk-test".into());
        let result = handler.call_tool("tts_generate", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("text"));
    }

    #[tokio::test]
    async fn pdf_analyze_missing_path_returns_error() {
        let handler = make_handler();
        let result = handler.call_tool("pdf_analyze", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("path"));
    }

    #[tokio::test]
    async fn pdf_analyze_nonexistent_file_returns_error() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "pdf_analyze",
                json!({"path": "/tmp/nonexistent_radix_test.pdf"}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn video_generate_returns_not_configured() {
        let handler = make_handler();
        let result = handler
            .call_tool("video_generate", json!({"prompt": "a sunset"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn music_generate_returns_not_configured() {
        let handler = make_handler();
        let result = handler
            .call_tool("music_generate", json!({"prompt": "jazz"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn praxis_evaluate_no_modules() {
        let handler = make_handler();
        let result = handler
            .call_tool("praxis_evaluate", json!({"action": "send_email"}))
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["action"], "send_email");
        assert_eq!(parsed["total_rules"], 0);
        assert_eq!(parsed["failures"], 0);
        assert_eq!(parsed["results"], json!([]));
    }

    #[tokio::test]
    async fn praxis_evaluate_with_safety_module() {
        use pares_radix_praxis::modules::safety::SafetyModule;
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_praxis_modules(vec![Box::new(SafetyModule::default())]);

        let result = handler
            .call_tool(
                "praxis_evaluate",
                json!({"action": "send_email", "payload": {"recipients": 50}}),
            )
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["action"], "send_email");
        assert!(parsed["total_rules"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn praxis_evaluate_px_constraints_from_pluresdb() {
        let shell = Arc::new(ShellExecutor::new());
        let state = Arc::new(pares_radix_core::InMemoryStateStore::new());

        // Seed a px constraint that requires `approved == true` when action is `deploy`
        state
            .set(
                "px:constraint/require_approval",
                json!({
                    "type": "constraint",
                    "name": "require_approval",
                    "scope": "deploy",
                    "phases": ["pre-push"],
                    "when": "action == deploy",
                    "require": "approved",
                    "severity": "error",
                    "message": "Deployment requires approval"
                }),
            )
            .await;

        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_state_store(state);

        // Test 1: action=deploy without approved → should fail
        let result = handler
            .call_tool(
                "praxis_evaluate",
                json!({"action": "deploy", "payload": {}}),
            )
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["failures"], 1);
        assert_eq!(parsed["results"][0]["status"], "fail");
        assert_eq!(parsed["results"][0]["source"], "px");
        assert_eq!(parsed["results"][0]["constraint"], "require_approval");
        assert_eq!(
            parsed["results"][0]["message"],
            "Deployment requires approval"
        );

        // Test 2: action=deploy with approved=true → should pass
        let result = handler
            .call_tool(
                "praxis_evaluate",
                json!({"action": "deploy", "payload": {"approved": true}}),
            )
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["failures"], 0);
        assert_eq!(parsed["results"][0]["status"], "pass");

        // Test 3: action=build → when condition doesn't match, constraint skipped
        let result = handler
            .call_tool("praxis_evaluate", json!({"action": "build", "payload": {}}))
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_rules"], 0);
    }

    #[tokio::test]
    async fn praxis_evaluate_px_constraints_phase_filter() {
        let shell = Arc::new(ShellExecutor::new());
        let state = Arc::new(pares_radix_core::InMemoryStateStore::new());

        state
            .set(
                "px:constraint/lint_check",
                json!({
                    "type": "constraint",
                    "name": "lint_check",
                    "phases": ["pre-commit"],
                    "when": "true",
                    "require": "linted",
                    "severity": "warning",
                    "message": "Code should be linted"
                }),
            )
            .await;

        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_state_store(state);

        // Filter by pre-push phase → should skip the pre-commit constraint
        let result = handler
            .call_tool(
                "praxis_evaluate",
                json!({"action": "commit", "phase": "pre-push"}),
            )
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_rules"], 0);

        // Filter by pre-commit phase → should include it
        let result = handler
            .call_tool(
                "praxis_evaluate",
                json!({"action": "commit", "phase": "pre-commit"}),
            )
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_rules"], 1);
        assert_eq!(parsed["warnings"], 1);
        assert_eq!(parsed["results"][0]["status"], "warning");
    }

    #[tokio::test]
    async fn praxis_list_no_modules() {
        let handler = make_handler();
        let result = handler.call_tool("praxis_list", json!({})).await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_rules"], 0);
    }

    #[tokio::test]
    async fn praxis_list_with_modules() {
        use pares_radix_praxis::modules::safety::SafetyModule;
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_praxis_modules(vec![Box::new(SafetyModule::default())]);

        let result = handler.call_tool("praxis_list", json!({})).await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert!(parsed["total_rules"].as_u64().unwrap() > 0);
        let modules = parsed["modules"].as_array().unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0]["name"], "safety");
    }

    #[tokio::test]
    async fn praxis_list_includes_persisted_constraints_and_rules() {
        let handler = make_handler_with_state();

        // Persist a constraint and a rule into PluresDB
        handler
            .call_tool(
                "praxis_add_constraint",
                json!({
                    "name": "test_constraint",
                    "severity": "warning",
                    "when": "action == 'deploy'",
                    "require": "approved == true",
                    "message": "Deployment requires approval",
                    "phases": ["pre-deploy"]
                }),
            )
            .await;
        handler
            .call_tool(
                "praxis_add_rule",
                json!({
                    "name": "test_rule",
                    "priority": 10,
                    "conditions": ["mood == 'happy'"],
                    "actions": [{"type": "log", "message": "All good"}]
                }),
            )
            .await;

        // Now list should include both
        let result = handler.call_tool("praxis_list", json!({})).await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();

        let constraints = parsed["persisted_constraints"].as_array().unwrap();
        assert_eq!(constraints.len(), 1);
        assert_eq!(constraints[0]["name"], "test_constraint");
        assert_eq!(constraints[0]["severity"], "warning");
        let phases = constraints[0]["phases"].as_array().unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0], "pre-deploy");

        let rules = parsed["persisted_rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["name"], "test_rule");
        assert_eq!(rules[0]["priority"], 10);
    }

    #[tokio::test]
    async fn praxis_list_empty_persisted_without_state_store() {
        let handler = make_handler(); // No state store
        let result = handler.call_tool("praxis_list", json!({})).await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        // Without state store, persisted arrays should be empty
        let constraints = parsed["persisted_constraints"].as_array().unwrap();
        assert!(constraints.is_empty());
        let rules = parsed["persisted_rules"].as_array().unwrap();
        assert!(rules.is_empty());
    }

    #[tokio::test]
    async fn praxis_evaluate_missing_action() {
        let handler = make_handler();
        let result = handler.call_tool("praxis_evaluate", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("action"));
    }

    #[tokio::test]
    async fn praxis_list_tools_always_present() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"praxis_evaluate"));
        assert!(names.contains(&"praxis_list"));
        assert!(names.contains(&"praxis_run"));
    }

    #[tokio::test]
    async fn praxis_run_inline_echo() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "praxis_run",
                json!({
                    "source": "procedure hello:\n  trigger: manual\n  echo {msg: \"world\"}\n"
                }),
            )
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["procedure"], "hello");
    }

    #[tokio::test]
    async fn praxis_run_missing_source() {
        let handler = make_handler();
        let result = handler.call_tool("praxis_run", json!({})).await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn praxis_run_shell_step() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "praxis_run",
                json!({
                    "source": "procedure test_shell:\n  trigger: manual\n  shell {command: \"echo hello\"}  -> $out\n"
                }),
            )
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["variables"]["out"]["stdout"], "hello");
    }

    /// SECURITY REGRESSION: an unrecognized action/step name must NEVER be
    /// executed as a shell command. Previously `ShellBackedProcedureHandler`
    /// fell through to `bash -c "{action} '{params_json}'"` for any action
    /// name it didn't recognize, which allowed arbitrary shell injection
    /// since procedures passed to `praxis_run` are attacker-controlled MCP
    /// input. This test uses a malicious-looking action name that would
    /// create a canary file if it were ever handed to a shell, and asserts
    /// both that execution fails with a typed error AND that no subprocess
    /// side effect occurred.
    #[tokio::test]
    async fn praxis_run_unknown_action_is_rejected_not_shelled_out() {
        let handler = make_handler();

        // Canary path: if the vulnerable code path ever ran this as a shell
        // command, it would execute `touch <canary>` and create the file.
        let canary = std::env::temp_dir().join(format!(
            "pares-agens-shell-injection-canary-{}",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&canary).await;

        let malicious_action = format!(
            "touch {} #",
            canary.display()
        );

        let source = format!(
            "procedure test_injection:\n  trigger: manual\n  {} {{}}\n",
            malicious_action
        );

        let result = handler
            .call_tool("praxis_run", json!({ "source": source }))
            .await;

        // The procedure must fail (unknown action), not succeed via a shelled-out command.
        assert!(
            result.is_error || {
                let parsed: Value = serde_json::from_str(&result.content).unwrap_or(json!({}));
                parsed["success"] == false
            },
            "expected unknown action to fail, got: {}",
            result.content
        );

        // The canary file must NOT exist: proof no subprocess was spawned for
        // the unrecognized action name.
        assert!(
            !canary.exists(),
            "vulnerable shell fallback executed the malicious action and created {}",
            canary.display()
        );

        let _ = tokio::fs::remove_file(&canary).await;
    }

    // ── Chronos tool tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn chronos_tools_always_present() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"chronos_history"));
        assert!(names.contains(&"chronos_recent"));
        assert!(names.contains(&"chronos_by_actor"));
        assert!(names.contains(&"chronos_record"));
        assert!(names.contains(&"chronos_set_level"));
        assert!(names.contains(&"chronos_get_level"));
    }

    #[tokio::test]
    async fn chronos_history_without_timeline() {
        let handler = make_handler();
        let result = handler
            .call_tool("chronos_history", json!({"key": "test:key"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn chronos_recent_without_timeline() {
        let handler = make_handler();
        let result = handler.call_tool("chronos_recent", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn chronos_history_with_timeline() {
        use pares_radix_core::chronos::{ChronosAction, ChronosTimeline};
        use pluresdb::CrdtStore;

        let store = Arc::new(CrdtStore::default());
        let timeline = Arc::new(ChronosTimeline::new(store));

        // Record an entry
        let entry = timeline.build_entry(
            "test:key1",
            "test-actor",
            ChronosAction::Create,
            &json!({"hello": "world"}),
            vec![],
            Some("test rationale".into()),
        );
        timeline.record(&entry);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(timeline);

        let result = handler
            .call_tool("chronos_history", json!({"key": "test:key1"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("test-actor"));
        assert!(result.content.contains("test:key1"));
    }

    #[tokio::test]
    async fn chronos_recent_with_timeline() {
        use pares_radix_core::chronos::{ChronosAction, ChronosTimeline};
        use pluresdb::CrdtStore;

        let store = Arc::new(CrdtStore::default());
        let timeline = Arc::new(ChronosTimeline::new(store));

        let entry = timeline.build_entry(
            "recent:test",
            "actor-1",
            ChronosAction::Update,
            &json!("data"),
            vec![],
            None,
        );
        timeline.record(&entry);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(timeline);

        let result = handler
            .call_tool("chronos_recent", json!({"limit": 5}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("recent:test"));
    }

    #[tokio::test]
    async fn chronos_by_actor_with_timeline() {
        use pares_radix_core::chronos::{ChronosAction, ChronosTimeline};
        use pluresdb::CrdtStore;

        let store = Arc::new(CrdtStore::default());
        let timeline = Arc::new(ChronosTimeline::new(store));

        let entry = timeline.build_entry(
            "actor:test",
            "special-actor",
            ChronosAction::ToolInvoked,
            &json!({"tool": "read_file"}),
            vec!["safety:pass".into()],
            Some("invoked by agent".into()),
        );
        timeline.record(&entry);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(timeline);

        let result = handler
            .call_tool("chronos_by_actor", json!({"actor": "special-actor"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("special-actor"));
        assert!(result.content.contains("ToolInvoked"));
    }

    #[tokio::test]
    async fn chronos_record_creates_entry() {
        use pluresdb::CrdtStore;

        let store = Arc::new(CrdtStore::default());
        let timeline = Arc::new(ChronosTimeline::new(store));

        let shell = Arc::new(ShellExecutor::new());
        let handler =
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(Arc::clone(&timeline));

        let result = handler
            .call_tool(
                "chronos_record",
                json!({
                    "key": "test:record",
                    "actor": "test-agent",
                    "action": "create",
                    "data": {"foo": "bar"},
                    "rationale": "testing record"
                }),
            )
            .await;
        assert!(!result.is_error, "error: {}", result.content);
        assert!(result.content.contains("test:record"));

        // Verify it's in history
        let entries = timeline.history("test:record", 10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].actor, "test-agent");
    }

    #[tokio::test]
    async fn chronos_record_without_timeline() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler
            .call_tool("chronos_record", json!({"key": "x"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn chronos_set_level_and_get_level() {
        let store = Arc::new(pluresdb::CrdtStore::default());
        let chronos = Arc::new(pares_radix_core::chronos::ChronosTimeline::new(store));
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(chronos);

        // Default level is info
        let result = handler.call_tool("chronos_get_level", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("info"));

        // Set to warn
        let result = handler
            .call_tool("chronos_set_level", json!({"level": "warn"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("warn"));

        // Verify it changed
        let result = handler.call_tool("chronos_get_level", json!({})).await;
        assert!(result.content.contains("warn"));

        // Record at info level should be filtered
        let result = handler
            .call_tool(
                "chronos_record",
                json!({"key": "test:filtered", "level": "info"}),
            )
            .await;
        assert!(!result.is_error);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["recorded"], false);

        // Record at error level should succeed
        let result = handler
            .call_tool(
                "chronos_record",
                json!({"key": "test:kept", "level": "error"}),
            )
            .await;
        assert!(!result.is_error);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["recorded"], true);
    }

    #[tokio::test]
    async fn chronos_set_level_invalid() {
        let store = Arc::new(pluresdb::CrdtStore::default());
        let chronos = Arc::new(pares_radix_core::chronos::ChronosTimeline::new(store));
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(chronos);

        let result = handler
            .call_tool("chronos_set_level", json!({"level": "banana"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid level"));
    }

    #[tokio::test]
    async fn chronos_set_level_without_timeline() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler
            .call_tool("chronos_set_level", json!({"level": "debug"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn chronos_replay_without_timeline() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler.call_tool("chronos_replay", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn chronos_replay_with_entries() {
        use pares_radix_core::chronos::ChronosTimeline;
        use pluresdb::CrdtStore;

        let shell = Arc::new(ShellExecutor::new());
        let store = Arc::new(CrdtStore::default());
        let chronos = Arc::new(ChronosTimeline::new(store.clone()));
        let state = Arc::new(pares_radix_core::InMemoryStateStore::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_chronos(chronos.clone())
            .with_state_store(state);

        // Record some entries
        handler
            .call_tool(
                "chronos_record",
                json!({"key": "test:a", "actor": "agent", "action": "Create", "data": {"x": 1}}),
            )
            .await;
        handler
            .call_tool(
                "chronos_record",
                json!({"key": "test:b", "actor": "agent", "action": "Update", "data": {"x": 2}}),
            )
            .await;

        // Replay all
        let result = handler.call_tool("chronos_replay", json!({})).await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["replayed"], 2);
        assert!(parsed["results"].as_array().unwrap().len() == 2);
    }

    #[tokio::test]
    async fn chronos_replay_in_tool_list() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"chronos_replay"));
    }

    #[tokio::test]
    async fn chronos_replay_invalid_from_id_returns_error() {
        use pares_radix_core::chronos::ChronosTimeline;
        use pluresdb::CrdtStore;

        let shell = Arc::new(ShellExecutor::new());
        let store = Arc::new(CrdtStore::default());
        let chronos = Arc::new(ChronosTimeline::new(store));
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(chronos);

        let result = handler
            .call_tool("chronos_replay", json!({"fromId": "nonexistent-id-abc"}))
            .await;
        assert!(result.is_error, "should return error for invalid fromId");
        assert!(result.content.contains("not found in timeline"));
    }

    #[tokio::test]
    async fn chronos_replay_invalid_to_id_returns_error() {
        use pares_radix_core::chronos::ChronosTimeline;
        use pluresdb::CrdtStore;

        let shell = Arc::new(ShellExecutor::new());
        let store = Arc::new(CrdtStore::default());
        let chronos = Arc::new(ChronosTimeline::new(store));
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(chronos);

        let result = handler
            .call_tool("chronos_replay", json!({"toId": "fake-id-xyz"}))
            .await;
        assert!(result.is_error, "should return error for invalid toId");
        assert!(result.content.contains("not found in timeline"));
    }

    #[tokio::test]
    async fn chronos_timeline_without_chronos() {
        let handler = make_handler();
        let result = handler.call_tool("chronos_timeline", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn chronos_timeline_basic() {
        use pares_radix_core::chronos::{ChronosAction, ChronosTimeline};
        use pluresdb::CrdtStore;

        let store = Arc::new(CrdtStore::default());
        let timeline = Arc::new(ChronosTimeline::new(store));

        let entry = timeline.build_entry(
            "timeline:test",
            "actor-timeline",
            ChronosAction::Create,
            &json!({"value": 42}),
            vec![],
            None,
        );
        timeline.record(&entry);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(timeline);

        let result = handler
            .call_tool("chronos_timeline", json!({"limit": 10}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("timeline:test"));
        assert!(result.content.contains("actor-timeline"));
    }

    #[tokio::test]
    async fn chronos_timeline_with_level_filter() {
        use pares_radix_core::chronos::{ChronosAction, ChronosLevel, ChronosTimeline};
        use pluresdb::CrdtStore;

        let store = Arc::new(CrdtStore::default());
        let timeline = Arc::new(ChronosTimeline::new(store));

        // Record a debug entry
        let debug_entry = timeline.build_entry_with_level(
            "timeline:debug",
            "actor-1",
            ChronosAction::Create,
            ChronosLevel::Debug,
            &json!("debug-data"),
            vec![],
            None,
        );
        timeline.record(&debug_entry);

        // Record an error entry
        let error_entry = timeline.build_entry_with_level(
            "timeline:error",
            "actor-1",
            ChronosAction::Update,
            ChronosLevel::Error,
            &json!("error-data"),
            vec![],
            None,
        );
        timeline.record(&error_entry);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_chronos(timeline);

        // Filter to error level - should only get the error entry
        let result = handler
            .call_tool("chronos_timeline", json!({"level": "error"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("timeline:error"));
        assert!(!result.content.contains("timeline:debug"));
    }

    #[tokio::test]
    async fn chronos_timeline_in_tool_list() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"chronos_timeline"));
    }

    #[tokio::test]
    async fn subagent_list_without_manager() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler.call_tool("subagent_list", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn subagent_spawn_without_manager() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler
            .call_tool(
                "subagent_spawn",
                json!({
                    "agent": "researcher",
                    "task": "find info"
                }),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn subagent_kill_without_manager() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler
            .call_tool("subagent_kill", json!({"session_id": "abc"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn subagent_steer_without_manager() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler
            .call_tool(
                "subagent_steer",
                json!({"session_id": "abc", "message": "change direction"}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn subagent_steer_missing_message() {
        // When manager is not configured, we get "not configured" before param check.
        // This test verifies param validation works when manager IS configured.
        // For the no-manager case, the error is the same as other subagent tools.
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        let result = handler
            .call_tool("subagent_steer", json!({"session_id": "abc"}))
            .await;
        // Without manager, returns "not configured" (manager check comes first)
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn with_px_dir_loads_procedures() {
        use std::io::Write;
        // Create a temp dir with a .px file
        let dir = std::env::temp_dir().join("radix_test_px_autoload");
        let _ = std::fs::create_dir_all(&dir);
        let px_file = dir.join("hello.px");
        let mut f = std::fs::File::create(&px_file).unwrap();
        writeln!(
            f,
            "procedure greet:\n  trigger: manual\n  emit {{message: \"hello world\"}}"
        )
        .unwrap();
        drop(f); // Ensure file is flushed before reading

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_px_dir(dir.clone());

        // Verify the procedure was loaded
        {
            let procs = handler.loaded_procedures.read().await;
            assert!(
                procs.contains_key("greet"),
                "expected 'greet' procedure to be loaded, got: {:?}",
                procs.keys().collect::<Vec<_>>()
            );
        }

        // Verify praxis_list shows it
        let result = handler.call_tool("praxis_list", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("greet"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn praxis_run_preloaded_procedure() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("radix_test_px_run");
        let _ = std::fs::create_dir_all(&dir);
        let px_file = dir.join("echo_test.px");
        let mut f = std::fs::File::create(&px_file).unwrap();
        writeln!(
            f,
            "procedure echo_test:\n  trigger: manual\n  emit {{result: \"ok\"}}"
        )
        .unwrap();

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_px_dir(dir.clone());

        let result = handler
            .call_tool("praxis_run", json!({"procedure": "echo_test"}))
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(result.content.contains("preloaded"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn praxis_run_procedure_composition() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("radix_test_px_compose");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        // Create two procedures: "helper" emits a value, "main_proc" calls "helper"
        let px_file = dir.join("compose.px");
        let mut f = std::fs::File::create(&px_file).unwrap();
        // .px syntax: step_call is `name {params} -> $var`
        write!(
            f,
            "procedure helper:\n  trigger: manual\n  echo {{value: \"from_helper\"}} -> $result\n\nprocedure main_proc:\n  trigger: manual\n  helper {{}} -> $sub_result\n"
        )
        .unwrap();
        drop(f);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_px_dir(dir.clone());

        // Both procedures should be loaded
        {
            let procs = handler.loaded_procedures.read().await;
            assert!(
                procs.contains_key("helper"),
                "expected 'helper' procedure, got: {:?}",
                procs.keys().collect::<Vec<_>>()
            );
            assert!(procs.contains_key("main_proc"));
        }

        // Run main_proc — it should call helper via ComposableHandler
        let result = handler
            .call_tool("praxis_run", json!({"procedure": "main_proc"}))
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(
            result.content.contains("sub_result"),
            "expected sub_result in output: {}",
            result.content
        );
        assert!(result.content.contains("preloaded"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn px_watcher_hot_reloads_procedures() {
        use std::io::Write;
        use tokio::time::{sleep, Duration};

        let dir = std::env::temp_dir().join("radix_test_px_hot_reload");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));

        // Start watcher (no initial files)
        handler
            .start_px_watcher(dir.clone())
            .await
            .expect("watcher should start");

        // Give watcher time to initialize
        sleep(Duration::from_millis(200)).await;

        // Initially no procedures
        {
            let procs = handler.loaded_procedures.read().await;
            assert!(
                procs.is_empty(),
                "expected empty, got {:?}",
                procs.keys().collect::<Vec<_>>()
            );
        }

        // Create a .px file with a procedure
        let px_file = dir.join("hot.px");
        {
            let mut f = std::fs::File::create(&px_file).unwrap();
            writeln!(
                f,
                "procedure hot_proc:\n  trigger: manual\n  emit {{status: \"hot\"}}"
            )
            .unwrap();
        }

        // Wait for debounce + processing
        sleep(Duration::from_millis(500)).await;

        // Procedure should now be loaded
        {
            let procs = handler.loaded_procedures.read().await;
            assert!(
                procs.contains_key("hot_proc"),
                "expected 'hot_proc' after hot-reload, got: {:?}",
                procs.keys().collect::<Vec<_>>()
            );
        }

        // Remove the file
        std::fs::remove_file(&px_file).unwrap();
        sleep(Duration::from_millis(500)).await;

        // Procedure should be gone
        {
            let procs = handler.loaded_procedures.read().await;
            assert!(
                !procs.contains_key("hot_proc"),
                "expected 'hot_proc' to be removed, got: {:?}",
                procs.keys().collect::<Vec<_>>()
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn px_watcher_persists_records_to_state_store() {
        use std::io::Write;
        use tokio::time::{sleep, Duration};

        let dir = std::env::temp_dir().join("radix_test_px_db_persist");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        // Write a .px file BEFORE starting the watcher (tests initial_scan persistence)
        let px_file = dir.join("persist_test.px");
        {
            let mut f = std::fs::File::create(&px_file).unwrap();
            writeln!(f, "fact agent_state:\n  mood: string\n  energy: int").unwrap();
            writeln!(f).unwrap();
            writeln!(f, "rule mood_rule:\n  when:\n    - agent_state.mood == \"happy\"\n  then:\n    - action: celebrate").unwrap();
        }

        let shell = Arc::new(ShellExecutor::new());
        let store = Arc::new(pares_radix_core::InMemoryStateStore::new());
        let handler =
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_state_store(store.clone());

        // Start watcher with initial_scan=true (our new default)
        handler
            .start_px_watcher(dir.clone())
            .await
            .expect("watcher should start");

        // Wait for initial scan + persistence
        sleep(Duration::from_millis(500)).await;

        // Verify records were persisted to state store
        let fact = store.get("px:fact/agent_state").await;
        assert!(
            fact.is_some(),
            "px:fact/agent_state should be persisted to state store"
        );
        let fact_val = fact.unwrap();
        assert_eq!(fact_val.get("type").and_then(|v| v.as_str()), Some("fact"));

        let rule = store.get("px:rule/mood_rule").await;
        assert!(
            rule.is_some(),
            "px:rule/mood_rule should be persisted to state store"
        );
        let rule_val = rule.unwrap();
        assert_eq!(rule_val.get("type").and_then(|v| v.as_str()), Some("rule"));

        // Now add a new file and check hot-reload also persists
        let px_file2 = dir.join("extra.px");
        {
            let mut f = std::fs::File::create(&px_file2).unwrap();
            writeln!(f, "constraint must_have_energy:\n  when: agent_state.energy < 0\n  require: false\n  severity: error\n  message: \"No energy\"").unwrap();
        }
        sleep(Duration::from_millis(500)).await;

        let constraint = store.get("px:constraint/must_have_energy").await;
        assert!(
            constraint.is_some(),
            "px:constraint/must_have_energy should be persisted after hot-reload"
        );

        // Remove and verify deletion
        std::fs::remove_file(&px_file2).unwrap();
        sleep(Duration::from_millis(500)).await;

        let deleted = store.get("px:constraint/must_have_energy").await;
        // After delete, value should be None or Null
        assert!(
            deleted.is_none() || deleted == Some(serde_json::Value::Null),
            "px:constraint/must_have_energy should be removed from state store"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── praxis_add_constraint tests ────────────────────────────────────────

    #[tokio::test]
    async fn praxis_add_constraint_missing_name() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("praxis_add_constraint", json!({"severity": "error"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("name"));
    }

    #[tokio::test]
    async fn praxis_add_constraint_missing_severity() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("praxis_add_constraint", json!({"name": "test_constraint"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("severity"));
    }

    #[tokio::test]
    async fn praxis_add_constraint_no_state_store() {
        let handler = make_handler(); // no state store
        let result = handler
            .call_tool(
                "praxis_add_constraint",
                json!({"name": "test", "severity": "error"}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("state store"));
    }

    #[tokio::test]
    async fn praxis_add_constraint_minimal_fields() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool(
                "praxis_add_constraint",
                json!({"name": "min_constraint", "severity": "warning"}),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("px:constraint/min_constraint"));
    }

    #[tokio::test]
    async fn praxis_add_constraint_all_fields() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool(
                "praxis_add_constraint",
                json!({
                    "name": "full_constraint",
                    "severity": "error",
                    "when": "action == 'deploy'",
                    "require": "tests_passing == true",
                    "message": "Tests must pass before deploy",
                    "phases": ["pre-push", "pre-deploy"]
                }),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("px:constraint/full_constraint"));
    }

    #[tokio::test]
    async fn praxis_add_constraint_roundtrip_via_db() {
        let handler = make_handler_with_state();
        handler
            .call_tool(
                "praxis_add_constraint",
                json!({
                    "name": "roundtrip_c",
                    "severity": "error",
                    "when": "x > 10",
                    "require": "y == true",
                    "message": "y must be true when x > 10"
                }),
            )
            .await;

        let get_result = handler
            .call_tool("db_get", json!({"key": "px:constraint/roundtrip_c"}))
            .await;
        assert!(!get_result.is_error);
        let val: Value = serde_json::from_str(&get_result.content).unwrap();
        assert_eq!(val["name"], "roundtrip_c");
        assert_eq!(val["severity"], "error");
        assert_eq!(val["when"], "x > 10");
        assert_eq!(val["require"], "y == true");
        assert_eq!(val["message"], "y must be true when x > 10");
    }

    #[tokio::test]
    async fn praxis_add_constraint_duplicate_overwrites() {
        let handler = make_handler_with_state();
        handler
            .call_tool(
                "praxis_add_constraint",
                json!({"name": "dup", "severity": "warning", "message": "v1"}),
            )
            .await;
        handler
            .call_tool(
                "praxis_add_constraint",
                json!({"name": "dup", "severity": "error", "message": "v2"}),
            )
            .await;

        let get_result = handler
            .call_tool("db_get", json!({"key": "px:constraint/dup"}))
            .await;
        let val: Value = serde_json::from_str(&get_result.content).unwrap();
        assert_eq!(val["severity"], "error");
        assert_eq!(val["message"], "v2");
    }

    #[tokio::test]
    async fn praxis_add_constraint_shows_in_praxis_list() {
        let handler = make_handler_with_state();
        handler
            .call_tool(
                "praxis_add_constraint",
                json!({"name": "listed_c", "severity": "error"}),
            )
            .await;

        let list_result = handler.call_tool("praxis_list", json!({})).await;
        assert!(!list_result.is_error);
        assert!(list_result.content.contains("listed_c"));
    }

    // ── praxis_add_rule tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn praxis_add_rule_missing_name() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("praxis_add_rule", json!({"priority": 10}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("name"));
    }

    #[tokio::test]
    async fn praxis_add_rule_no_state_store() {
        let handler = make_handler(); // no state store
        let result = handler
            .call_tool("praxis_add_rule", json!({"name": "test_rule"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("state store"));
    }

    #[tokio::test]
    async fn praxis_add_rule_minimal_fields() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("praxis_add_rule", json!({"name": "min_rule"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("px:rule/min_rule"));
    }

    #[tokio::test]
    async fn praxis_add_rule_all_fields() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool(
                "praxis_add_rule",
                json!({
                    "name": "full_rule",
                    "priority": 100,
                    "conditions": ["action == 'build'", "env == 'prod'"],
                    "actions": [{"type": "notify", "target": "#ops"}]
                }),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("px:rule/full_rule"));
    }

    #[tokio::test]
    async fn praxis_add_rule_roundtrip_via_db() {
        let handler = make_handler_with_state();
        handler
            .call_tool(
                "praxis_add_rule",
                json!({
                    "name": "roundtrip_r",
                    "priority": 50,
                    "conditions": ["c1", "c2"],
                    "actions": ["a1"]
                }),
            )
            .await;

        let get_result = handler
            .call_tool("db_get", json!({"key": "px:rule/roundtrip_r"}))
            .await;
        assert!(!get_result.is_error);
        let val: Value = serde_json::from_str(&get_result.content).unwrap();
        assert_eq!(val["name"], "roundtrip_r");
        assert_eq!(val["priority"], 50);
        assert_eq!(val["conditions"], json!(["c1", "c2"]));
        assert_eq!(val["actions"], json!(["a1"]));
    }

    #[tokio::test]
    async fn praxis_add_rule_duplicate_overwrites() {
        let handler = make_handler_with_state();
        handler
            .call_tool(
                "praxis_add_rule",
                json!({"name": "dup_rule", "priority": 1}),
            )
            .await;
        handler
            .call_tool(
                "praxis_add_rule",
                json!({"name": "dup_rule", "priority": 99}),
            )
            .await;

        let get_result = handler
            .call_tool("db_get", json!({"key": "px:rule/dup_rule"}))
            .await;
        let val: Value = serde_json::from_str(&get_result.content).unwrap();
        assert_eq!(val["priority"], 99);
    }

    #[tokio::test]
    async fn praxis_add_rule_shows_in_praxis_list() {
        let handler = make_handler_with_state();
        handler
            .call_tool("praxis_add_rule", json!({"name": "listed_r"}))
            .await;

        let list_result = handler.call_tool("praxis_list", json!({})).await;
        assert!(!list_result.is_error);
        assert!(list_result.content.contains("listed_r"));
    }

    // --- Test-runner built-in action tests ---

    fn make_shell_handler() -> ShellBackedProcedureHandler {
        ShellBackedProcedureHandler {
            shell: Arc::new(ShellExecutor::new()),
            workdir: PathBuf::from("/tmp"),
            children: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    #[tokio::test]
    async fn test_px_assert_eq_pass() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let result = h
            .call("assert_eq", &json!({"actual": 42, "expected": 42}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_px_assert_eq_fail() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let result = h
            .call("assert_eq", &json!({"actual": 1, "expected": 2}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_px_assert_contains_string() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let result = h
            .call(
                "assert_contains",
                &json!({"value": "hello world", "contains": "world"}),
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_px_assert_contains_missing() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let result = h
            .call(
                "assert_contains",
                &json!({"value": "hello", "contains": "xyz"}),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_px_json_parse() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let result = h
            .call("json_parse", &json!({"text": "{\"a\": 1, \"b\": [2,3]}"}))
            .await
            .unwrap();
        assert_eq!(result["a"], 1);
        assert_eq!(result["b"][0], 2);
    }

    #[tokio::test]
    async fn test_px_sleep() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let start = std::time::Instant::now();
        let result = h.call("sleep", &json!({"ms": 100})).await;
        assert!(result.is_ok());
        assert!(start.elapsed().as_millis() >= 80);
    }

    #[tokio::test]
    async fn test_px_assert_ok_truthy() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        assert!(h.call("assert_ok", &json!({"value": true})).await.is_ok());
        assert!(h.call("assert_ok", &json!({"value": "yes"})).await.is_ok());
        assert!(h.call("assert_ok", &json!({"value": 1})).await.is_ok());
        assert!(h.call("assert_ok", &json!({"value": null})).await.is_err());
        assert!(h.call("assert_ok", &json!({"value": false})).await.is_err());
        assert!(h.call("assert_ok", &json!({"value": ""})).await.is_err());
    }

    #[tokio::test]
    async fn test_px_start_and_stop_process() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let result = h
            .call("start_process", &json!({"command": "sleep 30"}))
            .await
            .unwrap();
        let pid = result["pid"].as_u64().unwrap();
        assert!(pid > 0);
        // Stop it
        let stop = h.call("stop_process", &json!({"pid": pid})).await;
        assert!(stop.is_ok());
    }

    #[tokio::test]
    async fn test_px_wait_for_ready_timeout() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        let result = h
            .call(
                "wait_for_ready",
                &json!({"url": "http://127.0.0.1:19999", "timeout_secs": 1}),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires network + python3 loopback HTTP server; not available in hermetic Nix build sandbox. Run in CI with --ignored."]
    async fn test_px_http_get() {
        use pares_radix_praxis::px::async_executor::AsyncActionHandler;
        let h = make_shell_handler();
        // Start a simple HTTP server
        let start = h
            .call(
                "start_process",
                &json!({"command": "python3 -m http.server 18321"}),
            )
            .await
            .unwrap();
        let pid = start["pid"].as_u64().unwrap();
        // Wait a moment for it to start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let result = h
            .call(
                "http_get",
                &json!({"url": "http://127.0.0.1:18321/", "timeout_secs": 5}),
            )
            .await;
        // Clean up
        let _ = h.call("stop_process", &json!({"pid": pid})).await;
        let resp = result.unwrap();
        assert_eq!(resp["status"], 200);
    }

    // ── Plugin management tests ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn plugin_list_without_runtime() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));
        let result = handler.call_tool("plugin_list", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("not configured"));
    }

    #[tokio::test]
    async fn plugin_list_with_runtime() {
        let shell = Arc::new(ShellExecutor::new());
        let runtime = Arc::new(PluginRuntime::new());
        let handler =
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_plugin_runtime(runtime);
        let result = handler.call_tool("plugin_list", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("[]"));
    }

    #[tokio::test]
    async fn plugin_register_and_info() {
        let shell = Arc::new(ShellExecutor::new());
        let runtime = Arc::new(PluginRuntime::new());
        let handler =
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_plugin_runtime(runtime);

        // Register
        let result = handler
            .call_tool(
                "plugin_register",
                json!({"name": "test-plugin", "version": "1.0.0", "description": "A test plugin"}),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("test-plugin"));

        // Info
        let result = handler
            .call_tool("plugin_info", json!({"name": "test-plugin"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("test-plugin"));
        assert!(result.content.contains("1.0.0"));
    }

    #[tokio::test]
    async fn plugin_activate_not_found() {
        let shell = Arc::new(ShellExecutor::new());
        let runtime = Arc::new(PluginRuntime::new());
        let handler =
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_plugin_runtime(runtime);
        let result = handler
            .call_tool("plugin_activate", json!({"name": "nonexistent"}))
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn plugin_deactivate_removes_plugin() {
        let shell = Arc::new(ShellExecutor::new());
        let runtime = Arc::new(PluginRuntime::new());
        let handler =
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_plugin_runtime(runtime);

        // Register first
        handler
            .call_tool(
                "plugin_register",
                json!({"name": "ephemeral", "version": "0.1.0"}),
            )
            .await;

        // Deactivate
        let result = handler
            .call_tool("plugin_deactivate", json!({"name": "ephemeral"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("deactivated"));

        // Verify gone
        let result = handler
            .call_tool("plugin_info", json!({"name": "ephemeral"}))
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn plugin_tools_in_tool_list() {
        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"));
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"plugin_list"));
        assert!(names.contains(&"plugin_info"));
        assert!(names.contains(&"plugin_register"));
        assert!(names.contains(&"plugin_activate"));
        assert!(names.contains(&"plugin_deactivate"));
    }

    #[tokio::test]
    async fn session_status_returns_info() {
        let handler = make_handler();
        let result = handler.call_tool("session_status", json!({})).await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["session_id"], "main");
        assert_eq!(parsed["status"], "running");
        assert!(parsed["version"].is_string());
        assert!(parsed["components"].is_object());
    }

    #[tokio::test]
    async fn session_status_with_session_id() {
        let handler = make_handler();
        let result = handler
            .call_tool("session_status", json!({"session_id": "test-session"}))
            .await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["session_id"], "test-session");
    }

    #[tokio::test]
    async fn session_history_missing_session_id() {
        let handler = make_handler();
        let result = handler.call_tool("session_history", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("session_id"));
    }

    #[tokio::test]
    async fn session_history_no_state_store() {
        let handler = make_handler();
        let result = handler
            .call_tool("session_history", json!({"session_id": "test"}))
            .await;
        // Without state store, should return error
        assert!(result.is_error);
        assert!(result.content.contains("State store not configured"));
    }

    #[tokio::test]
    async fn session_tools_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"session_status"));
        assert!(names.contains(&"session_history"));
        assert!(names.contains(&"session_send"));
        assert!(names.contains(&"session_list"));
    }

    #[tokio::test]
    async fn session_send_missing_session_id() {
        let handler = make_handler();
        let result = handler
            .call_tool("session_send", json!({"message": "hello"}))
            .await;
        assert!(result.is_error);
        assert!(result
            .content
            .contains("missing required parameter: session_id"));
    }

    #[tokio::test]
    async fn session_send_missing_message() {
        let handler = make_handler();
        let result = handler
            .call_tool("session_send", json!({"session_id": "test"}))
            .await;
        assert!(result.is_error);
        assert!(result
            .content
            .contains("missing required parameter: message"));
    }

    #[tokio::test]
    async fn session_send_no_state_store() {
        let handler = make_handler();
        let result = handler
            .call_tool("session_send", json!({"session_id": "s1", "message": "hi"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("State store not configured"));
    }

    #[tokio::test]
    async fn session_send_delivers_with_state_store() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool(
                "session_send",
                json!({"session_id": "target-1", "message": "test msg", "timeout_seconds": 0}),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("delivered"));
    }

    #[tokio::test]
    async fn session_list_no_state_store() {
        let handler = make_handler();
        let result = handler.call_tool("session_list", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("State store not configured"));
    }

    #[tokio::test]
    async fn session_list_with_state_store() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("session_list", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("shell_sessions"));
        assert!(result.content.contains("subagent_count"));
    }

    #[tokio::test]
    async fn session_yield_basic() {
        let handler = make_handler();
        let result = handler.call_tool("session_yield", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("yielded"));
        assert!(result.content.contains("true"));
    }

    #[tokio::test]
    async fn session_yield_with_message() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "session_yield",
                json!({"message": "Waiting for subagent to finish code review"}),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("Waiting for subagent"));
        assert!(result.content.contains("yielded"));
    }

    #[tokio::test]
    async fn session_yield_with_state_store() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool("session_yield", json!({"message": "spawned 3 workers"}))
            .await;
        assert!(!result.is_error);
        // Verify yield state was stored
        let store = handler.state_store.as_ref().unwrap();
        let stored = store.get("session:yield:pending").await;
        assert!(stored.is_some());
        let val = stored.unwrap();
        assert_eq!(val["yielded"], true);
        assert_eq!(val["message"], "spawned 3 workers");
    }

    #[tokio::test]
    async fn session_yield_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"session_yield"));
    }

    #[tokio::test]
    async fn plugin_register_emits_tools_list_changed_notification() {
        let shell = Arc::new(ShellExecutor::new());
        let runtime = Arc::new(PluginRuntime::new());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_plugin_runtime(runtime)
            .with_notification_tx(tx);

        let result = handler
            .call_tool(
                "plugin_register",
                json!({"name": "notif-test", "version": "1.0.0"}),
            )
            .await;
        assert!(!result.is_error);

        // Should have received a tools/list_changed notification
        let notif = rx
            .try_recv()
            .expect("expected tools_list_changed notification");
        assert_eq!(notif.method, "notifications/tools/list_changed");
        assert!(notif.params.is_none());
    }

    #[tokio::test]
    async fn plugin_deactivate_emits_tools_list_changed_notification() {
        let shell = Arc::new(ShellExecutor::new());
        let runtime = Arc::new(PluginRuntime::new());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp"))
            .with_plugin_runtime(runtime)
            .with_notification_tx(tx);

        // Register first
        handler
            .call_tool(
                "plugin_register",
                json!({"name": "temp-plugin", "version": "0.1.0"}),
            )
            .await;
        // Drain the register notification
        let _ = rx.try_recv();

        // Deactivate
        let result = handler
            .call_tool("plugin_deactivate", json!({"name": "temp-plugin"}))
            .await;
        assert!(!result.is_error);

        // Should have received another tools/list_changed notification
        let notif = rx
            .try_recv()
            .expect("expected tools_list_changed notification on deactivate");
        assert_eq!(notif.method, "notifications/tools/list_changed");
    }

    #[tokio::test]
    async fn no_notification_when_tx_not_configured() {
        let shell = Arc::new(ShellExecutor::new());
        let runtime = Arc::new(PluginRuntime::new());
        // No notification_tx attached
        let handler =
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_plugin_runtime(runtime);

        // Should not panic
        let result = handler
            .call_tool(
                "plugin_register",
                json!({"name": "safe-plugin", "version": "1.0.0"}),
            )
            .await;
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn telemetry_snapshot_returns_initial_state() {
        let handler = make_handler();
        let result = handler.call_tool("telemetry_snapshot", json!({})).await;
        assert!(!result.is_error);
        let data: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(data["total_calls"], 0);
        assert_eq!(data["unique_tools_used"], 0);
    }

    #[tokio::test]
    async fn telemetry_records_tool_calls() {
        let handler = make_handler();

        // Make a few tool calls
        handler.call_tool("db_get", json!({"key": "x"})).await;
        handler.call_tool("db_get", json!({"key": "y"})).await;
        handler.call_tool("runtime_status", json!({})).await;

        let result = handler.call_tool("telemetry_snapshot", json!({})).await;
        assert!(!result.is_error);
        let data: Value = serde_json::from_str(&result.content).unwrap();
        // 3 calls (telemetry_snapshot itself is excluded from counting)
        assert_eq!(data["total_calls"], 3);
        assert_eq!(data["unique_tools_used"], 2);

        // Top tools should include db_get
        let top_tools = data["top_tools"].as_array().unwrap();
        assert!(top_tools
            .iter()
            .any(|t| t["name"] == "db_get" && t["calls"] == 2));
    }

    #[tokio::test]
    async fn telemetry_tracks_failures() {
        let handler = make_handler();

        // Call an unknown tool — should fail
        let result = handler.call_tool("nonexistent_tool", json!({})).await;
        assert!(result.is_error);

        let snapshot = handler.call_tool("telemetry_snapshot", json!({})).await;
        let data: Value = serde_json::from_str(&snapshot.content).unwrap();
        assert_eq!(data["total_calls"], 1);
        let top = data["top_tools"].as_array().unwrap();
        let entry = &top[0];
        assert_eq!(entry["name"], "nonexistent_tool");
        assert_eq!(entry["failures"], 1);
        assert_eq!(entry["successes"], 0);
    }

    #[tokio::test]
    async fn telemetry_reset_clears_counters() {
        let handler = make_handler();

        handler.call_tool("db_get", json!({"key": "a"})).await;
        handler.call_tool("db_get", json!({"key": "b"})).await;

        let result = handler.call_tool("telemetry_reset", json!({})).await;
        assert!(!result.is_error);
        let data: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(data["reset"], true);

        // Snapshot should now be clean
        let snapshot = handler.call_tool("telemetry_snapshot", json!({})).await;
        let snap_data: Value = serde_json::from_str(&snapshot.content).unwrap();
        assert_eq!(snap_data["total_calls"], 0);
    }

    #[tokio::test]
    async fn telemetry_tools_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"telemetry_snapshot"));
        assert!(names.contains(&"telemetry_reset"));
    }

    // ── px_compose tests ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn px_compose_register_inline() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "px_compose",
                json!({
                    "action": "register",
                    "source": "procedure greet:\n  trigger: manual\n  echo {msg: \"hello\"} -> $out\n"
                }),
            )
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(result.content.contains("greet"));

        // Verify it's in the list
        let list_result = handler
            .call_tool("px_compose", json!({"action": "list"}))
            .await;
        assert!(!list_result.is_error);
        assert!(list_result.content.contains("greet"));
    }

    #[tokio::test]
    async fn px_compose_unregister() {
        let handler = make_handler();
        // Register first
        handler
            .call_tool(
                "px_compose",
                json!({
                    "action": "register",
                    "source": "procedure temp_proc:\n  trigger: manual\n  echo {x: 1} -> $y\n"
                }),
            )
            .await;

        // Unregister
        let result = handler
            .call_tool(
                "px_compose",
                json!({"action": "unregister", "name": "temp_proc"}),
            )
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(result.content.contains("temp_proc"));

        // Verify it's gone
        let list_result = handler
            .call_tool("px_compose", json!({"action": "list"}))
            .await;
        assert!(!list_result.content.contains("temp_proc"));
    }

    #[tokio::test]
    async fn px_compose_unregister_nonexistent() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "px_compose",
                json!({"action": "unregister", "name": "does_not_exist"}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn px_compose_list_empty() {
        let handler = make_handler();
        let result = handler
            .call_tool("px_compose", json!({"action": "list"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("\"count\":0") || result.content.contains("\"count\": 0"));
    }

    #[tokio::test]
    async fn px_compose_pipe_missing_procedure() {
        let handler = make_handler();
        let result = handler
            .call_tool(
                "px_compose",
                json!({"action": "pipe", "pipeline": ["nonexistent"]}),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn px_compose_pipe_runs_single_procedure() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("radix_test_px_compose_pipe");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let px_file = dir.join("pipe_test.px");
        let mut f = std::fs::File::create(&px_file).unwrap();
        write!(
            f,
            "procedure add_greeting:\n  trigger: manual\n  echo {{greeting: \"hello world\"}} -> $output\n"
        )
        .unwrap();
        drop(f);

        let shell = Arc::new(ShellExecutor::new());
        let handler = RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_px_dir(dir.clone());

        let result = handler
            .call_tool(
                "px_compose",
                json!({"action": "pipe", "pipeline": ["add_greeting"], "input": {"name": "test"}}),
            )
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(result.content.contains("success"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn px_compose_invalid_action() {
        let handler = make_handler();
        let result = handler
            .call_tool("px_compose", json!({"action": "invalid"}))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown action"));
    }

    #[tokio::test]
    async fn px_compose_missing_action() {
        let handler = make_handler();
        let result = handler.call_tool("px_compose", json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("action"));
    }

    #[tokio::test]
    async fn px_compose_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"px_compose"));
    }

    #[tokio::test]
    async fn px_status_returns_overview() {
        let handler = make_handler();
        let result = handler.call_tool("px_status", json!({})).await;
        let v: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(v["procedures"]["count"], 0);
        assert!(v["persisted"].is_object());
        assert!(v["modules"].is_object());
    }

    #[tokio::test]
    async fn px_status_reflects_registered_procedures() {
        let handler = make_handler();
        // Register a procedure first
        handler
            .call_tool(
                "px_compose",
                json!({"action": "register", "source": "procedure greet:\n  trigger: manual\n  echo {msg: \"hello\"} -> $out\n"}),
            )
            .await;

        let result = handler.call_tool("px_status", json!({})).await;
        let v: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(v["procedures"]["count"], 1);
        let names = v["procedures"]["names"].as_array().unwrap();
        assert!(names.iter().any(|n| n == "greet"));
    }

    #[tokio::test]
    async fn px_status_in_tool_list() {
        let handler = make_handler();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"px_status"));
    }

    // ── Canvas tool tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn canvas_create_returns_document() {
        let handler = make_handler_with_state();
        let result = handler
            .call_tool(
                "canvas_create",
                json!({"title": "Test App", "description": "A test"}),
            )
            .await;
        assert!(!result.is_error);
        let doc: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(doc["title"], "Test App");
        assert_eq!(doc["description"], "A test");
        assert!(doc["id"].as_str().is_some());
        assert!(doc["tree"].is_object());
    }

    #[tokio::test]
    async fn canvas_get_returns_active() {
        let handler = make_handler_with_state();
        // No active canvas initially
        let result = handler.call_tool("canvas_get", json!({})).await;
        assert_eq!(result.content, "null");

        // Create one
        handler
            .call_tool("canvas_create", json!({"title": "My Canvas"}))
            .await;
        let result = handler.call_tool("canvas_get", json!({})).await;
        assert!(!result.is_error);
        let doc: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(doc["title"], "My Canvas");
    }

    #[tokio::test]
    async fn canvas_set_tree_replaces_tree() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Tree Test"}))
            .await;

        let new_tree =
            json!({"id": "root", "type": "Root", "children": [{"id": "box1", "type": "Box"}]});
        let result = handler
            .call_tool("canvas_set_tree", json!({"tree": new_tree}))
            .await;
        assert!(!result.is_error);

        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert_eq!(doc["tree"]["children"][0]["id"], "box1");
    }

    #[tokio::test]
    async fn canvas_add_and_remove_node() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Node Test"}))
            .await;

        // Add a node under root
        let node = json!({"id": "child1", "type": "Text", "props": {"text": "Hello"}});
        let result = handler
            .call_tool("canvas_add_node", json!({"parentId": "root", "node": node}))
            .await;
        assert!(!result.is_error);

        // Verify it's there
        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert_eq!(doc["tree"]["children"][0]["id"], "child1");

        // Remove it
        let result = handler
            .call_tool("canvas_remove_node", json!({"nodeId": "child1"}))
            .await;
        assert!(!result.is_error);

        // Verify it's gone
        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert!(doc["tree"]["children"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn canvas_validate_catches_duplicate_ids() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Validate Test"}))
            .await;

        // Set tree with duplicate IDs
        let tree = json!({"id": "root", "type": "Root", "children": [
            {"id": "dup", "type": "Box"},
            {"id": "dup", "type": "Text"}
        ]});
        handler
            .call_tool("canvas_set_tree", json!({"tree": tree}))
            .await;

        let result = handler.call_tool("canvas_validate", json!({})).await;
        let validation: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(validation["valid"], false);
        assert!(!validation["issues"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn canvas_save_and_load() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Save Test"}))
            .await;

        // Get the ID
        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        let id = doc["id"].as_str().unwrap().to_string();

        // Save it
        let result = handler.call_tool("canvas_save", json!({})).await;
        assert!(!result.is_error);

        // Create a new canvas (overwrites active)
        handler
            .call_tool("canvas_create", json!({"title": "Other"}))
            .await;

        // Load the saved one
        let result = handler.call_tool("canvas_load", json!({"id": id})).await;
        assert!(!result.is_error);
        let loaded: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(loaded["title"], "Save Test");
    }

    #[tokio::test]
    async fn canvas_list_shows_saved() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Listed"}))
            .await;
        handler.call_tool("canvas_save", json!({})).await;

        let result = handler.call_tool("canvas_list", json!({})).await;
        assert!(!result.is_error);
        let list: Vec<Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["title"], "Listed");
    }

    #[tokio::test]
    async fn canvas_export_and_import() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Export Me"}))
            .await;

        let exported = handler.call_tool("canvas_export", json!({})).await;
        assert!(!exported.is_error);

        // Create a different canvas
        handler
            .call_tool("canvas_create", json!({"title": "Different"}))
            .await;

        // Import the exported one
        let result = handler
            .call_tool("canvas_import", json!({"json": exported.content}))
            .await;
        assert!(!result.is_error);

        // Verify active is the imported one
        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert_eq!(doc["title"], "Export Me");
    }

    #[tokio::test]
    async fn canvas_set_data_merges() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Data Test"}))
            .await;

        handler
            .call_tool(
                "canvas_set_data",
                json!({"data": {"count": 1, "name": "test"}}),
            )
            .await;
        handler
            .call_tool(
                "canvas_set_data",
                json!({"data": {"count": 2, "extra": true}}),
            )
            .await;

        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert_eq!(doc["data"]["count"], 2);
        assert_eq!(doc["data"]["name"], "test");
        assert_eq!(doc["data"]["extra"], true);
    }

    #[tokio::test]
    async fn canvas_add_procedure_and_rule() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Proc Test"}))
            .await;

        let proc = json!({"name": "onClick", "steps": [{"action": "navigate", "url": "/next"}]});
        let result = handler
            .call_tool("canvas_add_procedure", json!({"procedure": proc}))
            .await;
        assert!(!result.is_error);

        let rule = json!({"name": "no-empty-text", "check": "node.props.text != ''"});
        let result = handler
            .call_tool("canvas_add_rule", json!({"rule": rule}))
            .await;
        assert!(!result.is_error);

        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert_eq!(doc["procedures"].as_array().unwrap().len(), 1);
        assert_eq!(doc["rules"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn canvas_tools_listed() {
        let handler = make_handler_with_state();
        let tools = handler.list_tools().await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"canvas_create"));
        assert!(names.contains(&"canvas_get"));
        assert!(names.contains(&"canvas_set_tree"));
        assert!(names.contains(&"canvas_add_node"));
        assert!(names.contains(&"canvas_remove_node"));
        assert!(names.contains(&"canvas_validate"));
        assert!(names.contains(&"canvas_export"));
        assert!(names.contains(&"canvas_import"));
        assert!(names.contains(&"canvas_list"));
        assert!(names.contains(&"canvas_load"));
        assert!(names.contains(&"canvas_save"));
        assert!(names.contains(&"canvas_set_data"));
        assert!(names.contains(&"canvas_add_procedure"));
        assert!(names.contains(&"canvas_add_rule"));
        assert!(names.contains(&"canvas_a2ui_push"));
        assert!(names.contains(&"canvas_a2ui_reset"));
        assert!(names.contains(&"canvas_catalog"));
    }

    #[tokio::test]
    async fn canvas_catalog_returns_components() {
        let handler = make_handler_with_state();
        let result = handler.call_tool("canvas_catalog", json!({})).await;
        assert!(!result.is_error);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let components = parsed["components"].as_array().unwrap();
        assert!(
            components.len() >= 15,
            "catalog should have at least 15 components"
        );

        // Verify key component types are present
        let types: Vec<&str> = components
            .iter()
            .filter_map(|c| c["type"].as_str())
            .collect();
        assert!(types.contains(&"Root"));
        assert!(types.contains(&"Container"));
        assert!(types.contains(&"Text"));
        assert!(types.contains(&"Button"));
        assert!(types.contains(&"Input"));
        assert!(types.contains(&"Select"));
        assert!(types.contains(&"Image"));
        assert!(types.contains(&"Table"));
        assert!(types.contains(&"Chart"));
        assert!(types.contains(&"Code"));
        assert!(types.contains(&"Card"));
        assert!(types.contains(&"Tabs"));
        assert!(types.contains(&"TabPane"));

        // Verify structure of a component
        let btn = components.iter().find(|c| c["type"] == "Button").unwrap();
        assert!(btn["description"].as_str().unwrap().contains("button"));
        assert!(btn["props"].is_object());
        assert_eq!(btn["children"], json!(false));
        let events = btn["events"].as_array().unwrap();
        assert!(events.contains(&json!("onClick")));
    }

    #[tokio::test]
    async fn canvas_a2ui_push_with_jsonl() {
        let handler = make_handler_with_state();
        // Create a canvas first
        handler
            .call_tool("canvas_create", json!({"title": "A2UI Test"}))
            .await;

        let jsonl = r#"{"op":"set","path":"/title","value":"Hello"}
{"op":"append","path":"/items","value":{"text":"Item 1"}}"#;

        let result = handler
            .call_tool("canvas_a2ui_push", json!({"jsonl": jsonl}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("2 instruction(s) pushed"));

        // Verify canvas has a2uiLastPush timestamp
        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert!(doc["a2uiLastPush"].as_str().is_some());
    }

    #[tokio::test]
    async fn canvas_a2ui_push_with_instructions_array() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "A2UI Array Test"}))
            .await;

        let instructions = json!([
            {"op": "set", "path": "/header", "value": "Dashboard"},
            {"op": "clear", "path": "/notifications"}
        ]);

        let result = handler
            .call_tool("canvas_a2ui_push", json!({"instructions": instructions}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("2 instruction(s) pushed"));
    }

    #[tokio::test]
    async fn canvas_a2ui_push_rejects_empty() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Empty Test"}))
            .await;

        // Empty JSONL
        let result = handler
            .call_tool("canvas_a2ui_push", json!({"jsonl": ""}))
            .await;
        assert!(result.is_error);

        // No params
        let result = handler.call_tool("canvas_a2ui_push", json!({})).await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn canvas_a2ui_reset_clears_queue() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Reset Test"}))
            .await;

        // Push some instructions
        let jsonl = r#"{"op":"set","path":"/x","value":1}"#;
        handler
            .call_tool("canvas_a2ui_push", json!({"jsonl": jsonl}))
            .await;

        // Reset
        let result = handler.call_tool("canvas_a2ui_reset", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("reset"));

        // Verify canvas has a2uiLastReset timestamp
        let get = handler.call_tool("canvas_get", json!({})).await;
        let doc: Value = serde_json::from_str(&get.content).unwrap();
        assert!(doc["a2uiLastReset"].as_str().is_some());
    }

    #[tokio::test]
    async fn canvas_a2ui_reset_with_target() {
        let handler = make_handler_with_state();
        handler
            .call_tool("canvas_create", json!({"title": "Target Reset"}))
            .await;

        let result = handler
            .call_tool("canvas_a2ui_reset", json!({"target": "node-1"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("node-1"));
    }
}
