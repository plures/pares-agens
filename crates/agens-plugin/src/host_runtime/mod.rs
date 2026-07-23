//! `host_runtime` - the relocated Pares Agens host runtime composition seam.
//!
//! RELOCATED (Stage R3a, Option A) from `pares-radix-cli-runtime/src/lib.rs`
//! into `agens-plugin`. Under Option A the host composition lives in agens, not
//! radix: `agens-plugin` IS the host (it owns `src/bin/pares-agens.rs` and the
//! agent command surface). This module holds the full host command surface and
//! the [`run_with_providers`] composition entrypoint (decision C1).
//!
//! The agens binary (`src/bin/pares-agens.rs`) constructs an [`AgensProvider`]
//! and calls [`run_with_providers`] with it. Option B (agens is the sole host of
//! a single provider): the host builds the base command, lets the provider
//! `augment` its subcommands on and gives it first refusal via `handle`, then
//! falls through to the host's own dispatch. The platform crates this composes
//! (`pares_rector`, `pares_radix_core`, `pares_radix_praxis`) remain radix
//! platform pins; the agens-owned MCP server (`pares_agens_mcp_server`) is a
//! local host crate. Only the host *composition* moved.
//!
//! NOTE (deferred follow-up): the data/log directory is still named
//! `~/.pares-radix` (see [`run_with_providers`] / `migrate_data_dir`). Renaming
//! the on-disk dir to `pares-agens` is a behavior change deliberately left OUT
//! of scope for R3a; track it as a follow-up.
//!
//! NOTE (v1.55.13): the former `pares_radix_cli_api` command-plugin seam
//! (`CommandProvider`/`ProviderRegistry`/`ProviderOutcome`/`CommandError`) was
//! removed upstream (breaking commit 3172cfa). agens is now the sole host, so
//! the registry indirection was dropped and the host calls the single
//! [`AgensProvider`] directly. The `Migrate` subcommand (backed by the deleted
//! `pares_radix_migrate` crate) was removed with it.

mod config;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
#[allow(unused_imports)]
use tracing::{debug, error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::agent_commands::AgensProvider;
use pares_agens_hostkit::build_env_filter;

#[derive(Debug, Parser)]
#[command(
    name = "pares-radix",
    version,
    about = "Pares Radix agent runtime CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Cluster management commands.
    Cluster {
        #[command(subcommand)]
        action: ClusterAction,
    },

    /// Run as an MCP server over stdio (for external agent integration).
    McpServe {
        /// Working directory for file operations.
        #[arg(long, default_value = ".")]
        workdir: PathBuf,

        /// Brave Search API key (falls back to BRAVE_API_KEY env var).
        #[arg(long, env = "BRAVE_API_KEY")]
        brave_api_key: Option<String>,
    },

    /// Show or manage configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Praxis .px file tools (check, test).
    Px {
        #[command(subcommand)]
        action: PxAction,
    },
}

#[derive(Debug, clap::Subcommand)]
enum PxAction {
    /// Check .px files for syntax errors.
    Check {
        /// .px files or directories to check.
        files: Vec<String>,
    },
    /// Run scenario tests in .px files.
    Test {
        /// .px files or directories to test.
        files: Vec<String>,
    },
}

#[derive(Debug, clap::Subcommand)]
enum ConfigAction {
    /// Show current configuration.
    Show,
    /// Print config file path.
    Path,
}

#[derive(Debug, clap::Subcommand)]
enum ClusterAction {
    /// Show cluster status.
    Status,
    /// List all discovered nodes.
    Nodes,
    /// Deploy workloads from a .px file.
    Deploy {
        /// Path to a .px constraint file.
        px_file: String,
    },
    /// List running workloads.
    Workloads,
    /// Join this node to a cluster.
    Join {
        /// Hyperswarm topic key (hex).
        topic_key: String,
        /// Comma-separated direct peers (ip:port,ip:port).
        #[arg(long)]
        direct: Option<String>,
        /// Enable LAN multicast discovery.
        #[arg(long)]
        lan: bool,
    },
    /// Show this node's capabilities.
    Info,
}

