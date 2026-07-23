fn main() {
    // Embed git commit hash at compile time.
    //
    // Resolution strategy (git CLI -> GIT_COMMIT_HASH env -> .git/HEAD ->
    // package-version fallback) lives once in the shared `pares-agens-buildinfo`
    // crate (ADR-0010: no duplicated operational logic). The fallback is the
    // package version, which in sandboxed builds (Nix, Docker) with no git
    // access IS the release version from the tag that triggered the build.
    let fallback = format!("v{}", env!("CARGO_PKG_VERSION"));
    let output = pares_agens_buildinfo::git_commit_hash(&fallback);

    println!("cargo:rustc-env=GIT_COMMIT_HASH={output}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");
}
