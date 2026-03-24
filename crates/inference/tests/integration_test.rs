//! Integration tests for `pares-agens-inference`.
//!
//! These tests exercise the public API surface and stub paths (no `native`
//! feature required).  Full end-to-end generation requires the `native`
//! feature plus a real model file.

use std::path::Path;

use pares_agens_inference::{
    BitNetLocalRunner, GenParams, InferenceConfig, InferenceError, ModelDownloader,
    ModelRegistry,
};

// ── Stub path tests (always run, no native library required) ──────────────────

#[test]
fn runner_load_returns_native_unavailable_without_native_feature() {
    let result = BitNetLocalRunner::load(Path::new("nonexistent.bin"), "test-model");

    #[cfg(not(feature = "native"))]
    {
        assert!(
            matches!(result, Err(InferenceError::NativeUnavailable)),
            "expected NativeUnavailable without native feature, got: {result:?}"
        );
    }

    #[cfg(feature = "native")]
    {
        assert!(
            matches!(result, Err(InferenceError::ModelLoad { .. })),
            "expected ModelLoad with native feature but missing file, got: {result:?}"
        );
    }
}

#[test]
fn inference_error_messages_are_non_empty() {
    let errors: &[InferenceError] = &[
        InferenceError::NativeUnavailable,
        InferenceError::ModelLoad {
            path: "a.bin".into(),
            reason: "not found".into(),
        },
        InferenceError::ContextCreate("oom".into()),
        InferenceError::OutOfMemory("allocation failed".into()),
        InferenceError::CorruptModel {
            path: "bad.bin".into(),
            reason: "magic mismatch".into(),
        },
        InferenceError::Tokenise("bad input".into()),
        InferenceError::TokenDecode {
            token: 42,
            reason: "unknown".into(),
        },
        InferenceError::Eval(-1),
        InferenceError::Sample(-2),
        InferenceError::ChannelClosed,
    ];

    for e in errors {
        let msg = e.to_string();
        assert!(!msg.is_empty(), "error variant has empty Display: {e:?}");
    }
}

#[test]
fn gen_params_default_values_are_sensible() {
    let p = GenParams::default();
    assert!(p.temperature > 0.0, "temperature must be positive");
    assert!(p.top_p > 0.0 && p.top_p <= 1.0, "top_p must be in (0, 1]");
    assert!(p.max_tokens > 0, "max_tokens must be positive");
    assert!(p.n_threads > 0, "n_threads must be positive");
    assert!(p.seed.is_none(), "default seed should be None");
    assert!(p.stop_sequences.is_empty(), "default stop_sequences should be empty");
}

#[test]
fn gen_params_stop_sequences_are_included() {
    let p = GenParams {
        stop_sequences: vec!["</s>".to_string(), "[END]".to_string()],
        ..GenParams::default()
    };
    assert_eq!(p.stop_sequences.len(), 2);
    assert!(p.stop_sequences.contains(&"</s>".to_string()));
}

#[test]
fn inference_config_default_values_are_sensible() {
    let cfg = InferenceConfig::default();
    assert!(!cfg.model_dir.as_os_str().is_empty());
    assert!(cfg.default_params.max_tokens > 0);
}

// ── ModelRegistry ─────────────────────────────────────────────────────────────

#[test]
fn registry_register_and_get() {
    let mut registry = ModelRegistry::new();
    registry.register(
        "bitnet-b1.58-3b",
        std::path::PathBuf::from("models/bitnet-3b.bin"),
        "BitNet 1.58-bit 3B",
    );

    let entry = registry.get("bitnet-b1.58-3b").expect("entry should be present");
    assert_eq!(entry.model_id, "bitnet-b1.58-3b");
    assert_eq!(entry.description, "BitNet 1.58-bit 3B");
}

#[test]
fn registry_get_missing_returns_none() {
    let registry = ModelRegistry::new();
    assert!(registry.get("does-not-exist").is_none());
}

#[test]
fn registry_remove_existing_entry() {
    let mut registry = ModelRegistry::new();
    registry.register("m1", std::path::PathBuf::from("m1.bin"), "M1");
    assert!(registry.remove("m1").is_some());
    assert!(registry.get("m1").is_none());
}

#[test]
fn registry_remove_missing_returns_none() {
    let mut registry = ModelRegistry::new();
    assert!(registry.remove("nope").is_none());
}

#[test]
fn registry_scan_dir_counts_bin_files() {
    let tmp = tempdir();
    // Create two .bin files and one non-.bin file.
    std::fs::write(tmp.join("model-a.bin"), b"").unwrap();
    std::fs::write(tmp.join("model-b.bin"), b"").unwrap();
    std::fs::write(tmp.join("readme.txt"), b"").unwrap();

    let mut registry = ModelRegistry::new();
    let added = registry.scan_dir(&tmp).expect("scan_dir should succeed");
    assert_eq!(added, 2, "should have found 2 .bin files");
    assert!(registry.get("model-a").is_some());
    assert!(registry.get("model-b").is_some());
    assert!(registry.get("readme").is_none());
}

// ── ModelDownloader ───────────────────────────────────────────────────────────

#[test]
fn downloader_model_path_uses_bin_extension() {
    let dl = ModelDownloader::new("/models");
    let path = dl.model_path("my-model");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("bin"));
    assert!(path.to_str().unwrap().contains("my-model"));
}

#[test]
fn downloader_verify_local_returns_false_for_missing_file() {
    let dl = ModelDownloader::new("/tmp/definitely-does-not-exist-pares-agens-test");
    assert!(!dl.verify_local("nonexistent-model"));
}

#[test]
fn downloader_install_and_evict_roundtrip() {
    let tmp = tempdir();
    let dl = ModelDownloader::new(tmp.join("models"));

    // Create a fake source model file.
    let src = tmp.join("source.bin");
    std::fs::write(&src, b"fake model content").unwrap();

    // Install it.
    let dest = dl.install_from("my-model", &src).expect("install_from should succeed");
    assert!(dest.is_file(), "installed file should exist");
    assert!(dl.verify_local("my-model"), "verify_local should return true after install");

    // Evict it.
    let removed = dl.evict("my-model").expect("evict should succeed");
    assert!(removed, "evict should return true when file existed");
    assert!(!dl.verify_local("my-model"), "verify_local should return false after eviction");

    // Evicting again should return false, not an error.
    let removed_again = dl.evict("my-model").expect("second evict should not error");
    assert!(!removed_again);
}

// ── Async stub tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn generate_returns_native_unavailable_without_native_feature() {
    let result = BitNetLocalRunner::load(Path::new("x.bin"), "x");

    #[cfg(not(feature = "native"))]
    {
        assert!(
            matches!(result, Err(InferenceError::NativeUnavailable)),
            "expected NativeUnavailable, got: {result:?}"
        );
    }

    // With native feature we expect ModelLoad (file doesn't exist).
    #[cfg(feature = "native")]
    {
        assert!(
            matches!(result, Err(InferenceError::ModelLoad { .. })),
            "expected ModelLoad, got: {result:?}"
        );
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// RAII temporary directory that is deleted when dropped.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pares-agens-inference-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl std::ops::Deref for TempDir {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Create a unique temporary directory that is automatically cleaned up.
fn tempdir() -> TempDir {
    TempDir::new()
}
