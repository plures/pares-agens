//! `pares-agens` CLI binary.
//!
//! # Usage
//!
//! ```text
//! pares-agens migrate [--from ~/.openclaw] [--output ./migration] [--dry-run]
//! pares-agens serve --telegram-token <TOKEN> [--model-url <URL>] [--model <MODEL>]
//! ```

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::telegram::{TelegramAdapter, TelegramConfig};
use pares_agens_core::agent::{Agent, InMemory};
use pares_agens_core::Event;
use pares_agens_migrate::{migrate, openclaw};
use pares_models::config::{ProviderConfig, RouterConfig};
use pares_models::router::ModelRouter;
use pares_models::types::{ChatCompletionRequest, ChatMessage, Role};

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

        /// OpenAI-compatible API URL (e.g., Ollama, GitHub Copilot, OpenAI).
        #[arg(long, env = "PARES_MODEL_URL", default_value = "http://localhost:11434")]
        model_url: String,

        /// Model name to use.
        #[arg(long, env = "PARES_MODEL", default_value = "llama3.2")]
        model: String,

        /// API key for the model provider (optional for local Ollama).
        #[arg(long, env = "PARES_API_KEY")]
        api_key: Option<String>,
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
        } => {
            tracing::info!("Starting Pares Agens daemon");
            tracing::info!("Model: {model} @ {model_url}");

            // Set up model router
            let provider_config = ProviderConfig::new(&model_url, api_key);
            let router_config = RouterConfig::single("default", provider_config);
            let model_router = std::sync::Arc::new(ModelRouter::new(router_config));
            let model_name = model.clone();

            // Set up agent with in-memory store
            let memory = std::sync::Arc::new(InMemory::new());
            let agent = std::sync::Arc::new(tokio::sync::Mutex::new(Agent::new(memory)));

            // Set up Telegram adapter
            let config = TelegramConfig::new(telegram_token);
            let adapter = TelegramAdapter::new(config);

            tracing::info!("Telegram adapter starting — bot is live");

            let agent_clone = agent.clone();
            let router_clone = model_router.clone();
            let model_for_closure = model_name.clone();

            if let Err(e) = adapter
                .run(move |event: Event| {
                    let agent = agent_clone.clone();
                    let router = router_clone.clone();
                    let model = model_for_closure.clone();
                    Box::pin(async move {
                        // Extract message content
                        let (request_id, content) = match &event {
                            Event::Message { id, content, .. } => {
                                (id.clone(), content.clone())
                            }
                            _ => return None,
                        };

                        // Capture in memory
                        {
                            let agent = agent.lock().await;
                            let _ = agent.handle_event(event).await;
                        }

                        // Call the LLM
                        let messages = vec![
                            ChatMessage::text(
                                Role::System,
                                "You are Praxis, an AI assistant built on the Pares Agens framework. \
                                 You are helpful, concise, and knowledgeable about software engineering.",
                            ),
                            ChatMessage::text(Role::User, &content),
                        ];

                        let request = ChatCompletionRequest::new(&model, messages);

                        match router.chat(&request).await {
                            Ok(response) => {
                                let reply = response.choices.first()
                                    .and_then(|c| c.message.content.as_deref())
                                    .unwrap_or("(no response from model)")
                                    .to_string();

                                tracing::info!(
                                    model = %model,
                                    input_len = content.len(),
                                    output_len = reply.len(),
                                    "LLM response generated"
                                );

                                Some(Event::ModelResponse {
                                    request_id,
                                    model,
                                    content: reply,
                                })
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "LLM call failed");
                                Some(Event::ModelResponse {
                                    request_id,
                                    model: "error".into(),
                                    content: format!("⚠️ Model error: {e}"),
                                })
                            }
                        }
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
