//! Build script for `pares-agens-bitnet-sys`.
//!
//! When the `native` Cargo feature is enabled this script compiles
//! the bitnet.cpp submodule (which includes llama.cpp) via CMake,
//! then compiles our shim (bitnet_shim.cpp) and links everything.

fn main() {
    if std::env::var("CARGO_FEATURE_NATIVE").is_err() {
        return;
    }

    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by Cargo");

    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root is two levels above crate manifest");

    let submodule = workspace_root.join("third_party").join("bitnet");

    if !submodule.join("CMakeLists.txt").exists() {
        println!(
            "cargo:warning=bitnet.cpp submodule not found at {}. \
             Run `git submodule update --init third_party/bitnet` to enable \
             native inference.",
            submodule.display()
        );
        return;
    }

    build_bitnet(&submodule, &manifest_dir);
}

fn build_bitnet(src_dir: &std::path::Path, manifest_dir: &str) {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set by Cargo");
    let build_dir = std::path::Path::new(&out_dir).join("bitnet-build");
    let target = std::env::var("TARGET").unwrap_or_default();
    let num_jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".to_owned());

    std::fs::create_dir_all(&build_dir).expect("could not create CMake build directory");

    // --- CMake configure (builds llama.cpp + bitnet kernel) ---
    let status = std::process::Command::new("cmake")
        .arg(src_dir)
        .arg("-B")
        .arg(&build_dir)
        .args([
            "-DCMAKE_BUILD_TYPE=Release",
            "-DBUILD_SHARED_LIBS=OFF",
            "-DLLAMA_BUILD_TESTS=OFF",
            "-DLLAMA_BUILD_EXAMPLES=OFF",
            "-DLLAMA_BUILD_SERVER=OFF",
        ])
        .status()
        .expect("cmake configure failed — is CMake ≥ 3.14 installed?");

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

    // --- Build our shim that wraps llama.cpp API into bitnet_* API ---
    let shim_dir = std::path::Path::new(manifest_dir).join("shim");
    let llama_include = src_dir.join("3rdparty").join("llama.cpp").join("include");
    let llama_common = src_dir.join("3rdparty").join("llama.cpp").join("common");
    let ggml_include = src_dir
        .join("3rdparty")
        .join("llama.cpp")
        .join("ggml")
        .join("include");

    cc::Build::new()
        .cpp(true)
        .file(shim_dir.join("bitnet_shim.cpp"))
        .include(&llama_include)
        .include(&llama_common)
        .include(&ggml_include)
        .include(src_dir.join("include"))
        .flag_if_supported("-std=c++17")
        .compile("bitnet_shim");

    // --- Link flags ---
    // Search for llama.cpp libraries in various possible locations
    let search_dirs = [
        build_dir.join("3rdparty").join("llama.cpp").join("src"),
        build_dir
            .join("3rdparty")
            .join("llama.cpp")
            .join("ggml")
            .join("src"),
        build_dir.join("3rdparty").join("llama.cpp").join("common"),
        build_dir.join("src"),
        build_dir.clone(),
    ];

    for dir in &search_dirs {
        if dir.exists() {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
    }

    println!("cargo:rustc-link-lib=static=bitnet_shim");
    println!("cargo:rustc-link-lib=static=llama");
    println!("cargo:rustc-link-lib=static=ggml");
    println!("cargo:rustc-link-lib=static=common");

    if target.contains("linux") {
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=dl");
        println!("cargo:rustc-link-lib=stdc++");
        println!("cargo:rustc-link-lib=gomp");
    }

    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("CMakeLists.txt").display()
    );
    println!("cargo:rerun-if-changed={}", shim_dir.display());
}
