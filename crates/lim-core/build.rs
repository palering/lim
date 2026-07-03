//! Build script: embed git-derived version metadata at compile time.
//!
//! Sets `LIM_BUILD_VERSION` env var so that `lim_core::LIM_VERSION` can
//! prefer the git description (e.g. `v0.1.0-3-gabcdef-dirty`) over the
//! static `CARGO_PKG_VERSION`. Falls back to `CARGO_PKG_VERSION` when:
//!   - `git` is not on PATH,
//!   - the build directory is not inside a git work tree,
//!   - git returns an empty string (shouldn't happen with `--always`).
//!
//! `LIM_VERSION` is consumed by:
//!   - `SessionHeader.lim_version` (debug/audit field in session JSONL)
//!   - clap `#[command(version = ...)]` in the CLI (`lim --version`)
//
// Rerun triggers: re-run this script when HEAD or any tag moves so the
// embedded version stays in sync with the checkout. `.git/HEAD` covers
// branch switches; `.git/refs/tags` covers tag creation/deletion. For
// packed-refs setups `.git/packed-refs` is also covered.
use std::process::Command;

fn main() {
    let version = git_describe_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=LIM_BUILD_VERSION={version}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");
    println!("cargo:rerun-if-changed=.git/packed-refs");
}

/// Run `git describe --tags --always --dirty=-dirty` from the crate
/// manifest dir. git walks up to find the repo root automatically.
fn git_describe_version() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty=-dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8(output.stdout).ok()?;
    let v = v.trim();
    if v.is_empty() { None } else { Some(v.to_string()) }
}
