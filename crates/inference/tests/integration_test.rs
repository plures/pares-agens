//! Integration tests for `pares-agens-inference`.
//!
//! These tests exercise the public API surface and stub paths (no `native`
//! feature required).  Full end-to-end generation requires the `native`
//! feature plus a real model file.

use std::path::Path;

use pares_agens_inference::{
    default_cache_dir, BitNetLocalRunner, GenParams, InferenceConfig, InferenceError,
    LocalModelsConfig, ModelDownloader, ModelRegistry,
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
    assert!(
        p.stop_sequences.is_empty(),
        "default stop_sequences should be empty"
    );
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

    let entry = registry
        .get("bitnet-b1.58-3b")
        .expect("entry should be present");
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
    let dest = dl
        .install_from("my-model", &src)
        .expect("install_from should succeed");
    assert!(dest.is_file(), "installed file should exist");
    assert!(
        dl.verify_local("my-model"),
        "verify_local should return true after install"
    );

    // Evict it.
    let removed = dl.evict("my-model").expect("evict should succeed");
    assert!(removed, "evict should return true when file existed");
    assert!(
        !dl.verify_local("my-model"),
        "verify_local should return false after eviction"
    );

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

// ── New error variants ────────────────────────────────────────────────────────

#[test]
fn download_error_has_non_empty_message() {
    let e = InferenceError::Download {
        repo: "owner/repo".to_string(),
        reason: "connection refused".to_string(),
    };
    assert!(!e.to_string().is_empty());
    assert!(e.to_string().contains("owner/repo"));
}

#[test]
fn model_not_found_error_has_non_empty_message() {
    let e = InferenceError::ModelNotFound {
        repo: "owner/missing".to_string(),
    };
    assert!(!e.to_string().is_empty());
    assert!(e.to_string().contains("owner/missing"));
}

// ── ModelEntry fields ─────────────────────────────────────────────────────────

#[test]
fn model_entry_has_file_size_and_capabilities() {
    let tmp = tempdir();
    let path = tmp.join("model.gguf");
    std::fs::write(&path, vec![0u8; 2048]).unwrap();

    let mut registry = ModelRegistry::new();
    registry.register("test-model", path, "A test model");

    let entry = registry.get("test-model").unwrap();
    assert_eq!(entry.file_size_bytes, Some(2048));
    assert!(entry.capabilities.is_empty());
}

#[test]
fn registry_scan_dir_counts_gguf_files() {
    let tmp = tempdir();
    std::fs::write(tmp.join("model-x.gguf"), b"gguf content").unwrap();
    std::fs::write(tmp.join("model-y.bin"), b"bin content").unwrap();
    std::fs::write(tmp.join("readme.txt"), b"ignore").unwrap();

    let mut registry = ModelRegistry::new();
    let added = registry.scan_dir(&tmp).expect("scan_dir should succeed");
    assert_eq!(added, 2, "should count both .gguf and .bin files");
    assert!(registry.get("model-x").is_some());
    assert!(registry.get("model-y").is_some());
    assert!(registry.get("readme").is_none());
}

#[test]
fn registry_scan_dir_populates_file_size() {
    let tmp = tempdir();
    std::fs::write(tmp.join("sized-model.gguf"), vec![42u8; 512]).unwrap();

    let mut registry = ModelRegistry::new();
    registry.scan_dir(&tmp).unwrap();

    let entry = registry.get("sized-model").unwrap();
    assert_eq!(entry.file_size_bytes, Some(512));
}

#[test]
fn registry_auto_detect_scans_supported_files() {
    let tmp = tempdir();
    std::fs::write(tmp.join("auto-a.gguf"), b"gguf").unwrap();
    std::fs::write(tmp.join("auto-b.bin"), b"bin").unwrap();
    std::fs::write(tmp.join("notes.md"), b"ignore").unwrap();

    let registry = ModelRegistry::auto_detect(&tmp).expect("auto_detect should succeed");
    assert!(registry.get("auto-a").is_some());
    assert!(registry.get("auto-b").is_some());
    assert!(registry.get("notes").is_none());
}

#[test]
fn registry_auto_detect_missing_dir_returns_empty_registry() {
    let tmp = tempdir();
    let missing = tmp.join("missing");

    let registry = ModelRegistry::auto_detect(&missing).expect("missing dir should not error");
    assert!(registry.entries().next().is_none());
}

#[test]
fn registry_register_full_sets_all_fields() {
    let mut registry = ModelRegistry::new();
    registry.register_full(
        "full-model",
        std::path::PathBuf::from("full.gguf"),
        "Full model",
        Some(1024),
        vec!["chat".to_string(), "instruct".to_string()],
    );
    let entry = registry.get("full-model").unwrap();
    assert_eq!(entry.file_size_bytes, Some(1024));
    assert_eq!(entry.capabilities, vec!["chat", "instruct"]);
}

// ── LocalModelsConfig ─────────────────────────────────────────────────────────

#[test]
fn local_models_config_default_has_expected_fields() {
    let cfg = LocalModelsConfig::default();
    assert!(!cfg.cache_dir.as_os_str().is_empty());
    assert!(cfg.default_model.is_none());
}

#[test]
fn local_models_config_serialization_roundtrip() {
    let original = LocalModelsConfig {
        cache_dir: std::path::PathBuf::from("/tmp/models"),
        default_model: Some("BitNet-b1.58-2B-4T".to_string()),
    };
    let json = serde_json::to_string(&original).unwrap();
    let decoded: LocalModelsConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.cache_dir, original.cache_dir);
    assert_eq!(decoded.default_model, original.default_model);
}

// ── InferenceConfig.local_models ──────────────────────────────────────────────

#[test]
fn inference_config_local_models_default() {
    let cfg = InferenceConfig::default();
    assert!(cfg.local_models.default_model.is_none());
    assert!(!cfg.local_models.cache_dir.as_os_str().is_empty());
}

// ── default_cache_dir ─────────────────────────────────────────────────────────

#[test]
fn default_cache_dir_ends_with_expected_components() {
    let dir = default_cache_dir();
    let s = dir.to_string_lossy();
    assert!(
        s.contains(".pares-agens"),
        "expected `.pares-agens` in cache dir: {s}"
    );
    assert!(
        s.ends_with("models"),
        "expected path to end with `models`: {s}"
    );
}

// ── ModelDownloader extended ──────────────────────────────────────────────────

#[test]
fn downloader_verify_local_checks_gguf_variant() {
    let tmp = tempdir();
    let dl = ModelDownloader::new(&*tmp);
    let path = tmp.join("my-model.gguf");
    std::fs::write(&path, b"data").unwrap();
    assert!(
        dl.verify_local("my-model"),
        "verify_local should find .gguf file"
    );
}

#[test]
fn downloader_evict_removes_gguf_file() {
    let tmp = tempdir();
    let dl = ModelDownloader::new(&*tmp);
    let path = tmp.join("evict-me.gguf");
    std::fs::write(&path, b"data").unwrap();

    let removed = dl.evict("evict-me").unwrap();
    assert!(removed, "evict should return true for existing .gguf file");
    assert!(!path.exists(), "gguf file should be deleted");
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
