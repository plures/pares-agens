//! `pares-agens` CLI binary.
//!
//! # Usage
//!
//! ```text
//! pares-agens migrate [--from ~/.openclaw] [--output ./migration] [--dry-run]
//! pares-agens serve --telegram-token <TOKEN> [--model-url <URL>] [--model <MODEL>]
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::telegram::{TelegramAdapter, TelegramConfig};
use pares_agens_core::agent::{Agent, Memory};
use pares_agens_core::cerebellum::{Cerebellum, CerebellumConfig};
use pares_agens_core::memory::{
    embed::{EmbeddingProvider, MockEmbedder, OllamaEmbedder},
    entry::Exchange,
    store::PluresDbStore,
    PluresLm,
};
use pares_agens_core::procedure::{Procedure, ProcedureRegistry};
use pares_agens_core::Event;
use pares_agens_migrate::{migrate, openclaw};
use pares_models::config::{ProviderConfig, RouterConfig};
use pares_models::router::ModelRouter;
use pares_models::types::{ChatCompletionRequest, ChatMessage, Role, Tool, ToolCall};

#[derive(Debug, Parser)]
#[command(
    name = "pares-agens",
    version,
    about = "Pares Agens agent runtime CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone)]
struct SystemPromptConfig {
    base: String,
}

impl SystemPromptConfig {
    fn with_context(&self, learned_context: &str) -> String {
        if learned_context.trim().is_empty() {
            self.base.clone()
        } else {
            format!(
                "{}\n\n## Recalled Context\n{}",
                self.base,
                learned_context.trim()
            )
        }
    }
}

struct PluresMemory {
    plures_lm: Arc<PluresLm>,
}

#[async_trait]
impl Memory for PluresMemory {
    async fn capture(&self, content: &str) -> Result<(), String> {
        let exchange = Exchange {
            user: content.to_string(),
            assistant: String::new(),
        };
        self.plures_lm
            .capture(&exchange)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn recall(&self, query: &str) -> Result<Vec<String>, String> {
        let entries = self
            .plures_lm
            .recall(query, 5, &[])
            .await
            .map_err(|e| e.to_string())?;
        Ok(entries.into_iter().map(|e| e.content).collect())
    }
}

struct ReadFileProcedure;
struct WriteFileProcedure;
struct RunCommandProcedure;

#[async_trait]
impl Procedure for ReadFileProcedure {
    fn name(&self) -> &str {
        "read_file"
    }

