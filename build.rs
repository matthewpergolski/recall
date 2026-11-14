use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

include!("git_hook_env.rs");

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
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("git_hook_env.rs").display()
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

    let mut commit = git_output(&manifest_dir, &["rev-parse", "HEAD"])
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let is_dirty = git_output(
        &manifest_dir,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .is_some_and(|output| !output.is_empty());
    if is_dirty {
        commit.push_str("-dirty");
    }

    println!("cargo:rustc-env=RECALL_BUILD_COMMIT={commit}");
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    scrub_git_command(&mut command);
    let output = command
        .arg("-C")
        .arg(manifest_dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
