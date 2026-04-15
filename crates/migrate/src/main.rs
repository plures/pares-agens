//! `pares-agens` CLI binary.
//!
//! # Usage
//!
//! ```text
//! pares-agens migrate [--from ~/.openclaw] [--output ./migration] [--dry-run]
//! pares-agens serve --telegram-token <TOKEN> [--model-url <URL>] [--model <MODEL>]
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

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
use pares_agens_core::model::{ChatMessage as CoreChatMessage, ChatOptions, ModelClient, ToolDefinition, ToolDispatcher};
use pares_agens_core::procedure::{Procedure, ProcedureRegistry};
use pares_agens_core::Event;
use pares_agens_migrate::{migrate, openclaw};
use pares_models::config::{ProviderConfig, RouterConfig};
use pares_models::router::ModelRouter;
use pares_models::types::{ChatCompletionRequest, ChatMessage, Role, Tool};

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

struct RouterModelClient {
    router: Arc<ModelRouter>,
    model: String,
}

#[async_trait]
impl ModelClient for RouterModelClient {
    async fn complete(
        &self,
        messages: &[CoreChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<pares_agens_core::model::ModelCompletion, String> {
        let converted_messages = messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "system" => Role::System,
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                ChatMessage {
                    role,
                    content: Some(m.content.clone()),
                    tool_calls: m.tool_calls.clone().map(|calls| {
                        calls
                            .into_iter()
                            .map(|call| pares_models::types::ToolCall {
                                id: call.id,
                                kind: "function".into(),
                                function: pares_models::types::FunctionCall {
                                    name: call.name,
                                    arguments: call.arguments.to_string(),
                                },
                            })
                            .collect()
                    }),
                    tool_call_id: m.tool_call_id.clone(),
                    name: None,
                }
            })
            .collect();

        let mut request = ChatCompletionRequest::new(&self.model, converted_messages);
        if !tools.is_empty() {
            request.tools = Some(
                tools
                    .iter()
                    .map(|tool| {
                        Tool::function(tool.name.clone(), tool.description.clone(), tool.parameters.clone())
                    })
                    .collect(),
            );
        }
        if let Some(temp) = options.temperature {
            request.temperature = Some(temp as f32);
        }
        if options.logprobs {
            request.logprobs = Some(true);
        }

        let response = self
            .router
            .chat(&request)
            .await
            .map_err(|e| e.to_string())?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| "model returned no choices".to_string())?;

        let tool_calls = choice
            .message
            .tool_calls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|call| pares_agens_core::model::ToolCall {
                id: call.id,
                name: call.function.name,
                arguments: serde_json::from_str(&call.function.arguments)
                    .unwrap_or_else(|_| serde_json::Value::String(call.function.arguments)),
            })
            .collect();

        let logprobs = choice
            .logprobs
            .as_ref()
            .and_then(|lp| lp.content.as_ref())
            .map(|tokens| tokens.iter().filter_map(|t| t.logprob).collect::<Vec<_>>())
            .filter(|vals| !vals.is_empty());

        Ok(pares_agens_core::model::ModelCompletion {
            content: choice.message.content.clone(),
            tool_calls,
            logprobs,
        })
    }
}

struct ProcedureToolDispatcher {
    registry: Arc<ProcedureRegistry>,
}

#[async_trait]
impl ToolDispatcher for ProcedureToolDispatcher {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> String {
        let mut handler = None;
        for proc in self.registry.matching(name) {
            handler = Some(proc);
            break;
        }
        let handler = match handler {
            Some(h) => h,
            None => return format!("no procedure registered for {name}"),
        };

        let event = Event::Message {
            id: Uuid::new_v4().to_string(),
            channel: "tool".into(),
            sender: "model".into(),
            content: arguments.to_string(),
        };

        let results = handler.execute(&event).await;
        for result in results {
            if let Event::ToolResult {
                content, is_error, ..
            } = result
            {
                if is_error {
                    return format!("tool error: {content}");
                }
                return content;
            }
        }

        format!("procedure {name} returned no tool result")
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

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a UTF-8 text file from disk".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "write_file".into(),
            description: "Write a UTF-8 text file to disk".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        },
        ToolDefinition {
            name: "run_command".into(),
            description: "Run a shell command and return stdout/stderr".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        },
    ]
}

fn build_system_prompt(path: Option<PathBuf>) -> Result<String, String> {
    let base = if let Some(path) = path {
        std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read system prompt {}: {e}", path.display()))?
    } else {
        "You are Praxis, an AI agent built on the Pares Agens framework. You are helpful, concise, and knowledgeable about software engineering.".to_string()
    };
    Ok(base)
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

        /// Deep model name used for low-confidence escalation.
        #[arg(long, env = "PARES_DEEP_MODEL", default_value = "gpt-4.1")]
        deep_model: String,

        /// Deep model API URL (defaults to --model-url).
        #[arg(long, env = "PARES_DEEP_MODEL_URL")]
        deep_model_url: Option<String>,

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
            deep_model,
            deep_model_url,
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

            let deep_model_url = deep_model_url.unwrap_or_else(|| model_url.clone());
            let deep_provider_config = ProviderConfig::new(&deep_model_url, api_key.clone());
            let deep_router_config = RouterConfig::single("deep", deep_provider_config);
            let deep_model_router = Arc::new(ModelRouter::new(deep_router_config));

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

            // Register native tool procedures
            let mut procedure_registry = ProcedureRegistry::new();
            procedure_registry.register(Box::new(ReadFileProcedure));
            procedure_registry.register(Box::new(WriteFileProcedure));
            procedure_registry.register(Box::new(RunCommandProcedure));
            let procedure_registry = Arc::new(procedure_registry);

            let model_client = Arc::new(RouterModelClient {
                router: model_router.clone(),
                model: model.clone(),
            });
            let deep_model_client = Arc::new(RouterModelClient {
                router: deep_model_router.clone(),
                model: deep_model.clone(),
            });
            let tool_dispatcher = Arc::new(ProcedureToolDispatcher {
                registry: Arc::clone(&procedure_registry),
            });

            let agent = Arc::new(Agent::with_cerebellum(
                memory,
                cerebellum,
                Arc::clone(&plures_lm),
            )
            .with_model(model_client, tool_dispatcher, system_prompt)
            .with_deep_model(deep_model_client));

            // Set up Telegram adapter
            let config = TelegramConfig::new(telegram_token);
            let adapter = TelegramAdapter::new(config);

            tracing::info!("Telegram adapter starting — bot is live");

            let agent_clone = agent.clone();

            if let Err(e) = adapter
                .run(move |event: Event| {
                    let agent = agent_clone.clone();
                    Box::pin(async move { agent.handle_event(event).await })
                })
                .await
            {
                tracing::error!("Telegram adapter exited: {e}");
                std::process::exit(1);
            }
        }
    }
}
