//! Integration tests for `pares-agens-bitnet`.
//!
//! These tests exercise the stub path (no `inference` feature) and the safe
//! API surface.  Full end-to-end generation tests require the `inference`
//! feature plus a real model file, so they are skipped here.

use pares_agens_bitnet::{BitNetRunner, GenParams, InferenceError};
use std::path::Path;

// ── Stub path tests (always run, no native library required) ──────────────────

#[test]
fn runner_load_returns_native_unavailable_without_inference_feature() {
    // This test only asserts the stub behaviour when `inference` is not
    // compiled in.  When it IS enabled and the model file does not exist we
    // expect ModelLoad instead.
    let result = BitNetRunner::load(Path::new("nonexistent.bin"));

    #[cfg(not(feature = "inference"))]
    {
        assert!(
            matches!(result, Err(InferenceError::NativeUnavailable)),
            "expected NativeUnavailable without inference feature, got: {result:?}"
        );
    }

    #[cfg(feature = "inference")]
    {
        assert!(
            matches!(result, Err(InferenceError::ModelLoad { .. })),
            "expected ModelLoad with inference feature but missing file, got: {result:?}"
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
        InferenceError::Tokenise("bad input".into()),
        InferenceError::TokenDecode {
            token: 42,
            reason: "unknown".into(),
        },
        InferenceError::Eval(-1),
        InferenceError::Sample(-2),
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
    assert!(
        p.seed.is_none(),
        "default seed should be None (time-based)"
    );
}
