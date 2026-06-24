//! `agens-plugin` - Pares Agens as the host runtime + cognition composition.
//!
//! This crate is the entire agens product after the Stage-4 collapse, and after
//! Stage R3a it IS the host (Option A): it owns the `run_with_providers` host
//! composition seam directly (see [`host_runtime`]) rather than depending on
//! `pares-radix-cli-runtime`.
//!
//! - It path-deps the agens HOST/COGNITION crates (`pares-agens-core`
//!   spine/agent, `pares-agens-channels`, `pares-agens-models`,
//!   `pares-agens-bitnet`, `pares-agens-agenda`, `pares-agens-tui`) — the LOCAL
//!   agens copies (Stage R3a pin-flip), so there is exactly ONE
//!   `pares-agens-core` in the graph.
//! - It git-pins the radix PLATFORM crates it composes (`pares-radix-cli-api`,
//!   `pares-radix-praxis`, `pares-radix-core`, `pares-radix-cli`/migrate,
//!   `pares-radix-mcp-server`, `pares-rector`).
//! - It implements [`pares_radix_cli_api::CommandProvider`] ([`AgensProvider`]) to
//!   contribute the agent subcommands (`serve-spine`, `serve`, `tui`, `ask`,
//!   `classify`) to its own host CLI.
//! - It brings its own agent IP: the [`headroom`] context-compression capability
//!   (carved out of the deleted agens-core fork) and the agens model/bitnet wiring.
//!
//! The dependency arrow is strictly `agens-plugin -> pares-radix (as lib)`. Radix
//! never depends on agens (invariant).

pub mod agent_commands;
pub mod headroom;
pub mod host_runtime;
pub mod self_update;

pub use agent_commands::AgensProvider;
