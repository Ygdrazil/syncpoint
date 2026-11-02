use std::path::Path;
use std::process::Command;

fn main() {
    emit_version();
    emit_changelog();
    tauri_build::build()
}

/// Bake the commit log into the binary for `whats_new`, which is what reads it.
/// No git (a source tarball) leaves it empty, which just means no popup.
fn emit_changelog() {
    let log = Command::new("git")
        .args(["log", "--decorate-refs=refs/tags/", "--format=%D%x09%s"])
        .current_dir("..")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_default();
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts");
    std::fs::write(Path::new(&out_dir).join("changelog.txt"), log)
        .expect("OUT_DIR is writable");
}

/// Bake the version derived from git into the binary as `SYNCPOINT_VERSION`.
fn emit_version() {
    let version = git_describe().unwrap_or_else(|| {
        // No git (a source tarball, or a checkout with no tags yet) — fall back
        // to the version in Cargo.toml.
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into())
    });
    println!("cargo:rustc-env=SYNCPOINT_VERSION={version}");

    // Without these, cargo caches the build script's output and the baked-in
    // version and log go stale the moment a new tag or commit lands.
    for path in [
        "../.git/HEAD",
        "../.git/logs/HEAD", // moves on every commit, which HEAD itself does not
        "../.git/refs/tags",
        "../.git/packed-refs", // where tags end up once git packs them
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_describe() -> Option<String> {
    // `--tags` matches lightweight tags too, `--always` degrades to a bare
    // commit hash when the repo has no tags at all. Deliberately no `--dirty`:
    // a build script only re-runs when a tracked file changes, so a
    // dirty marker would be stale as often as it was right.
    let output = Command::new("git")
        .args(["describe", "--tags", "--always"])
        .current_dir("..")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let described = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if described.is_empty() {
        return None;
    }
    // Tags are written `v0.1.0`; the displayed version drops the prefix.
    Some(
        described
            .strip_prefix('v')
            .unwrap_or(&described)
            .to_string(),
    )
}
