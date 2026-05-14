//! `self_update` — resilient self-update command builder for pares-agens.
//!
//! Single source of truth for the self-update shell command used by both
//! the Telegram `/update` handler and the scheduled self-update task.
//!
//! See ADR-0010 for the rationale behind centralizing this logic.

/// Default source directory under `$HOME/projects/`.
pub const DEFAULT_PARES_AGENS_DIR: &str = "pares-agens";
/// Subdirectory under `$HOME` for project sources.
pub const PROJECTS_SUBDIR: &str = "projects";
/// Default self-update interval in seconds (1 hour).
pub const DEFAULT_SELF_UPDATE_INTERVAL_SECS: u64 = 3600;

/// Escape a string for safe inclusion in a single-quoted shell argument.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Resolve the pares-agens source directory.
///
/// Priority: `$PARES_AGENS_DIR` env var → `$HOME/projects/pares-agens`.
pub fn resolve_agens_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/kbristol".into());
    std::env::var("PARES_AGENS_DIR")
        .unwrap_or_else(|_| format!("{home}/{PROJECTS_SUBDIR}/{DEFAULT_PARES_AGENS_DIR}"))
}

/// Build a self-update shell command using NixOS rebuild.
///
/// This is the correct approach for NixOS-managed hosts:
/// 1. `nix flake update pares-agens` — fetch latest source via flake input
/// 2. `nixos-rebuild switch` — build in a pure sandbox, install, restart service
///
/// NixOS handles everything that manual approaches get wrong:
/// - No dirty tree problems (Nix builds from a clean source snapshot)
/// - No wrong package names (the derivation knows what to build)
/// - No bootstrap problem (the update command is minimal and stable)
/// - Service restart is automatic (systemd unit is part of the NixOS config)
///
/// The external `scripts/self-update.sh` remains as a fallback for non-NixOS
/// hosts that build from source directly.
pub fn build_update_command(flake_dir: &str, host: &str) -> String {
    let flake_dir = shell_single_quote(flake_dir);
    let host = shell_single_quote(host);
    // The flake input name varies by nixos-config (pares-radix, pares-agens, etc.).
    // Discover it dynamically from flake.nix rather than hardcoding.
    format!(
        "set -eu; \
         cd {flake_dir}; \
         FLAKE_INPUT=$(grep -oP 'pares-(?:radix|agens)(?=\\.url)' flake.nix | head -1); \
         if [ -z \"$FLAKE_INPUT\" ]; then echo 'ERROR: No pares-radix or pares-agens input found in flake.nix'; exit 1; fi; \
         echo \"Updating flake input: $FLAKE_INPUT\"; \
         lock_before=$(sha256sum flake.lock 2>/dev/null | cut -d' ' -f1 || true); \
         sudo nix flake update \"$FLAKE_INPUT\"; \
         lock_after=$(sha256sum flake.lock 2>/dev/null | cut -d' ' -f1 || true); \
         if [ \"$lock_before\" != \"$lock_after\" ]; then \
           sudo nixos-rebuild switch --flake .#{host}; \
           echo 'Self-update applied'; \
         else \
           echo 'No new commits on main'; \
         fi"
    )
}

/// Build a [`super::scheduler::Task`] for periodic self-update.
pub fn build_self_update_task(
    flake_dir: &str,
    host: &str,
    interval_secs: u64,
) -> super::scheduler::Task {
    super::scheduler::Task {
        id: "self-update.rebuild".to_string(),
        name: "Self-update via source rebuild".to_string(),
        schedule: super::scheduler::Schedule::Interval {
            every_secs: interval_secs,
        },
        command: build_update_command(flake_dir, host),
        enabled: true,
        last_run: None,
        last_result: None,
    }
}

/// Build a self-update task using environment variables or defaults.
pub fn self_update_task_from_env() -> super::scheduler::Task {
    let flake_dir =
        std::env::var("PARES_NIX_FLAKE_DIR").unwrap_or_else(|_| ".".into());
    let host = std::env::var("PARES_NIX_HOST").unwrap_or_else(|_| "praxisbot".into());
    let interval = std::env::var("PARES_SELF_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_SELF_UPDATE_INTERVAL_SECS);

    build_self_update_task(&flake_dir, &host, interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_command_discovers_flake_input() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("grep -oP"), "must discover flake input dynamically");
        assert!(cmd.contains("pares-(?:radix|agens)"), "must match either pares-radix or pares-agens");
        assert!(!cmd.contains("nix flake update pares-agens;"), "must not hardcode input name");
    }

    #[test]
    fn update_command_runs_nixos_rebuild() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("sudo nixos-rebuild switch --flake .#'praxisbot'"), "must rebuild NixOS config");
    }

    #[test]
    fn update_command_skips_rebuild_when_no_changes() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("No new commits on main"), "must skip when flake.lock unchanged");
    }

    #[test]
    fn update_command_does_not_embed_cargo_or_git() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(!cmd.contains("cargo build"), "must not embed cargo build");
        assert!(!cmd.contains("git pull"), "must not embed git pull");
        assert!(!cmd.contains("git fetch"), "must not embed git fetch");
    }

    #[test]
    fn self_update_task_defaults() {
        let task = build_self_update_task(".", "praxisbot", DEFAULT_SELF_UPDATE_INTERVAL_SECS);
        assert_eq!(task.id, "self-update.rebuild");
        assert!(task.enabled);
        match task.schedule {
            super::super::scheduler::Schedule::Interval { every_secs } => {
                assert_eq!(every_secs, DEFAULT_SELF_UPDATE_INTERVAL_SECS);
            }
            _ => panic!("expected interval schedule"),
        }
    }
}
