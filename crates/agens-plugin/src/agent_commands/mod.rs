//! Agent command surface contributed by the agens plugin (Stage 4c).
//!
//! [`AgensProvider`] provides the agens agent command surface consumed by the
//! host composition ([`crate::host_runtime`]): it augments the host `clap`
//! command with the five agent subcommands
//! (`serve-spine`, `serve`, `tui`, `ask`, `classify`) and dispatches each to the
//! handler fns carved out of `pares-radix-cli-runtime` (see [`handlers`]).
//!
//! The host (`pares-radix`) no longer carries any agent command; this module is
//! the new home. Arg definitions mirror the former derive-`Commands` variants
//! exactly (long names = kebab-case of the field, same env vars / defaults).
//!
//! NOTE: the former `pares_radix_cli_api::CommandProvider` trait was removed
//! upstream at pares-radix v1.55.13 (breaking commit 3172cfa). `augment`/`handle`
//! are now inherent methods on [`AgensProvider`] that the agens host calls
//! directly (Option B — agens is the sole host of a single provider).

// BitNet-backed cerebellum classifier. Real implementation carved from the radix
// host in Stage 4c (commit d823bbb) but not yet wired into the routing path, so
// its items are currently dead. `#[allow(dead_code)]` here (consistent with the
// crate's existing not-yet-wired allowances) keeps `clippy -D warnings` green
// until the classifier is connected; it is NOT stubbed — the logic is complete.
// TODO(agens): wire BitNetClassifier into AgensProvider message routing.
#[allow(dead_code)]
mod bitnet_classifier;
mod config;
mod px_config;
mod runtime;

use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::PathBuf;

/// The agens command provider. The agens host ([`crate::host_runtime::run_with_providers`])
/// constructs one of these and calls [`AgensProvider::augment`] +
/// [`AgensProvider::handle`] directly to contribute the agent subcommands.
/// (The host composition seam was relocated into this crate at Stage R3a; the
/// former `pares_radix_cli_api::CommandProvider` trait was removed upstream at
/// v1.55.13, so these are now inherent methods rather than a trait impl.)
pub struct AgensProvider;

impl AgensProvider {
    /// Construct the provider.
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgensProvider {
    fn default() -> Self {
        Self::new()
    }
}

// --- small ArgMatches helpers (the host parsed these; we re-read them) ---

fn s(m: &ArgMatches, id: &str) -> String {
    m.get_one::<String>(id).cloned().unwrap_or_default()
}
fn os(m: &ArgMatches, id: &str) -> Option<String> {
    m.get_one::<String>(id).cloned()
}
fn b(m: &ArgMatches, id: &str) -> bool {
    m.get_flag(id)
}
fn op(m: &ArgMatches, id: &str) -> Option<PathBuf> {
    m.get_one::<String>(id).map(PathBuf::from)
}

/// Build a `--long`, takes-value arg with optional env + default.
fn opt(id: &'static str, env: Option<&'static str>, default: Option<&'static str>) -> Arg {
    let long: &'static str = Box::leak(id.replace('_', "-").into_boxed_str());
    let mut a = Arg::new(id).long(long).num_args(1);
    if let Some(e) = env {
        a = a.env(e);
    }
    if let Some(d) = default {
        a = a.default_value(d);
    }
    a
}

/// Build a boolean flag arg (with optional env).
fn flag(id: &'static str, env: Option<&'static str>) -> Arg {
    let long: &'static str = Box::leak(id.replace('_', "-").into_boxed_str());
    let mut a = Arg::new(id)
        .long(long)
        .action(ArgAction::SetTrue);
    if let Some(e) = env {
        a = a.env(e);
    }
    a
}

impl AgensProvider {
    /// The provider name (used for logging/diagnostics).
    pub fn name(&self) -> &str {
        "agens"
    }

