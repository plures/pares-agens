//! Shared build-script helper for resolving the embedded git commit hash.
//!
//! ADR-0010 (no duplicated operational logic): the `git_hash_from_cli` /
//! `git_hash_from_env` / `git_hash_from_head_file` trio was previously
//! copy-pasted into both `crates/cli/build.rs` and
//! `crates/agens-plugin/build.rs`. It now lives here once and both build
//! scripts call [`git_commit_hash`].
//!
//! Resolution strategy (unchanged from the previous per-crate copies):
//!   1. `git rev-parse --short=8 HEAD` (normal cargo builds with git access)
//!   2. the `GIT_COMMIT_HASH` env var (set by CI / nix / sandboxed builds)
//!   3. reading `.git/HEAD` directly and resolving the ref
//!   4. `fallback_version` (the caller's package version, e.g.
//!      `format!("v{}", env!("CARGO_PKG_VERSION"))`) — this is the release
//!      version from the tag that triggered a sandboxed (Nix/Docker) build.

use std::process::Command;

/// Resolve the git commit hash to embed at build time.
///
/// `fallback_version` is used only when git is entirely unavailable (no CLI,
/// no `GIT_COMMIT_HASH`, no readable `.git/HEAD`). Callers typically pass their
/// own package version, e.g. `&format!("v{}", env!("CARGO_PKG_VERSION"))`.
pub fn git_commit_hash(fallback_version: &str) -> String {
    git_hash_from_cli()
        .or_else(git_hash_from_env)
        .or_else(git_hash_from_head_file)
        .unwrap_or_else(|| fallback_version.to_string())
}

/// Try `git rev-parse --short=8 HEAD` (works for normal cargo builds).
fn git_hash_from_cli() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Fall back to the `GIT_COMMIT_HASH` env var (can be set by CI or build systems).
fn git_hash_from_env() -> Option<String> {
    std::env::var("GIT_COMMIT_HASH")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Fall back to reading `.git/HEAD` directly and resolving a ref.
fn git_hash_from_head_file() -> Option<String> {
    let head = std::fs::read_to_string(".git/HEAD").ok()?;
    let head = head.trim();
    let full_hash = if let Some(ref_path) = head.strip_prefix("ref: ") {
        std::fs::read_to_string(format!(".git/{ref_path}")).ok()?
    } else {
        // Detached HEAD - HEAD contains the hash directly
        head.to_string()
    };
    let trimmed = full_hash.trim();
    if trimmed.len() >= 8 {
        Some(trimmed[..8].to_string())
    } else {
        None
    }
}
