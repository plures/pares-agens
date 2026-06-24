//! `pares-agens` - the agens plugin binary (praxisbot).
//!
//! Composes the host runtime (`crate::host_runtime::run_with_providers`, the
//! host composition seam RELOCATED into this crate in Stage R3a) with the agens
//! [`AgensProvider`], which contributes the agent subcommands (`serve-spine`,
//! `serve`, `tui`, `ask`, `classify`). The host owns the command surface; this
//! binary registers the provider into it (decision C1).

use agens_plugin::host_runtime::{run_with_providers, ProviderRegistry};
use agens_plugin::AgensProvider;

#[tokio::main]
async fn main() {
    let registry = ProviderRegistry::new().register(Box::new(AgensProvider::new()));
    run_with_providers(registry).await;
}