    /// Augment the host `clap` command with the agens agent subcommands.
    pub fn augment(&self, cmd: Command) -> Command {
        let serve_spine = Command::new("serve-spine")
            .about("Run the agent using the spine-driven pipeline (ADR-0001).")
            .arg(opt("config", Some("PARES_CONFIG"), None))
            .arg(opt("channel", Some("PARES_CHANNEL"), Some("telegram")))
            .arg(opt(
                "telegram_token",
                Some("PARES_TELEGRAM_TOKEN"),
                Some(""),
            ))
            .arg(opt("http_port", Some("PARES_HTTP_PORT"), Some("3200")))
            .arg(opt(
                "model_url",
                Some("PARES_MODEL_URL"),
                Some("https://models.inference.ai.azure.com"),
            ))
            .arg(opt("model", Some("PARES_MODEL"), Some("gpt-4o")))
            .arg(flag("use_copilot", Some("PARES_USE_COPILOT")));

        let serve = Command::new("serve")
            .about("Run the agent as a headless daemon with a channel adapter.")
            .arg(opt(
                "telegram_token",
                Some("PARES_TELEGRAM_TOKEN"),
                Some(""),
            ))
            .arg(opt(
                "model_url",
                Some("PARES_MODEL_URL"),
                Some("https://models.inference.ai.azure.com"),
            ))
            .arg(opt("model", Some("PARES_MODEL"), Some("auto")))
            .arg(flag("copilot", None))
            .arg(opt("deep_model", Some("PARES_DEEP_MODEL"), Some("auto")))
            .arg(opt("fast_model", Some("PARES_FAST_MODEL"), Some("auto")))
            .arg(opt("deep_model_url", Some("PARES_DEEP_MODEL_URL"), None))
            .arg(opt("api_key", Some("PARES_API_KEY"), None))
            .arg(opt("embed_url", Some("PARES_EMBED_URL"), None))
            .arg(opt(
                "embed_model",
                Some("PARES_EMBED_MODEL"),
                Some("nomic-embed-text"),
            ))
            .arg(opt("system_prompt", None, None))
            .arg(opt("brave_api_key", Some("BRAVE_API_KEY"), None))
            .arg(opt(
                "manus_ws_url",
                Some("PARES_MANUS_WS_URL"),
                Some("ws://127.0.0.1:18790"),
            ))
            .arg(opt("sync_topic_key", Some("PARES_SYNC_TOPIC_KEY"), None))
            .arg(opt("sync_shared_key", Some("PARES_SYNC_SHARED_KEY"), None))
            .arg(flag("no_event_spine", Some("PARES_NO_EVENT_SPINE")))
            .arg(opt(
                "bitnet_model_path",
                Some("PARES_BITNET_MODEL_PATH"),
                None,
            ))
            .arg(opt(
                "cerebellum_model_path",
                Some("PARES_CEREBELLUM_MODEL_PATH"),
                None,
            ));

        let tui = Command::new("tui")
            .about("Run the agent with an interactive terminal UI.")
            .arg(opt(
                "model_url",
                Some("PARES_MODEL_URL"),
                Some("https://models.inference.ai.azure.com"),
            ))
            .arg(opt("model", Some("PARES_MODEL"), Some("claude-sonnet-4.5")))
            .arg(flag("copilot", None))
            .arg(opt("api_key", Some("PARES_API_KEY"), None))
            .arg(opt("system_prompt", None, None))
            .arg(opt("bitnet_model_path", Some("PARES_BITNET_MODEL_PATH"), None))
            .arg(opt(
                "cerebellum_model_path",
                Some("PARES_CEREBELLUM_MODEL_PATH"),
                None,
            ));

        let ask = Command::new("ask")
            .about("Send a single prompt and print the response (non-interactive).")
            .arg(Arg::new("prompt").required(true).num_args(1))
            .arg(opt(
                "model_url",
                Some("PARES_MODEL_URL"),
                Some("https://models.inference.ai.azure.com"),
            ))
            .arg(opt("model", Some("PARES_MODEL"), Some("claude-sonnet-4.5")))
            .arg(flag("copilot", None))
            .arg(opt("api_key", Some("PARES_API_KEY"), None))
            .arg(opt("bitnet_model_path", None, None))
            .arg(opt("cerebellum_model_path", None, None))
            .arg(opt("system_prompt", None, None))
            .arg(opt("format", None, Some("text")));

        let cmd = cmd
            .subcommand(serve_spine)
            .subcommand(serve)
            .subcommand(tui)
            .subcommand(ask);

        #[cfg(feature = "bitnet-native")]
        let cmd = cmd.subcommand(
            Command::new("classify")
                .about("Test the cerebellum classifier on a message (non-interactive).")
                .arg(Arg::new("message").required(true).num_args(1))
                .arg(opt("bitnet_model_path", None, None).required(true)),
        );

        cmd
    }

