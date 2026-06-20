use std::process::Command;

fn main() {
    // Embed git commit hash at compile time.
    // Strategy: git CLI → GIT_COMMIT_HASH env var → .git/HEAD file → Cargo.toml version
    let output = git_hash_from_cli()
        .or_else(git_hash_from_env)
        .or_else(git_hash_from_head_file)
        .unwrap_or_else(|| {
            // In sandboxed builds (Nix, Docker) with no git access, use the package version.
            // This IS the release version from the tag that triggered the build.
            format!("v{}", env!("CARGO_PKG_VERSION"))
        });

    println!("cargo:rustc-env=GIT_COMMIT_HASH={output}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");
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