/// Migrate data directory from `~/.pares-radix` to `~/.pares-radix`.
///
/// If the old directory exists and the new one does not, rename it.
/// If both exist, leave them alone (user manages the conflict).
fn migrate_data_dir(home: &str) {
    let old = PathBuf::from(home).join(".pares-radix");
    let new = PathBuf::from(home).join(".pares-radix");
    if old.is_dir() && !new.exists() {
        match std::fs::rename(&old, &new) {
            Ok(()) => eprintln!("Migrated data directory: {old:?} \u{2192} {new:?}"),
            Err(e) => eprintln!("Warning: failed to migrate {old:?} \u{2192} {new:?}: {e}"),
        }
    }
}
/// Run the Pares Agens host CLI with an explicit set of plugin command providers.
///
/// This is the reusable composition seam (decision C1), RELOCATED into
/// `agens-plugin` (Stage R3a). The agens binary (`src/bin/pares-agens.rs`)
/// builds a registry that registers the agent `serve`/`tui`/`ask`/`classify`
/// provider and calls this with it. Plugin subcommands are augmented onto the
/// host `clap` command before parsing and offered to the registry before the
/// host's own command dispatch.
pub async fn run_with_providers(provider: AgensProvider) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

    // Migrate data directory from ~/.pares-radix to ~/.pares-radix if needed
    migrate_data_dir(&home);

    let log_dir = PathBuf::from(&home).join(".pares-radix/logs");
    let _ = std::fs::create_dir_all(&log_dir);

    // Default Chronos JSONL to ~/.pares-radix/logs/chronos/
    if std::env::var("PARES_TELEMETRY_DIR").is_err() {
        unsafe {
            std::env::set_var("PARES_TELEMETRY_DIR", log_dir.join("chronos"));
        }
    }

    let initial_filter = build_env_filter("info").expect("default log level should be valid");
    let (filter_layer, _log_filter_handle) = tracing_subscriber::reload::Layer::new(initial_filter);

    let log_file_path = log_dir.join("pares-radix.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .expect("failed to open log file");

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(log_file))
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true),
        )
        .init();

    // Compose the host command surface with the agens provider (decision C1,
    // Option B). The provider augments its subcommands onto the derived `Cli`
    // command, then gets first refusal on the matched top-level subcommand
    // before the host's own dispatch runs.
    let base = <Cli as clap::CommandFactory>::command();
    let augmented = provider.augment(base);
    let matches = augmented.get_matches();

    if let Some((name, sub_matches)) = matches.subcommand() {
        if let Some(result) = provider.handle(name, sub_matches).await {
            match result {
                Ok(()) => return,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
    }

    let cli = match <Cli as clap::FromArgMatches>::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    let radix_config = config::RadixConfig::load();

    match cli.command {
        Commands::Cluster { action } => {
            use pares_rector::cluster;
            use pares_rector::discovery::PluresDbDiscovery;
            use pares_rector::node::{ClusterNode, NodeStatus};

            let caps = PluresDbDiscovery::detect_local_capabilities();
            let hostname = std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("COMPUTERNAME"))
                .unwrap_or_else(|_| {
                    std::fs::read_to_string("/etc/hostname")
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|_| "unknown".to_string())
                });
            let local_node = ClusterNode {
                id: "local".to_string(),
                hostname: hostname.clone(),
                addresses: vec![],
                capabilities: caps.clone(),
                status: NodeStatus::Online,
                workloads: vec![],
                last_seen: 0,
                cpu_usage: 0.0,
            };
            let nodes = vec![local_node];

            match action {
                ClusterAction::Status => {
                    let summary = cluster::ClusterSummary::from_nodes(&nodes);
                    println!("{}", cluster::format_cluster_status(&summary));
                }
                ClusterAction::Nodes => {
                    println!("{}", cluster::format_cluster_nodes(&nodes));
                }
                ClusterAction::Info => {
                    println!("{}", cluster::format_node_info(&caps));
                }
                ClusterAction::Deploy { px_file } => match std::fs::read_to_string(&px_file) {
                    Ok(content) => println!("{}", cluster::format_deploy_result(&content, &nodes)),
                    Err(e) => {
                        eprintln!("Failed to read {px_file}: {e}");
                        std::process::exit(1);
                    }
                },
                ClusterAction::Workloads => {
                    println!("No active workloads.");
                }
                ClusterAction::Join {
                    topic_key,
                    direct,
                    lan,
                } => {
                    println!("Joining cluster with topic key: {topic_key}");
                    if let Some(ref peers) = direct {
                        println!("Direct peers: {peers}");
                    }
                    if lan {
                        println!("LAN multicast discovery enabled");
                    }
                    println!("(Hyperswarm join not yet wired \u{2014} PluresDB sync must be configured separately)");
                }
            }
        }

        Commands::McpServe {
            workdir,
            brave_api_key,
        } => {
            use pares_radix_core::shell_executor::ShellExecutor;
            use pares_agens_mcp_server::{McpServer, RadixToolHandler};

            let shell = Arc::new(ShellExecutor::new());
            let resolved_workdir = std::fs::canonicalize(&workdir).unwrap_or(workdir);

            // Set up PluresDB state store for db_get/db_put/db_delete
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let state_dir = std::path::PathBuf::from(&home)
                .join(".pares-radix")
                .join("mcp-state");
            std::fs::create_dir_all(&state_dir).ok();
            let state_store: Arc<dyn pares_radix_core::StateStore> = {
                use pares_radix_core::state::PluresDbStateStore;
                match PluresDbStateStore::open(&state_dir) {
                    Ok(store) => {
                        tracing::info!("MCP state store opened at {}", state_dir.display());
                        Arc::new(store)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to open MCP state store: {e}, using in-memory");
                        Arc::new(pares_radix_core::state::InMemoryStateStore::new())
                    }
                }
            };

            let mut handler = RadixToolHandler::new(shell, resolved_workdir.clone())
                .with_state_store(state_store);
            if let Some(key) = brave_api_key {
                handler = handler.with_brave_api_key(key);
            }

            // Memory (memory_search/memory_store) is a COGNITION concern: it is
            // backed by `PluresLm`, which lives in the cognition crate
            // (`pares-agens-core`) and is relocating out of this platform crate
            // (ADR-0022 cognition relocation). The platform host therefore does
            // NOT construct memory here \u2014 the cognition composition (the
            // `pares-agens` plugin binary / the relocated mcp-server in a later
            // stage) attaches an `Arc<dyn pares_radix_core::memory_client::MemoryClient>`
            // implementation. When no memory is attached, the MCP handler reports
            // `memory: not_configured` and `memory_*` tools return a real,
            // caller-handled "memory not configured" error (no stub, no fake).

            // Set up Chronos timeline with its own dedicated PluresDB CrdtStore.
            {
                use pares_radix_core::chronos::ChronosTimeline;
                use pares_radix_core::{CrdtStore, MemoryStorage, SledStorage, StorageEngine};
                let chronos_dir = std::path::PathBuf::from(&home)
                    .join(".pares-radix")
                    .join("mcp-chronos");
                std::fs::create_dir_all(&chronos_dir).ok();
                let storage: Arc<dyn StorageEngine> = match SledStorage::open(&chronos_dir) {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        tracing::warn!(
                            "Failed to open Chronos store at {}: {e}, using in-memory",
                            chronos_dir.display()
                        );
                        Arc::new(MemoryStorage::default())
                    }
                };
                let crdt = Arc::new(CrdtStore::default().with_persistence(storage));
                let chronos = ChronosTimeline::new(crdt);
                handler = handler.with_chronos(Arc::new(chronos));
                tracing::info!("MCP Chronos timeline enabled");
            }

            // Auto-load .px procedures from praxis/ directory if it exists
            let px_dir = resolved_workdir.join("praxis");
            if px_dir.is_dir() {
                handler = handler.with_px_dir(px_dir.clone());
            }
            // Also check ~/.radix/praxis/ for user-level procedures
            let user_px_dir = if let Ok(home) = std::env::var("HOME") {
                let dir = std::path::PathBuf::from(home).join(".radix").join("praxis");
                if dir.is_dir() {
                    handler = handler.with_px_dir(dir.clone());
                    Some(dir)
                } else {
                    None
                }
            } else {
                None
            };

            // Start PxWatcher for hot-reload on praxis directories
            let mut watch_dirs = Vec::new();
            if px_dir.is_dir() {
                watch_dirs.push(px_dir);
            }
            if let Some(dir) = user_px_dir {
                watch_dirs.push(dir);
            }
            for dir in &watch_dirs {
                if let Err(e) = handler.start_px_watcher(dir.clone()).await {
                    tracing::warn!(path = %dir.display(), "failed to start PxWatcher: {e}");
                }
            }

            let server = McpServer::new(Arc::new(handler));
            if let Err(e) = server.run().await {
                tracing::error!("MCP server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Config { action } => match action {
            ConfigAction::Show => {
                println!(
                    "{}",
                    toml::to_string_pretty(&radix_config).unwrap_or_default()
                );
            }
            ConfigAction::Path => {
                println!("{}", config::RadixConfig::config_path().display());
            }
        },
        Commands::Px { action } => match action {
            PxAction::Check { files } => {
                let mut errors = 0;
                let paths = collect_px_files(&files);
                if paths.is_empty() {
                    eprintln!("No .px files found");
                    std::process::exit(1);
                }
                for path in &paths {
                    match std::fs::read_to_string(path) {
                        Ok(source) => match pares_radix_praxis::px::parse(&source) {
                            Ok(_) => println!("  \x1b[32m\u{2713}\x1b[0m {}", path.display()),
                            Err(e) => {
                                eprintln!(
                                    "  \x1b[31m\u{2717}\x1b[0m {} \u{2014} {}",
                                    path.display(),
                                    e
                                );
                                errors += 1;
                            }
                        },
                        Err(e) => {
                            eprintln!(
                                "  \x1b[31m\u{2717}\x1b[0m {} \u{2014} read error: {}",
                                path.display(),
                                e
                            );
                            errors += 1;
                        }
                    }
                }
                println!("\n{} file(s) checked, {} error(s)", paths.len(), errors);
                if errors > 0 {
                    std::process::exit(1);
                }
            }
            PxAction::Test { files } => {
                use pares_radix_praxis::px::compiler::compile;
                use pares_radix_praxis::px::scenario_runner::{run_scenarios, BuiltinChecker};

                let paths = collect_px_files(&files);
                if paths.is_empty() {
                    eprintln!("No .px files found");
                    std::process::exit(1);
                }

                let mut total_scenarios = 0;
                let mut total_passed = 0;
                let mut total_failed = 0;

                for path in &paths {
                    let source = match std::fs::read_to_string(path) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!(
                                "  \x1b[31m\u{2717}\x1b[0m {} \u{2014} read error: {}",
                                path.display(),
                                e
                            );
                            total_failed += 1;
                            continue;
                        }
                    };

                    let doc = match pares_radix_praxis::px::parse(&source) {
                        Ok(d) => d,
                        Err(e) => {
                            eprintln!(
                                "  \x1b[31m\u{2717}\x1b[0m {} \u{2014} parse error: {}",
                                path.display(),
                                e
                            );
                            total_failed += 1;
                            continue;
                        }
                    };

                    let has_scenarios = doc.statements.iter().any(|s| {
                        matches!(s, pares_radix_praxis::px::Statement::Scenario(_))
                    });
                    if !has_scenarios {
                        continue;
                    }

                    let records = compile(&doc);

                    let mut procedures = std::collections::HashMap::new();
                    for record in &records {
                        if record.key.starts_with("px:procedure/") {
                            let name = record.key.strip_prefix("px:procedure/").unwrap_or("");
                            procedures.insert(name.to_string(), record.data.clone());
                        }
                    }

                    let scenario_data: Vec<serde_json::Value> = records
                        .iter()
                        .filter(|r| r.key.starts_with("px:scenario/"))
                        .map(|r| r.data.clone())
                        .collect();

                    let suite = run_scenarios(&scenario_data, &procedures, &BuiltinChecker);

                    println!("\n\x1b[1m{}\x1b[0m", path.display());
                    for result in &suite.results {
                        if result.passed {
                            println!("  \x1b[32m\u{2713}\x1b[0m {}", result.name);
                        } else {
                            println!("  \x1b[31m\u{2717}\x1b[0m {}", result.name);
                            if let Some(err) = &result.error {
                                println!("    error: {}", err);
                            }
                            for exp in &result.expectations {
                                if !exp.passed {
                                    let neg = if exp.negated { "NOT " } else { "" };
                                    println!(
                                        "    - {}{}: {}",
                                        neg,
                                        exp.check,
                                        exp.reason.as_deref().unwrap_or("failed")
                                    );
                                }
                            }
                        }
                    }

                    total_scenarios += suite.total;
                    total_passed += suite.passed;
                    total_failed += suite.failed;
                }

                println!();
                if total_failed == 0 {
                    println!(
                        "\x1b[32m\u{2713} {} scenario(s) passed\x1b[0m",
                        total_passed
                    );
                } else {
                    println!(
                        "\x1b[31m\u{2717} {}/{} scenario(s) failed\x1b[0m",
                        total_failed, total_scenarios
                    );
                }
                if total_failed > 0 {
                    std::process::exit(1);
                }
            }
        },
    }
}
/// Collect .px file paths from arguments (files or directories, up to 2 levels deep).
fn collect_px_files(args: &[String]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for arg in args {
        let p = PathBuf::from(arg);
        if p.is_file() {
            paths.push(p);
        } else if p.is_dir() {
            collect_px_in_dir(&p, &mut paths, 2);
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn collect_px_in_dir(dir: &std::path::Path, paths: &mut Vec<PathBuf>, depth: usize) {
    if depth == 0 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let ep = entry.path();
            if ep.is_file() && ep.extension().map(|e| e == "px").unwrap_or(false) {
                paths.push(ep);
            } else if ep.is_dir() {
                collect_px_in_dir(&ep, paths, depth - 1);
            }
        }
    }
}
