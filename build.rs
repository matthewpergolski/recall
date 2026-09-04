use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by Cargo"),
    );

    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(".git/HEAD").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(".git/refs/heads/main").display()
    );
    for path in [
        "src",
        "Cargo.toml",
        "Cargo.lock",
        "capture-helper/Package.swift",
        "capture-helper/Sources",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(path).display()
        );
    }

    let mut commit = Command::new("git")
        .arg("-C")
        .arg(&manifest_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let is_dirty = Command::new("git")
        .arg("-C")
        .arg(&manifest_dir)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty());
    if is_dirty {
        commit.push_str("-dirty");
    }

    println!("cargo:rustc-env=RECALL_BUILD_COMMIT={commit}");
}
