//! Puts memory.x on the linker's search path (z7link.x, from zynq7000-rt,
//! includes it by name) and records the commit being built for the boot
//! banner, as `ARGUS_FW_REV`.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let Some(out) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        println!("cargo::error=OUT_DIR is not set");
        return;
    };
    if fs::write(out.join("memory.x"), include_bytes!("memory.x")).is_err() {
        println!("cargo::error=could not write memory.x to OUT_DIR");
        return;
    }
    println!("cargo::rustc-link-search={}", out.display());
    println!("cargo::rerun-if-changed=memory.x");

    println!("cargo::rustc-env=ARGUS_FW_REV={}", revision());

    // The revision must describe the tree as it is now. A commit moves a
    // branch ref, an edit to any tracked file (in this crate or not, C
    // included) makes the tree dirty, and cargo can watch neither cheaply. So
    // this script runs on every build, by naming a file that never exists:
    // cargo treats a missing rerun-if-changed path as always stale. It costs
    // two git commands and a relink of this crate.
    println!("cargo::rerun-if-changed=.always-rerun-for-the-revision");
}

/// The first 12 hex digits of HEAD, with `-dirty` appended when tracked files
/// have uncommitted changes (staged or not). `unknown` when git is missing or
/// this is not a checkout, so a build outside the repository still works.
fn revision() -> String {
    // Cargo runs build scripts in the package's directory, which is inside
    // the repository.
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    // Not `--short=12`: that grows past 12 digits when the prefix is
    // ambiguous, and the banner's format is exactly 12.
    let Some(rev) = git(&["rev-parse", "HEAD"]).and_then(|full| full.get(..12).map(str::to_owned))
    else {
        return "unknown".to_owned();
    };
    // Untracked files are left out: they cannot reach the image without a
    // change to a tracked file (a `mod` line, a Cargo.toml entry). Clean only
    // when git says so: if `status` fails, the tree is reported dirty rather
    // than vouched for.
    let clean =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| s.is_empty());
    if clean { rev } else { format!("{rev}-dirty") }
}
