//! `pares-agens` - the agens plugin binary (praxisbot).
//!
//! Composes the host runtime (`pares-radix-cli-runtime::run_with_providers`)
//! with the agens [`AgensProvider`], which contributes the agent subcommands
//! (`serve-spine`, `serve`, `tui`, `ask`, `classify`). The host owns the
//! command surface; this binary registers the provider into it (decision C1).

use agens_plugin::AgensProvider;
use pares_radix_cli_runtime::{run_with_providers, ProviderRegistry};

#[tokio::main]
async fn main() {
    let registry = ProviderRegistry::new().register(Box::new(AgensProvider::new()));
    run_with_providers(registry).await;
}
