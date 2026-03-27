//! Build script for `pares-agens-bitnet-sys`.
//!
//! When the `native` Cargo feature is enabled this script attempts to compile
//! the `third_party/bitnet` submodule with CMake and link the resulting static
//! library.  If the submodule is absent (e.g. in a shallow CI clone that only
//! runs `cargo check`) a warning is emitted and the script exits cleanly —
//! `cargo check --features native` will still succeed because linking is not
//! performed during a check.

fn main() {
    // Nothing to do unless the `native` feature is requested.
    if std::env::var("CARGO_FEATURE_NATIVE").is_err() {
        return;
    }

    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by Cargo");

    // The bitnet.cpp source lives two levels up from this crate's manifest
    // (workspace-root/third_party/bitnet).
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("workspace root is two levels above crate manifest");

    let submodule = workspace_root.join("third_party").join("bitnet");

    if !submodule.join("CMakeLists.txt").exists() {
        // The submodule has not been initialised.  Emit a warning so the
        // developer knows what is missing, but do not fail — `cargo check`
        // does not require the native library to be present.
        println!(
            "cargo:warning=bitnet.cpp submodule not found at {}. \
             Run `git submodule update --init third_party/bitnet` to enable \
             native inference. Skipping native build.",
            submodule.display()
        );
        return;
    }

    build_bitnet(&submodule);
}

/// Compile bitnet.cpp with CMake and register linker flags.
fn build_bitnet(src_dir: &std::path::Path) {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set by Cargo");
    let build_dir = std::path::Path::new(&out_dir).join("bitnet-build");

    std::fs::create_dir_all(&build_dir).expect("could not create CMake build directory");

    let target = std::env::var("TARGET").unwrap_or_default();
    let num_jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".to_owned());

    // --- CMake configure ---
    let status = std::process::Command::new("cmake")
        .arg(src_dir)
        .arg("-B")
        .arg(&build_dir)
        .args([
            "-DCMAKE_BUILD_TYPE=Release",
            // Build a static library so we can link it into the Rust binary
            // without shipping a separate shared object.
            "-DBUILD_SHARED_LIBS=OFF",
            // Disable optional components that add heavy system dependencies.
            "-DBITNET_BUILD_TESTS=OFF",
            "-DBITNET_BUILD_EXAMPLES=OFF",
        ])
        .status()
        .expect("cmake configure failed — is CMake ≥ 3.21 installed?");

    assert!(status.success(), "CMake configure step failed");

    // --- CMake build ---
    let status = std::process::Command::new("cmake")
        .args(["--build", "."])
        .arg("--config")
        .arg("Release")
        .arg("-j")
        .arg(&num_jobs)
        .current_dir(&build_dir)
        .status()
        .expect("cmake build failed");

    assert!(status.success(), "CMake build step failed");

    // --- Link flags ---
    println!("cargo:rustc-link-search=native={}", build_dir.display());
    println!("cargo:rustc-link-lib=static=bitnet");

    // On Linux, bitnet.cpp typically requires pthread and math libraries.
    if target.contains("linux") {
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=dl");
        println!("cargo:rustc-link-lib=stdc++");
    }

    // Invalidate the build script if the submodule source changes.
    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("CMakeLists.txt").display()
    );
    println!("cargo:rerun-if-changed={}", src_dir.join("src").display());
}
