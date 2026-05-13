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
/// The command handles real-world conditions that would break a naive
/// `git pull && cargo build`:
///
/// 1. **Dirty working tree** — `Cargo.lock` (and other files) may have local
///    modifications from previous builds. We reset tracked files and clean
///    untracked files before pulling.
///
/// 2. **Diverged history** — `git pull --ff-only` fails when the local branch
///    has diverged. We use `git fetch + reset --hard` instead.
///
/// 3. **Wrong package name** — The workspace has many crates. We verify the
///    target package exists via `cargo metadata` before building.
///
/// 4. **Correct package name** — The binary crate is `pares-agens` (in
///    `crates/migrate/`), not `pares-agens-cli` or any other variant.
///
/// The `_flake_dir` and `_host` parameters are retained for backward
/// compatibility with callers but are not used by the current implementation.
pub fn build_update_command(_flake_dir: &str, _host: &str) -> String {
    let agens_dir = shell_single_quote(&resolve_agens_dir());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/kbristol".into());
    let bin_dir = format!("{home}/.local/bin");
    format!(
        "set -eu; \
         echo 'Step 1: Preparing source tree...'; \
         cd {agens_dir}; \
         git checkout -- Cargo.lock 2>/dev/null || true; \
         git clean -fd 2>/dev/null || true; \
         echo 'Step 2: Pulling latest pares-agens source...'; \
         git fetch origin main && git reset --hard origin/main; \
         echo 'Step 3: Verifying workspace...'; \
         cargo metadata --no-deps --format-version 1 2>/dev/null | grep -q '\"pares-agens\"' || {{ echo 'ERROR: pares-agens package not found in workspace'; exit 1; }}; \
         echo 'Step 4: Building pares-agens binary...'; \
         cargo build --release -p pares-agens 2>&1; \
         echo 'Step 5: Installing binary...'; \
         mkdir -p {bin_dir}; \
         cp target/release/pares-agens {bin_dir}/pares-agens; \
         echo 'Step 6: Restarting service...'; \
         sudo systemctl restart pares-agens; \
         echo 'Self-update complete. New binary installed and service restarted.'"
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
        assert!(cmd.contains("git checkout -- Cargo.lock"), "must reset Cargo.lock");
        assert!(cmd.contains("git clean -fd"), "must clean untracked files");
    }

    #[test]
    fn update_command_uses_hard_reset() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("git fetch origin main"), "must fetch latest");
        assert!(cmd.contains("git reset --hard origin/main"), "must hard-reset");
        assert!(!cmd.contains("git pull"), "must not use fragile git pull");
    }

    #[test]
    fn update_command_verifies_workspace() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("cargo metadata"), "must verify workspace before build");
    }

    #[test]
    fn update_command_uses_correct_package_name() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("cargo build --release -p pares-agens"), "must use correct name");
        assert!(!cmd.contains("pares-agens-cli"), "must not use wrong name");
    }

    #[test]
    fn update_command_restarts_service() {
        let cmd = build_update_command("/etc/nixos", "praxisbot");
        assert!(cmd.contains("systemctl restart pares-agens"));
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
