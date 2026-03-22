//! Build script for `pares-agens-inference`.
//!
//! When the `native` feature is enabled **and** the `bitnet.cpp` git submodule
//! is present at `crates/inference/bitnet.cpp/`, this script compiles the
//! inference engine as a static C++ library and links it into the crate.
//!
//! To initialise the submodule, run:
//! ```sh
//! git submodule update --init crates/inference/bitnet.cpp
//! ```

use std::path::Path;

fn main() {
    // Re-run if the build script itself changes.
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_FEATURE_NATIVE").is_ok() {
        compile_bitnet();
    }
}

fn compile_bitnet() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set by Cargo");
    let bitnet_root = Path::new(&manifest_dir).join("bitnet.cpp");

    if !bitnet_root.exists() {
        panic!(
            "\n\n[pares-agens-inference] bitnet.cpp submodule not found at {:?}.\n\
             Initialise it with:\n\
             \n\
             \tgit submodule update --init crates/inference/bitnet.cpp\n\n",
            bitnet_root
        );
    }

    // Detect the host triple so we can pass optimised SIMD flags.
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .flag_if_supported("-std=c++17")
        // MSVC uses a different flag for C++17; both are emitted and the
        // compiler ignores flags it does not recognise when using
        // `flag_if_supported`.
        .flag_if_supported("/std:c++17")
        .flag_if_supported("-O3")
        .flag_if_supported("-ffast-math")
        .include(bitnet_root.join("include"))
        .file(bitnet_root.join("src/bitnet_runner.cpp"))
        .file(bitnet_root.join("src/bitnet_model.cpp"))
        .file(bitnet_root.join("src/bitnet_tokenizer.cpp"));

    // Architecture-specific SIMD optimisations.
    if target_arch == "x86_64" {
        build
            .flag_if_supported("-mavx2")
            .flag_if_supported("-mfma")
            .flag_if_supported("-mavx512f");
    } else if target_arch == "aarch64" {
        build.flag_if_supported("-march=armv8-a+dotprod+i8mm");
    }

    build.compile("bitnet");

    println!("cargo:rustc-link-lib=static=bitnet");
    println!("cargo:rerun-if-changed=bitnet.cpp/");
}
