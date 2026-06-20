//! `agens-plugin` - Pares Agens as a thin plugin over `pares-radix` as a library.
//!
//! This crate is the entire agens product after the Stage-4 collapse:
//!
//! - It depends on `pares-radix-cli-runtime` (the host runtime + `run_with_providers`
//!   composition seam) and the radix-presented capability crates
//!   (`pares-agens-core` spine/agent, `pares-agens-channels`, `pares-models`,
//!   `pares-agens-bitnet`).
//! - It implements [`pares_radix_cli_api::CommandProvider`] ([`AgensProvider`]) to
//!   contribute the agent subcommands (`serve-spine`, `serve`, `tui`, `ask`,
//!   `classify`) to the host CLI without the host depending on agens.
//! - It brings its own agent IP: the [`headroom`] context-compression capability
//!   (carved out of the deleted agens-core fork) and the agens model/bitnet wiring.
//!
//! The dependency arrow is strictly `agens-plugin -> pares-radix (as lib)`. Radix
//! never depends on agens (invariant).

pub mod headroom;
