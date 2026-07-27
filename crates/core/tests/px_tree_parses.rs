//! CI parse-gate: the entire live `praxis/procedures/*.px` tree must parse
//! cleanly under the canonical `pares_radix_praxis::dataflow::parse_px`
//! parser (the exact function the production loader in
//! `crates/agens-plugin/src/agent_commands/runtime.rs` calls).
//!
//! This closes the loop described in the px-production-wiring-and-loader-
//! reliability ADR: a broken `.px` file used to be silently skipped by the
//! loader with zero trace. This test converts that class of bug into a
//! hard CI failure so a broken policy file can never land on `main` again.

use pares_radix_praxis::dataflow::parse_px;
use std::path::{Path, PathBuf};

/// Locate the repo's `praxis/procedures` directory relative to this crate's
/// manifest dir (`crates/core`), independent of the CI runner's cwd.
fn praxis_procedures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("praxis")
        .join("procedures")
}

#[test]
fn full_live_praxis_tree_parses_under_canonical_parser() {
    let dir = praxis_procedures_dir();
    assert!(
        dir.exists(),
        "praxis/procedures directory not found at {}",
        dir.display()
    );

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .map(|e| e.unwrap_or_else(|e| panic!("failed to read entry in {}: {e}", dir.display())))
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("px"))
        .collect();
    entries.sort();

    assert!(
        !entries.is_empty(),
        "expected to find .px files under {}, found none",
        dir.display()
    );

    let mut failures: Vec<(PathBuf, String)> = Vec::new();

    for path in &entries {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        if let Err(e) = parse_px(&source) {
            failures.push((path.clone(), e.to_string()));
        }
    }

    if !failures.is_empty() {
        let mut msg = format!(
            "{} of {} live .px file(s) FAILED to parse under the canonical grammar:\n",
            failures.len(),
            entries.len()
        );
        for (path, err) in &failures {
            msg.push_str(&format!("\n--- {} ---\n{}\n", path.display(), err));
        }
        panic!("{msg}");
    }
}