    /// Dispatch a parsed subcommand. Returns `None` if `name` is not an agens
    /// command (the host should fall through to its own dispatch), or
    /// `Some(Ok(()))` / `Some(Err(msg))` when handled.
    pub async fn handle(&self, name: &str, m: &ArgMatches) -> Option<Result<(), String>> {
        // The agent command futures are intentionally `!Send` (they hold
        // tracing `&dyn Value` temporaries across awaits inside the radix spine
        // bootstrap, and drive single-threaded TUI/event loops). `handle` must
        // be `Send` (async-trait), so we drive each command to completion on a
        // dedicated OS thread with its own current-thread Tokio runtime and join
        // it. These commands own the whole process lifetime (serve runs until
        // shutdown; ask/classify run to completion then the host returns), so
        // blocking the dispatch thread here is the same control flow the host's
        // `run_with_providers` match arms had before the carve.
        macro_rules! run_on_local_rt {
            ($fut:expr) => {{
                let join = std::thread::Builder::new()
                    .name(format!("agens-{}", name))
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("failed to build agens command runtime");
                        rt.block_on($fut);
                    })
                    .expect("failed to spawn agens command thread");
                match join.join() {
                    Ok(()) => Some(Ok(())),
                    Err(_) => Some(Err(format!("agens command '{name}' panicked"))),
                }
            }};
        }

        match name {
            "serve-spine" => {
                let (config, channel, telegram_token, http_port, model_url, model, use_copilot) = (
                    os(m, "config"),
                    s(m, "channel"),
                    s(m, "telegram_token"),
                    s(m, "http_port").parse().unwrap_or(3200),
                    s(m, "model_url"),
                    s(m, "model"),
                    b(m, "use_copilot"),
                );
                run_on_local_rt!(runtime::run_serve_spine(
                    config,
                    channel,
                    telegram_token,
                    http_port,
                    model_url,
                    model,
                    use_copilot,
                ))
            }
            "serve" => {
                let telegram_token = s(m, "telegram_token");
                let model_url = s(m, "model_url");
                let model = s(m, "model");
                let copilot = b(m, "copilot");
                let deep_model = s(m, "deep_model");
                let fast_model = s(m, "fast_model");
                let deep_model_url = os(m, "deep_model_url");
                let api_key = os(m, "api_key");
                let embed_url = os(m, "embed_url");
                let embed_model = s(m, "embed_model");
                let system_prompt = op(m, "system_prompt");
                let brave_api_key = os(m, "brave_api_key");
                let manus_ws_url = s(m, "manus_ws_url");
                let sync_topic_key = os(m, "sync_topic_key");
                let sync_shared_key = os(m, "sync_shared_key");
                let no_event_spine = b(m, "no_event_spine");
                let bitnet_model_path = op(m, "bitnet_model_path");
                let cerebellum_model_path = op(m, "cerebellum_model_path");
                run_on_local_rt!(runtime::run_serve(
                    telegram_token,
                    model_url,
                    model,
                    copilot,
                    deep_model,
                    fast_model,
                    deep_model_url,
                    api_key,
                    embed_url,
                    embed_model,
                    system_prompt,
                    brave_api_key,
                    manus_ws_url,
                    sync_topic_key,
                    sync_shared_key,
                    no_event_spine,
                    bitnet_model_path,
                    cerebellum_model_path,
                ))
            }
            "tui" => {
                let model_url = s(m, "model_url");
                let model = s(m, "model");
                let copilot = b(m, "copilot");
                let api_key = os(m, "api_key");
                let system_prompt = op(m, "system_prompt");
                let bitnet_model_path = op(m, "bitnet_model_path");
                let cerebellum_model_path = op(m, "cerebellum_model_path");
                run_on_local_rt!(runtime::run_tui(
                    model_url,
                    model,
                    copilot,
                    api_key,
                    system_prompt,
                    bitnet_model_path,
                    cerebellum_model_path,
                ))
            }
            "ask" => {
                let prompt = s(m, "prompt");
                let model = s(m, "model");
                let copilot = b(m, "copilot");
                let bitnet_model_path = op(m, "bitnet_model_path");
                let system_prompt = op(m, "system_prompt");
                let format = s(m, "format");
                run_on_local_rt!(runtime::run_ask(
                    prompt,
                    model,
                    copilot,
                    bitnet_model_path,
                    system_prompt,
                    format,
                ))
            }
            #[cfg(feature = "bitnet-native")]
            "classify" => {
                let message = s(m, "message");
                let bitnet_model_path = op(m, "bitnet_model_path").unwrap_or_default();
                run_on_local_rt!(runtime::run_classify(message, bitnet_model_path))
            }
            _ => None,
        }
    }
}
