//! Embed the git commit (short hash + date, with a `+` marker when the tree is dirty) so the
//! running build is identifiable in the UI footer and the startup log. Falls back to "unknown"
//! when git is unavailable, so a source-only build still compiles.

use std::process::Command;

fn main() {
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    // Only tracked modifications count as "dirty" — untracked scratch files (test artifacts,
    // unrelated docs) must not flip the marker, or every working tree would look modified.
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let hash = if dirty { format!("{hash}+") } else { hash };
    let date = git(&["show", "-s", "--format=%cd", "--date=format:%Y-%m-%d", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=AIRPASTE_GIT_HASH={hash}");
    println!("cargo:rustc-env=AIRPASTE_GIT_DATE={date}");

    // Re-run when HEAD moves so the embedded commit stays fresh between builds. Also track this
    // script itself: emitting any rerun-if-changed otherwise disables the default "rerun on
    // package change", so edits here would not take effect until HEAD next moved.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