    fn handles(&self) -> &str {
        "read_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("path").and_then(|v| v.as_str()) {
                        Some(path) => tokio::fs::read_to_string(path)
                            .await
                            .map_err(|e| e.to_string()),
                        None => Err("missing 'path'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "read_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for WriteFileProcedure {
    fn name(&self) -> &str {
        "write_file"
    }

    fn handles(&self) -> &str {
        "write_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let path = args.get("path").and_then(|v| v.as_str());
                        let body = args.get("content").and_then(|v| v.as_str());
                        match (path, body) {
                            (Some(path), Some(body)) => tokio::fs::write(path, body)
                                .await
                                .map_err(|e| e.to_string())
                                .map(|_| "ok".to_string()),
                            _ => Err("missing 'path' or 'content'".into()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "write_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for RunCommandProcedure {
    fn name(&self) -> &str {
        "run_command"
    }

    fn handles(&self) -> &str {
        "run_command"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("command").and_then(|v| v.as_str()) {
                        Some(command) => {
                            let output = tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(command)
                                .output()
                                .await
                                .map_err(|e| e.to_string());
                            match output {
                                Ok(output) => {
                                    let stdout = String::from_utf8_lossy(&output.stdout);
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    let status = output
                                        .status
                                        .code()
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "signal".into());
                                    Ok(format!(
                                        "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                                        status, stdout, stderr
                                    ))
                                }
                                Err(e) => Err(e),
                            }
                        }
                        None => Err("missing 'command'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "run_command".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

fn parse_tool_args(raw: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(raw).map_err(|e| format!("invalid tool arguments: {e}"))
}

fn tool_definitions() -> Vec<Tool> {
    vec![
        Tool::function(
            "read_file",
            "Read a UTF-8 text file from disk",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        ),
        Tool::function(
            "write_file",
            "Write a UTF-8 text file to disk",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        ),
        Tool::function(
            "run_command",
            "Run a shell command and return stdout/stderr",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        ),
    ]
}

async fn dispatch_tool_call(
    registry: &Arc<ProcedureRegistry>,
    tool_call: &ToolCall,
) -> Result<String, String> {
    let tool_name = tool_call.function.name.as_str();
    let event = Event::Message {
        id: tool_call.id.clone(),
        channel: "tool".into(),
        sender: "model".into(),
        content: tool_call.function.arguments.clone(),
    };

    let mut handler = None;
    for proc in registry.matching(tool_name) {
        handler = Some(proc);
        break;
    }

    let handler = handler.ok_or_else(|| format!("no procedure registered for {tool_name}"))?;
    let results = handler.execute(&event).await;

    for result in results {
        if let Event::ToolResult {
            content,
            is_error,
            ..
        } = result
        {
            return if is_error { Err(content) } else { Ok(content) };
        }
    }

    Err(format!("procedure {tool_name} returned no tool result"))
}

fn extract_recalled_context(event: &Event) -> String {
    if let Event::ModelResponse { content, .. } = event {
        let marker = "## Recalled Context";
        if let Some(idx) = content.find(marker) {
            return content[idx + marker.len()..].trim().to_string();
        }
    }
    String::new()
}

fn build_system_prompt(path: Option<PathBuf>) -> Result<SystemPromptConfig, String> {
    let base = if let Some(path) = path {
        std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read system prompt {}: {e}", path.display()))?
    } else {
        "You are Praxis, an AI agent built on the Pares Agens framework. You are helpful, concise, and knowledgeable about software engineering.".to_string()
    };
    Ok(SystemPromptConfig { base })
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Migrate data from an existing OpenClaw installation.
    Migrate {
        /// Path to the OpenClaw installation directory.
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,

        /// Directory to write migrated output files.
        #[arg(long, value_name = "PATH", default_value = "migration")]
        output: PathBuf,

        /// Simulate the migration without writing any files.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run the agent as a headless daemon with a channel adapter.
    Serve {
        /// Telegram bot token (from BotFather).
        #[arg(long, env = "PARES_TELEGRAM_TOKEN")]
        telegram_token: String,

        /// OpenAI-compatible API URL (GitHub Models or OpenAI compatible endpoint).
        #[arg(
            long,
            env = "PARES_MODEL_URL",
            default_value = "https://models.inference.ai.azure.com"
        )]
        model_url: String,

        /// Model name to use.
        #[arg(long, env = "PARES_MODEL", default_value = "gpt-4o")]
        model: String,

        /// API key for the model provider.
        #[arg(long, env = "PARES_API_KEY")]
        api_key: Option<String>,

        /// Optional OpenAI-compatible embeddings endpoint.
        #[arg(long, env = "PARES_EMBED_URL")]
        embed_url: Option<String>,

        /// Embeddings model name.
        #[arg(long, env = "PARES_EMBED_MODEL", default_value = "nomic-embed-text")]
        embed_model: String,

        /// Path to a system prompt file.
        #[arg(long, value_name = "PATH")]
        system_prompt: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Migrate {
            from,
            output,
            dry_run,
        } => {
            let source = match from.or_else(openclaw::auto_detect) {
                Some(p) => p,
                None => {
                    eprintln!(
                        "No OpenClaw installation found. \
                         Use --from <PATH> to specify one."
                    );
                    std::process::exit(1);
                }
            };
            match migrate::run(&source, &output, dry_run) {
                Ok(report) => {
                    report.print();
                }
                Err(e) => {
                    eprintln!("Migration failed: {e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Serve {
            telegram_token,
            model_url,
            model,
            api_key,
            embed_url,
            embed_model,
            system_prompt,
        } => {
            tracing::info!("Starting Pares Agens daemon");
            tracing::info!("Model: {model} @ {model_url}");

            let system_prompt = match build_system_prompt(system_prompt) {
                Ok(prompt) => prompt,
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            };

            // Set up model router
            let provider_config = ProviderConfig::new(&model_url, api_key.clone());
            let router_config = RouterConfig::single("default", provider_config);
            let model_router = Arc::new(ModelRouter::new(router_config));
            let model_name = model.clone();

            // Set up PluresDB memory store + PluresLM (native)
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let memory_path = PathBuf::from(home).join(".pares-agens/memory");
            let store = match PluresDbStore::open_with_embeddings(&memory_path) {
                Ok(store) => {
                    tracing::info!("PluresDB with native fastembed (auto-embed on every write)");
                    Arc::new(store)
                }
                Err(e) => {
                    tracing::warn!("fastembed unavailable ({e}), falling back to basic store");
                    match PluresDbStore::open(&memory_path) {
                        Ok(store) => Arc::new(store),
                        Err(e2) => {
                            tracing::error!("failed to open memory store: {e2}");
                            std::process::exit(1);
                        }
                    }
                }
            };

            let embedder: Box<dyn EmbeddingProvider> = match embed_url {
                Some(url) => Box::new(OllamaEmbedder::new(url, embed_model.clone(), api_key.clone())),
                None => Box::new(MockEmbedder),
            };

            let plures_lm = Arc::new(PluresLm::new(
                store as Arc<dyn pares_agens_core::memory::store::MemoryStore>,
                embedder,
                128_000,
            ));

            let memory = Arc::new(PluresMemory {
                plures_lm: Arc::clone(&plures_lm),
            });
            let cerebellum = Cerebellum::new(CerebellumConfig::default());
            let agent = Arc::new(tokio::sync::Mutex::new(Agent::with_cerebellum(
                memory,
                cerebellum,
                Arc::clone(&plures_lm),
            )));

            // Register native tool procedures
            let mut procedure_registry = ProcedureRegistry::new();
            procedure_registry.register(Box::new(ReadFileProcedure));
            procedure_registry.register(Box::new(WriteFileProcedure));
            procedure_registry.register(Box::new(RunCommandProcedure));
            let procedure_registry = Arc::new(procedure_registry);

            // Set up Telegram adapter
            let config = TelegramConfig::new(telegram_token);
            let adapter = TelegramAdapter::new(config);

            tracing::info!("Telegram adapter starting — bot is live");

            let agent_clone = agent.clone();
            let router_clone = model_router.clone();
            let model_for_closure = model_name.clone();
            let histories: Arc<tokio::sync::Mutex<HashMap<String, Vec<ChatMessage>>>> =
                Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let histories_clone = histories.clone();
            let prompt_clone = system_prompt.clone();
            let registry_clone = procedure_registry.clone();

            if let Err(e) = adapter
                .run(move |event: Event| {
                    let agent = agent_clone.clone();
                    let router = router_clone.clone();
                    let model = model_for_closure.clone();
                    let histories = histories_clone.clone();
                    let system_prompt = prompt_clone.clone();
                    let registry = registry_clone.clone();
                    Box::pin(async move {
                        // Extract message content
                        let (request_id, content, chat_key) = match &event {
                            Event::Message {
                                id,
                                content,
                                channel,
                                sender,
                            } => (id.clone(), content.clone(), format!("{channel}:{sender}")),
                            _ => return None,
                        };

                        // Autorecall + capture via agent
                        let learned_context = {
                            let agent = agent.lock().await;
                            match agent.handle_event(event.clone()).await {
                                Some(response) => extract_recalled_context(&response),
                                None => return None,
                            }
                        };

                        let system_text = system_prompt.with_context(&learned_context);

                        let tools = tool_definitions();
                        let mut history_guard = histories.lock().await;
                        let history = history_guard.entry(chat_key.clone()).or_default();
                        let history_len = history.len();

                        let mut messages = Vec::with_capacity(history.len() + 2);
                        messages.push(ChatMessage::text(Role::System, system_text));
                        messages.extend(history.iter().cloned());
                        messages.push(ChatMessage::text(Role::User, &content));

                        let mut final_reply = None;

                        for _ in 0..10 {
                            let mut request = ChatCompletionRequest::new(&model, messages.clone());
                            request.tools = Some(tools.clone());

                            let response = match router.chat(&request).await {
                                Ok(r) => r,
                                Err(e) => {
                                    tracing::error!(error = %e, "LLM call failed");
                                    return Some(Event::ModelResponse {
                                        request_id,
                                        model: "error".into(),
                                        content: format!("⚠️ Model error: {e}"),
                                    });
                                }
                            };

                            let choice = match response.choices.first() {
                                Some(choice) => choice,
                                None => {
                                    final_reply = Some("(no response from model)".to_string());
                                    break;
                                }
                            };

                            let message = choice.message.clone();
                            if let Some(tool_calls) = message.tool_calls.clone() {
                                messages.push(message);
                                for tool_call in tool_calls {
                                    let tool_result = dispatch_tool_call(&registry, &tool_call)
                                        .await
                                        .unwrap_or_else(|e| format!("tool error: {e}"));
                                    messages.push(ChatMessage {
                                        role: Role::Tool,
                                        content: Some(tool_result),
                                        tool_calls: None,
                                        tool_call_id: Some(tool_call.id),
                                        name: None,
                                    });
                                }
                                continue;
                            }

                            if let Some(content) = message.content.clone() {
                                final_reply = Some(content.clone());
                                messages.push(message);
                                break;
                            }

                            final_reply = Some("(empty response from model)".to_string());
                            break;
                        }

                        let reply = final_reply.unwrap_or_else(|| "(no response from model)".into());

                        tracing::info!(
                            model = %model,
                            input_len = content.len(),
                            output_len = reply.len(),
                            "LLM response generated"
                        );

                        let start = 1 + history_len;
                        if messages.len() > start {
                            history.extend_from_slice(&messages[start..]);
                            if history.len() > 20 {
                                let drain = history.len() - 20;
                                history.drain(0..drain);
                            }
                        }

                        Some(Event::ModelResponse {
                            request_id,
                            model,
                            content: reply,
                        })
                    })
                })
                .await
            {
                tracing::error!("Telegram adapter exited: {e}");
                std::process::exit(1);
            }
        }
    }
}
