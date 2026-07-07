//! `agens-host` - the agens plugin host binary. Composes the pares-radix host
//!
//! Composes the host runtime (`crate::host_runtime::run_with_providers`, the
//! host composition seam RELOCATED into this crate in Stage R3a) with the agens
//! [`AgensProvider`], which contributes the agent subcommands (`serve-spine`,
//! `serve`, `tui`, `ask`, `classify`). The host owns the command surface; this
//! binary hands it the single provider (decision C1, Option B — the former
//! `ProviderRegistry` indirection was dropped when `pares_radix_cli_api` was
//! removed upstream at v1.55.13).

use agens_plugin::host_runtime::run_with_providers;
use agens_plugin::AgensProvider;

#[tokio::main]
async fn main() {
    run_with_providers(AgensProvider::new()).await;
}
