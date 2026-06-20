//! `pares-agens` - the agens plugin binary (praxisbot). [SCAFFOLD - provider added next step]

use pares_radix_cli_runtime::{run_with_providers, ProviderRegistry};

#[tokio::main]
async fn main() {
    // Provider registration is wired in once AgensProvider lands.
    let registry = ProviderRegistry::new();
    run_with_providers(registry).await;
}
