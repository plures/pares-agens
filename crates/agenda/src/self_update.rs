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

/// Build a resilient self-update shell command.
///
/// **Bootstrap-safe design:** The binary invokes an external script
/// (`scripts/self-update.sh`) rather than embedding the update procedure.
/// This means pulling new source automatically updates the update procedure
/// itself — solving the bootstrap problem where a broken embedded command
/// can never fix itself.
///
/// The flow:
/// 1. `git fetch + reset --hard` to get latest source (including the script)
/// 2. Run the now-updated `scripts/self-update.sh`
///
/// The script handles: dirty trees, lock conflicts, workspace verification,
/// correct package names, binary installation, and service restart.
///
/// The `_flake_dir` and `_host` parameters are retained for backward
/// compatibility with callers but are not used by the current implementation.
pub fn build_update_command(_flake_dir: &str, _host: &str) -> String {
    let agens_dir = shell_single_quote(&resolve_agens_dir());
    // Step 1 is a minimal bootstrap: fetch + reset to get the latest script.
    // Step 2 delegates everything to the external script which is now up-to-date.
    // This two-phase approach means the embedded command is tiny and stable —
    // all the real logic lives in the script that just got pulled.
    format!(
        "set -eu; \
         cd {agens_dir}; \
         git checkout -- . 2>/dev/null || true; \
         git fetch origin main && git reset --hard origin/main; \
         exec bash scripts/self-update.sh"
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
    fn update_command_resets_dirty_tree() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("git checkout -- ."), "must reset dirty tree");
    }

    #[test]
    fn update_command_uses_hard_reset() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("git fetch origin main"), "must fetch latest");
        assert!(cmd.contains("git reset --hard origin/main"), "must hard-reset");
        assert!(!cmd.contains("git pull"), "must not use fragile git pull");
    }

    #[test]
    fn update_command_delegates_to_external_script() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("exec bash scripts/self-update.sh"), "must invoke external script");
        // The embedded command must be minimal — no cargo build, no package names.
        // All that logic lives in the script which gets pulled fresh.
        assert!(!cmd.contains("cargo build"), "must not embed build command");
        assert!(!cmd.contains("pares-agens-cli"), "must not embed wrong package name");
    }

    #[test]
    fn update_command_is_bootstrap_safe() {
        // The key property: the embedded command only does fetch+reset+exec.
        // Even if the script itself is broken, pulling replaces it before exec runs.
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        let steps: Vec<&str> = cmd.split(';').map(|s| s.trim()).collect();
        // Should be: set -eu, cd, git checkout, git fetch && reset, exec bash
        assert!(steps.len() <= 6, "embedded command must be minimal (got {} steps)", steps.len());
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
