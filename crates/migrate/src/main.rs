//! `pares-agens` CLI binary.
//!
//! # Usage
//!
//! ```text
//! pares-agens migrate [--from ~/.openclaw] [--output ./migration] [--dry-run]
//! pares-agens serve --telegram-token <TOKEN>
//! ```

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::telegram::{TelegramAdapter, TelegramConfig};
use pares_agens_core::agent::{Agent, InMemory};
use pares_agens_core::Event;
use pares_agens_migrate::{migrate, openclaw};

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
        ///
        /// When omitted, the tool auto-detects the default location
        /// (`~/.openclaw`).
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,

        /// Directory to write migrated output files.
        ///
        /// Defaults to `./migration` in the current working directory.
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

        Commands::Serve { telegram_token } => {
            tracing::info!("Starting Pares Agens daemon with Telegram adapter");

            let memory = InMemory::new();
            let memory = std::sync::Arc::new(memory);
            let agent = Agent::new(memory);
            let agent = std::sync::Arc::new(tokio::sync::Mutex::new(agent));

            let config = TelegramConfig::new(telegram_token);
            let adapter = TelegramAdapter::new(config);

            tracing::info!("Telegram adapter starting — bot is live");

            let agent_clone = agent.clone();
            if let Err(e) = adapter
                .run(move |event: Event| {
                    let agent = agent_clone.clone();
                    Box::pin(async move {
                        let agent = agent.lock().await;
                        agent.handle_event(event).await
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
